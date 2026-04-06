//! Built-in MCP server for Nexterm.
//!
//! Exposes terminal state, command blocks, and execution to AI tools
//! via the Model Context Protocol over streamable HTTP.

use std::sync::Arc;

use nex_block::{Block, BlockStore};
use nex_common::{BlockId, PaneId};
use parking_lot::Mutex;
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::router::tool::ToolRouter,
    model::*,
    schemars, tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DEFAULT_RECENT_COUNT: u32 = 10;
const DEFAULT_SEARCH_MAX_RESULTS: u32 = 20;
const EXECUTE_CHANNEL_TIMEOUT_SECS: u64 = 30;
const MCP_SERVER_NAME: &str = "nexterm-mcp";
const MCP_SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const MCP_INSTRUCTIONS: &str = "\
You are connected to Nexterm, an AI-first terminal. Workflow: \
1) Use get_context for working directory and terminal state — don't run pwd/git status. \
2) Use get_recent_blocks to see what commands already ran — don't re-run them. \
3) To see more detail on a command's output, call get_block with the block ID and tier='classified' (key lines) or tier='raw' (full output). \
4) Use execute to run shell commands — it returns stdout/stderr directly. \
5) Use search_blocks to find specific output across all command history.";

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

/// Snapshot of terminal state shared between main thread and MCP server.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TerminalStateSnapshot {
    pub panes: Vec<PaneInfo>,
    pub active_pane_id: Option<String>,
}

/// Information about a single terminal pane.
#[derive(Debug, Clone, Serialize)]
pub struct PaneInfo {
    pub id: String,
    pub tab_title: String,
    pub cwd: String,
    pub term_rows: u16,
    pub term_cols: u16,
    pub last_exit_code: Option<i32>,
}

/// A command to execute, sent from MCP server to the main terminal thread.
pub struct ExecuteCommand {
    pub command: String,
    pub pane_id: Option<PaneId>,
    pub response_tx: tokio::sync::oneshot::Sender<String>,
}

/// Sender half for the execute command channel (std::sync::mpsc).
pub type ExecuteSender = std::sync::mpsc::Sender<ExecuteCommand>;

// ---------------------------------------------------------------------------
// Tool parameter structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetRecentBlocksParams {
    /// Number of recent blocks to retrieve (default: 10).
    #[serde(default)]
    count: Option<u32>,
    /// Restrict to a specific pane by its UUID. If omitted, uses the active pane.
    #[serde(default)]
    pane_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetBlockParams {
    /// The UUID of the block to retrieve.
    block_id: String,
    /// Detail tier: "summary" (default), "classified", or "raw".
    #[serde(default)]
    tier: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchBlocksParams {
    /// Regex pattern to search across all block outputs.
    pattern: String,
    /// Maximum number of results to return (default: 20).
    #[serde(default)]
    max_results: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetContextParams {
    /// Pane UUID. If omitted, returns context for the active pane.
    #[serde(default)]
    pane_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ExecuteParams {
    /// The shell command to execute.
    command: String,
    /// Target pane UUID. If omitted, uses the active pane.
    #[serde(default)]
    pane_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Block serialization helpers
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct BlockSummary {
    id: String,
    pane_id: String,
    command: String,
    exit_code: Option<i32>,
    classification: String,
    summary: String,
    timestamp: String,
    token_estimate: usize,
}

#[derive(Serialize)]
struct BlockClassified {
    id: String,
    pane_id: String,
    command: String,
    exit_code: Option<i32>,
    classification: String,
    summary: String,
    key_lines: Vec<String>,
    compression_ratio: f32,
    timestamp: String,
    token_estimate: usize,
}

#[derive(Serialize)]
struct BlockRaw {
    id: String,
    pane_id: String,
    command: String,
    exit_code: Option<i32>,
    classification: String,
    raw_output: String,
    timestamp: String,
    token_estimate: usize,
}

fn block_to_summary(block: &Block) -> BlockSummary {
    BlockSummary {
        id: block.id.to_string(),
        pane_id: block.pane_id.to_string(),
        command: block.command.clone(),
        exit_code: block.exit_code,
        classification: format!("{:?}", block.classification),
        summary: block.output.compressed.summary.clone(),
        timestamp: block.timestamp.to_rfc3339(),
        token_estimate: block.token_estimate,
    }
}

fn block_to_classified(block: &Block) -> BlockClassified {
    BlockClassified {
        id: block.id.to_string(),
        pane_id: block.pane_id.to_string(),
        command: block.command.clone(),
        exit_code: block.exit_code,
        classification: format!("{:?}", block.classification),
        summary: block.output.compressed.summary.clone(),
        key_lines: block.output.compressed.key_lines.clone(),
        compression_ratio: block.output.compressed.compression_ratio,
        timestamp: block.timestamp.to_rfc3339(),
        token_estimate: block.token_estimate,
    }
}

fn block_to_raw(block: &Block) -> BlockRaw {
    BlockRaw {
        id: block.id.to_string(),
        pane_id: block.pane_id.to_string(),
        command: block.command.clone(),
        exit_code: block.exit_code,
        classification: format!("{:?}", block.classification),
        raw_output: block.output.stripped_text.clone(),
        timestamp: block.timestamp.to_rfc3339(),
        token_estimate: block.token_estimate,
    }
}

fn block_by_tier(block: &Block, tier: &str) -> String {
    match tier {
        "classified" => serde_json::to_string_pretty(&block_to_classified(block))
            .unwrap_or_default(),
        "raw" => serde_json::to_string_pretty(&block_to_raw(block))
            .unwrap_or_default(),
        _ => serde_json::to_string_pretty(&block_to_summary(block))
            .unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// MCP server
// ---------------------------------------------------------------------------

/// The Nexterm MCP server. Exposes terminal blocks, context, and execution.
#[derive(Clone)]
pub struct NextermMcpServer {
    block_store: Arc<BlockStore>,
    terminal_state: Arc<Mutex<TerminalStateSnapshot>>,
    execute_tx: ExecuteSender,
    tool_router: ToolRouter<NextermMcpServer>,
}

impl NextermMcpServer {
    pub fn new(
        block_store: Arc<BlockStore>,
        terminal_state: Arc<Mutex<TerminalStateSnapshot>>,
        execute_tx: ExecuteSender,
    ) -> Self {
        Self {
            block_store,
            terminal_state,
            execute_tx,
            tool_router: Self::tool_router(),
        }
    }

    /// Update the terminal state snapshot (called from the main thread).
    pub fn update_state(&self, snapshot: TerminalStateSnapshot) {
        *self.terminal_state.lock() = snapshot;
    }

    fn resolve_pane_id(&self, pane_id_str: Option<&str>) -> Option<PaneId> {
        if let Some(s) = pane_id_str {
            Uuid::parse_str(s).ok().map(PaneId)
        } else {
            let state = self.terminal_state.lock();
            state
                .active_pane_id
                .as_ref()
                .and_then(|s| Uuid::parse_str(s).ok())
                .map(PaneId)
        }
    }
}

#[tool_router]
impl NextermMcpServer {
    /// Retrieve recent command blocks from the terminal history.
    /// Use this before re-running commands to avoid duplication.
    #[tool(
        name = "get_recent_blocks",
        description = "Get recent command blocks from the terminal. Returns compressed summaries including block IDs. To see full output of a specific command, use get_block with the block's ID and tier='classified' for key lines or tier='raw' for complete output."
    )]
    fn get_recent_blocks(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<GetRecentBlocksParams>,
    ) -> Result<CallToolResult, McpError> {
        let count = params.count.unwrap_or(DEFAULT_RECENT_COUNT) as usize;

        let blocks = if let Some(pane_id) = self.resolve_pane_id(params.pane_id.as_deref()) {
            self.block_store.get_recent(&pane_id, count)
        } else {
            // No pane resolved — return empty
            vec![]
        };

        let summaries: Vec<BlockSummary> = blocks.iter().map(|b| block_to_summary(b)).collect();
        let json = serde_json::to_string_pretty(&summaries).unwrap_or_default();
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Retrieve a specific block by ID with configurable detail level.
    #[tool(
        name = "get_block",
        description = "Get a specific command block by its ID (from get_recent_blocks or search_blocks). Tiers: 'summary' (one line), 'classified' (summary + key lines like errors/failures — default), 'raw' (full uncompressed output). Start with 'classified' and only use 'raw' if you need exact output."
    )]
    fn get_block(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<GetBlockParams>,
    ) -> Result<CallToolResult, McpError> {
        let block_uuid = Uuid::parse_str(&params.block_id).map_err(|_| {
            McpError::invalid_params("Invalid block_id UUID", None)
        })?;
        let block_id = BlockId(block_uuid);

        match self.block_store.get(&block_id) {
            Some(block) => {
                let tier = params.tier.as_deref().unwrap_or("summary");
                let json = block_by_tier(&block, tier);
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            None => Ok(CallToolResult::error(vec![Content::text(
                format!("Block {} not found", params.block_id),
            )])),
        }
    }

    /// Search across all block outputs using a regex pattern.
    #[tool(
        name = "search_blocks",
        description = "Search all command block outputs using a regex pattern. Returns matching blocks as summaries. Useful for finding previous commands or output containing specific text."
    )]
    fn search_blocks(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<SearchBlocksParams>,
    ) -> Result<CallToolResult, McpError> {
        let max = params.max_results.unwrap_or(DEFAULT_SEARCH_MAX_RESULTS) as usize;
        let blocks = self.block_store.search(&params.pattern, max);
        let summaries: Vec<BlockSummary> = blocks.iter().map(|b| block_to_summary(b)).collect();
        let json = serde_json::to_string_pretty(&summaries).unwrap_or_default();
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Get terminal context for a pane (cwd, size, last exit code).
    /// Use this instead of running `pwd` or `git status`.
    #[tool(
        name = "get_context",
        description = "Get terminal pane context including working directory, terminal size, and last exit code. Use this instead of running pwd or git status — it costs zero tokens and returns instantly."
    )]
    fn get_context(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<GetContextParams>,
    ) -> Result<CallToolResult, McpError> {
        let state = self.terminal_state.lock();

        let pane = if let Some(ref id_str) = params.pane_id {
            state.panes.iter().find(|p| p.id == *id_str)
        } else if let Some(ref active) = state.active_pane_id {
            state.panes.iter().find(|p| p.id == *active)
        } else {
            state.panes.first()
        };

        match pane {
            Some(info) => {
                let json = serde_json::to_string_pretty(info).unwrap_or_default();
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            None => Ok(CallToolResult::error(vec![Content::text(
                "No matching pane found",
            )])),
        }
    }

    /// List all terminal panes.
    #[tool(
        name = "list_panes",
        description = "List all open terminal panes with their IDs, titles, working directories, and sizes."
    )]
    fn list_panes(&self) -> Result<CallToolResult, McpError> {
        let state = self.terminal_state.lock();
        let json = serde_json::to_string_pretty(&state.panes).unwrap_or_default();
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Execute a shell command in a terminal pane.
    #[tool(
        name = "execute",
        description = "Execute a shell command as a subprocess and return stdout/stderr with exit code. Runs in the terminal's working directory. Use this for running builds, tests, and other commands."
    )]
    async fn execute(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ExecuteParams>,
    ) -> Result<CallToolResult, McpError> {
        tracing::info!(command = %params.command, "MCP execute");

        let pane_id = params.pane_id.and_then(|s| Uuid::parse_str(&s).ok().map(PaneId));

        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let cmd = ExecuteCommand {
            command: params.command.clone(),
            pane_id,
            response_tx,
        };

        self.execute_tx.send(cmd).map_err(|_| {
            tracing::error!("Execute channel closed — bridge thread may have crashed");
            McpError::internal_error("Terminal execution channel closed", None)
        })?;

        tracing::debug!("MCP execute: waiting for response");

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(EXECUTE_CHANNEL_TIMEOUT_SECS),
            response_rx,
        )
        .await
        .map_err(|_| {
            tracing::error!("MCP execute: timed out after {EXECUTE_CHANNEL_TIMEOUT_SECS}s waiting for response");
            McpError::internal_error("Command execution timed out", None)
        })?
        .map_err(|_| {
            tracing::error!("MCP execute: response channel dropped — main thread handler may have failed");
            McpError::internal_error("Response channel dropped", None)
        })?;

        tracing::debug!(bytes = result.len(), "MCP execute: got response");
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }
}

#[tool_handler]
impl ServerHandler for NextermMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(Implementation::new(MCP_SERVER_NAME, MCP_SERVER_VERSION))
        .with_instructions(MCP_INSTRUCTIONS.to_string())
    }
}

// ---------------------------------------------------------------------------
// HTTP transport
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use nex_common::{BlockId, CompressedOutput, OutputClass, PaneId};
    use std::path::PathBuf;
    use std::time::Duration;

    fn test_block() -> Block {
        Block {
            id: BlockId(Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()),
            pane_id: PaneId(Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()),
            sequence: 0,
            prompt: String::new(),
            command: "cargo test".to_string(),
            output: nex_block::BlockOutput {
                stripped_text: "running 5 tests\ntest result: ok".to_string(),
                compressed: CompressedOutput {
                    summary: "5 tests passed".to_string(),
                    key_lines: vec!["test result: ok".to_string()],
                    compression_ratio: 0.5,
                },
            },
            exit_code: Some(0),
            cwd: PathBuf::from("/tmp"),
            duration: Some(Duration::from_secs(2)),
            timestamp: Utc::now(),
            classification: OutputClass::TestResult,
            token_estimate: 42,
        }
    }

    #[test]
    fn block_to_summary_serializes_correctly() {
        let block = test_block();
        let summary = block_to_summary(&block);
        assert_eq!(summary.command, "cargo test");
        assert_eq!(summary.exit_code, Some(0));
        assert_eq!(summary.summary, "5 tests passed");
        assert_eq!(summary.classification, "TestResult");
        assert_eq!(summary.token_estimate, 42);
        assert_eq!(summary.id, "11111111-1111-1111-1111-111111111111");
    }

    #[test]
    fn block_to_classified_includes_key_lines() {
        let block = test_block();
        let classified = block_to_classified(&block);
        assert_eq!(classified.key_lines, vec!["test result: ok"]);
        assert!((classified.compression_ratio - 0.5).abs() < f32::EPSILON);
        assert_eq!(classified.command, "cargo test");
    }

    #[test]
    fn block_to_raw_includes_full_output() {
        let block = test_block();
        let raw = block_to_raw(&block);
        assert_eq!(raw.raw_output, "running 5 tests\ntest result: ok");
        assert_eq!(raw.command, "cargo test");
    }

    #[test]
    fn block_by_tier_routes_correctly() {
        let block = test_block();

        let summary_json = block_by_tier(&block, "summary");
        assert!(summary_json.contains("\"summary\""));
        assert!(!summary_json.contains("raw_output"));
        assert!(!summary_json.contains("key_lines"));

        let classified_json = block_by_tier(&block, "classified");
        assert!(classified_json.contains("key_lines"));
        assert!(!classified_json.contains("raw_output"));

        let raw_json = block_by_tier(&block, "raw");
        assert!(raw_json.contains("raw_output"));
        assert!(!raw_json.contains("key_lines"));

        // Unknown tier falls back to summary
        let fallback_json = block_by_tier(&block, "unknown_tier");
        assert_eq!(fallback_json, summary_json);
    }
}

impl NextermMcpServer {
    /// Start the MCP server on the given HTTP port using streamable HTTP transport.
    pub async fn start_http(self, port: u16) -> anyhow::Result<()> {
        use rmcp::transport::streamable_http_server::{
            session::local::LocalSessionManager,
            StreamableHttpServerConfig, StreamableHttpService,
        };

        let config = StreamableHttpServerConfig::default();
        let session_manager = Arc::new(LocalSessionManager::default());

        let service = StreamableHttpService::new(
            move || Ok(self.clone()),
            session_manager,
            config,
        );

        let app = axum::Router::new().nest_service("/mcp", service);

        let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await?;
        tracing::info!(port, "Nexterm MCP server listening");
        axum::serve(listener, app).await?;

        Ok(())
    }
}
