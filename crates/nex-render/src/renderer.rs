//! Core renderer using wgpu for GPU rendering and glyphon for text.
//! Supports multi-pane rendering with per-pane text buffers.

use std::collections::HashMap;

use glyphon::{
    Attrs, Buffer, Color, Family, FontSystem, Metrics, Resolution, Shaping, Style, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};
use wgpu::{
    util::{BufferInitDescriptor, DeviceExt},
    BufferUsages, CommandEncoderDescriptor, Device, DeviceDescriptor, Instance, InstanceDescriptor,
    LoadOp, MultisampleState, Operations, PipelineCompilationOptions, PowerPreference, Queue,
    RenderPassColorAttachment, RenderPassDescriptor, RenderPipeline, RequestAdapterOptions,
    StoreOp, Surface, SurfaceConfiguration, TextureUsages, TextureViewDescriptor,
};

use nex_common::{PaneId, CELL_WIDTH_RATIO, LINE_HEIGHT_RATIO, PADDING};

pub const DEFAULT_FONT_SIZE: f32 = 14.0;
pub const MIN_FONT_SIZE: f32 = 8.0;
pub const MAX_FONT_SIZE: f32 = 40.0;
pub const FONT_SIZE_STEP: f32 = 1.0;

const BG_R: u8 = 13;
const BG_G: u8 = 13;
const BG_B: u8 = 18;

// Selection highlight
const SELECTION_COLOR: [u8; 3] = [80, 120, 200];

// Focus border
const FOCUS_BORDER_COLOR: [u8; 3] = [80, 140, 220];
const FOCUS_BORDER_THICKNESS: f32 = 2.0;

// Pane divider
const DIVIDER_COLOR: [u8; 3] = [90, 90, 105];

// Tab bar layout (logical pixels, scaled by DPI at runtime)
const TAB_GAP_LOGICAL: f32 = 4.0;
const MAX_TAB_WIDTH_LOGICAL: f32 = 180.0;
const MIN_TAB_WIDTH_LOGICAL: f32 = 60.0;
const TAB_TEXT_INSET_LOGICAL: f32 = 6.0;
const TAB_TEXT_Y_PAD_LOGICAL: f32 = 2.0;
const TAB_FONT_SIZE_LOGICAL: f32 = 12.0;
const TAB_LINE_HEIGHT_LOGICAL: f32 = 16.0;
const TAB_CHAR_WIDTH_RATIO: f32 = 0.6;

// Tab bar colors
const TAB_BAR_BG: [u8; 3] = [25, 25, 30];
const TAB_ACTIVE_BG: [u8; 3] = [50, 50, 58];
const TAB_INACTIVE_BG: [u8; 3] = [35, 35, 40];
const TAB_ACTIVE_TEXT_COLOR: [u8; 3] = [220, 220, 225];
const TAB_INACTIVE_TEXT_COLOR: [u8; 3] = [140, 140, 150];

// Default text & cursor
const DEFAULT_TEXT_COLOR: [u8; 3] = [204, 204, 204];
const CURSOR_COLOR: [u8; 4] = [200, 200, 200, 180];

// Secondary text (tab/overlay defaults)
const SECONDARY_TEXT_COLOR: [u8; 3] = [180, 180, 190];

// Overlay (tab switcher)
const OVERLAY_PANEL_WIDTH_LOGICAL: f32 = 400.0;
const OVERLAY_ENTRY_HEIGHT_LOGICAL: f32 = 32.0;
const OVERLAY_PADDING_LOGICAL: f32 = 12.0;
const OVERLAY_MAX_HEIGHT_RATIO: f32 = 0.7;
const OVERLAY_FONT_SIZE_LOGICAL: f32 = 14.0;
const OVERLAY_DIM_ALPHA: f32 = 0.75;
const OVERLAY_ENTRY_INSET_LOGICAL: f32 = 4.0;
const OVERLAY_ENTRY_GAP_LOGICAL: f32 = 2.0;
const OVERLAY_PANEL_BG: [u8; 3] = [50, 50, 62];
const OVERLAY_SELECTED_BG: [u8; 3] = [55, 95, 175];
const OVERLAY_NORMAL_BG: [u8; 3] = [58, 58, 68];
const OVERLAY_SELECTED_TEXT_COLOR: [u8; 3] = [255, 255, 255];
const OVERLAY_NORMAL_TEXT_COLOR: [u8; 3] = [170, 170, 180];

// Session browser overlay
const SESSION_PANEL_WIDTH_LOGICAL: f32 = 520.0;
const SESSION_ENTRY_HEIGHT_LOGICAL: f32 = 48.0;
const SESSION_SEARCH_HEIGHT_LOGICAL: f32 = 48.0; // two text lines: tab bar + search input
const SESSION_PADDING_LOGICAL: f32 = 12.0;
const SESSION_MAX_HEIGHT_RATIO: f32 = 0.75;
const SESSION_FONT_SIZE_LOGICAL: f32 = 14.0;
const SESSION_ENTRY_INSET_LOGICAL: f32 = 4.0;
const SESSION_ENTRY_GAP_LOGICAL: f32 = 2.0;
const SESSION_SEARCH_BAR_BG: [u8; 3] = [40, 40, 50];
const SESSION_SEARCH_TEXT_COLOR: [u8; 3] = [200, 200, 210];
const SESSION_SEARCH_PLACEHOLDER_COLOR: [u8; 3] = [100, 100, 115];
const SESSION_ACTIVE_INDICATOR_COLOR: [u8; 3] = [80, 200, 120];
const SESSION_SECONDARY_TEXT_COLOR: [u8; 3] = [120, 120, 135];
const SESSION_CHAR_WIDTH_RATIO: f32 = 0.6;
const SESSION_HEADER_SELECTED_BG: [u8; 3] = [65, 65, 80];

fn glyphon_rgb(c: [u8; 3]) -> Color {
    Color::rgb(c[0], c[1], c[2])
}

pub struct RenderSpan {
    pub text: String,
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub bold: bool,
    pub italic: bool,
}

pub struct BgCell {
    pub row: usize,
    pub col: usize,
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

pub struct SelectionCell {
    pub row: usize,
    pub col: usize,
}

/// Content for one pane, ready to render.
pub struct PaneContent {
    pub pane_id: PaneId,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub spans: Vec<RenderSpan>,
    pub bg_cells: Vec<BgCell>,
    pub selection_cells: Vec<SelectionCell>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub is_focused: bool,
    pub bell_active: bool,
}

/// An overlay panel (e.g., tab switcher).
pub struct OverlayContent {
    pub entries: Vec<OverlayEntry>,
    pub selected_index: usize,
}

pub struct OverlayEntry {
    pub label: String,
    pub is_active: bool,
}

/// Session browser overlay panel.
pub struct SessionOverlay {
    pub entries: Vec<SessionOverlayEntry>,
    pub selected_index: usize,
    pub filter: String,
}

pub enum SessionEntryKind {
    ProjectHeader {
        name: String,
        session_count: usize,
        expanded: bool,
    },
    Session {
        project_name: String,
        summary: String,
        time_ago: String,
        message_count: usize,
        model: String,
        is_active: bool,
        depth: usize,
        tree_prefix: String,
    },
}

pub struct SessionOverlayEntry {
    pub kind: SessionEntryKind,
    pub is_selected: bool,
}

/// A divider line between split panes.
pub struct DividerLine {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// A tab in the tab bar.
pub struct TabInfo {
    pub title: String,
    pub is_active: bool,
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct RectVertex {
    position: [f32; 2],
    color: [f32; 4],
}

pub struct Renderer {
    device: Device,
    queue: Queue,
    surface: Surface<'static>,
    surface_config: SurfaceConfiguration,
    font_system: FontSystem,
    swash_cache: SwashCache,
    text_atlas: TextAtlas,
    text_renderer: TextRenderer,
    viewport: Viewport,
    rect_pipeline: RenderPipeline,
    font_size: f32,
    scale_factor: f32,
    // Per-pane text buffers (reused across frames)
    pane_buffers: HashMap<PaneId, Buffer>,
    cursor_buffer: Buffer,
    tab_buffers: Vec<Buffer>,
    overlay_buffer: Buffer,
    session_overlay_buffer: Buffer,
}

impl Renderer {
    pub async fn new(
        window: std::sync::Arc<winit::window::Window>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let size = window.inner_size();
        let scale_factor = window.scale_factor() as f32;

        let instance = Instance::new(&InstanceDescriptor::default());
        let surface = instance.create_surface(window)?;

        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await?;

        let (device, queue) = adapter
            .request_device(&DeviceDescriptor::default())
            .await?;

        let surface_caps = surface.get_capabilities(&adapter);
        let format = surface_caps
            .formats
            .iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(surface_caps.formats[0]);

        let surface_config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        // Rect pipeline for cell backgrounds and dividers
        let rect_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rect_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("rect.wgsl").into()),
        });

        let rect_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rect_pipeline"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &rect_shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<RectVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 8,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                    ],
                }],
                compilation_options: PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &rect_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = glyphon::Cache::new(&device);
        let mut text_atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut text_atlas, &device, MultisampleState::default(), None);
        let viewport = Viewport::new(&device, &cache);

        let font_size = DEFAULT_FONT_SIZE;
        let physical = font_size * scale_factor;
        let line_height = physical * LINE_HEIGHT_RATIO;
        let metrics = Metrics::new(physical, line_height);

        let mut cursor_buffer = Buffer::new(&mut font_system, metrics);
        cursor_buffer.set_size(&mut font_system, Some(physical), Some(line_height));

        let overlay_buffer = Buffer::new(&mut font_system, Metrics::new(
            OVERLAY_FONT_SIZE_LOGICAL * scale_factor, OVERLAY_ENTRY_HEIGHT_LOGICAL * scale_factor,
        ));

        let session_overlay_buffer = Buffer::new(&mut font_system, Metrics::new(
            SESSION_FONT_SIZE_LOGICAL * scale_factor, SESSION_ENTRY_HEIGHT_LOGICAL * scale_factor,
        ));

        Ok(Self {
            device,
            queue,
            surface,
            surface_config,
            font_system,
            swash_cache,
            text_atlas,
            text_renderer,
            viewport,
            rect_pipeline,
            font_size,
            scale_factor,
            pane_buffers: HashMap::new(),
            cursor_buffer,
            tab_buffers: Vec::new(),
            overlay_buffer,
            session_overlay_buffer,
        })
    }

    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    pub fn surface_size(&self) -> (u32, u32) {
        (self.surface_config.width, self.surface_config.height)
    }

    fn physical_font_size(&self) -> f32 {
        self.font_size * self.scale_factor
    }

    fn line_height(&self) -> f32 {
        self.physical_font_size() * LINE_HEIGHT_RATIO
    }

    fn cell_width(&self) -> f32 {
        self.physical_font_size() * CELL_WIDTH_RATIO
    }

    pub fn set_font_size(&mut self, new_size: f32) -> bool {
        let new_size = new_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        if (new_size - self.font_size).abs() < 0.1 {
            return false;
        }
        self.font_size = new_size;
        let physical = self.physical_font_size();
        let line_height = physical * LINE_HEIGHT_RATIO;
        let metrics = Metrics::new(physical, line_height);

        for buf in self.pane_buffers.values_mut() {
            buf.set_metrics(&mut self.font_system, metrics);
        }
        self.cursor_buffer.set_metrics(&mut self.font_system, metrics);
        self.cursor_buffer
            .set_size(&mut self.font_system, Some(physical), Some(line_height));
        true
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.surface_config.width = width;
            self.surface_config.height = height;
            self.surface.configure(&self.device, &self.surface_config);
        }
    }

    pub fn terminal_size(&self) -> (u16, u16) {
        let cols = ((self.surface_config.width as f32 - PADDING * 2.0) / self.cell_width())
            .floor()
            .max(1.0) as u16;
        let rows = ((self.surface_config.height as f32 - PADDING * 2.0) / self.line_height())
            .floor()
            .max(1.0) as u16;
        (rows, cols)
    }

    fn pixel_to_ndc(&self, x: f32, y: f32) -> [f32; 2] {
        let w = self.surface_config.width as f32;
        let h = self.surface_config.height as f32;
        [x / w * 2.0 - 1.0, 1.0 - y / h * 2.0]
    }

    fn srgb_to_linear(c: u8) -> f32 {
        let c = c as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }

    fn color3(rgb: [u8; 3]) -> [f32; 4] {
        [Self::srgb_to_linear(rgb[0]), Self::srgb_to_linear(rgb[1]), Self::srgb_to_linear(rgb[2]), 1.0]
    }

    fn build_rect_vertices(&self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) -> [RectVertex; 6] {
        let tl = self.pixel_to_ndc(x, y);
        let tr = self.pixel_to_ndc(x + w, y);
        let bl = self.pixel_to_ndc(x, y + h);
        let br = self.pixel_to_ndc(x + w, y + h);
        [
            RectVertex { position: tl, color },
            RectVertex { position: bl, color },
            RectVertex { position: tr, color },
            RectVertex { position: tr, color },
            RectVertex { position: bl, color },
            RectVertex { position: br, color },
        ]
    }

    /// Remove buffers for panes that no longer exist.
    pub fn cleanup_pane_buffers(&mut self, active_pane_ids: &[PaneId]) {
        self.pane_buffers.retain(|id, _| active_pane_ids.contains(id));
    }

    /// Render a complete frame with tab bar, multiple panes, dividers, and optional overlays.
    ///
    /// If both `overlay` (tab switcher) and `session_overlay` (session browser)
    /// are provided, the session browser takes priority.
    pub fn render_frame(
        &mut self,
        tabs: &[TabInfo],
        tab_bar_height: f32,
        panes: &[PaneContent],
        dividers: &[DividerLine],
        overlay: Option<&OverlayContent>,
        session_overlay: Option<&SessionOverlay>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let output = self.surface.get_current_texture()?;
        let view = output.texture.create_view(&TextureViewDescriptor::default());

        let resolution = Resolution {
            width: self.surface_config.width,
            height: self.surface_config.height,
        };
        self.viewport.update(&self.queue, resolution);

        let cw = self.cell_width();
        let lh = self.line_height();
        let sw = self.surface_config.width as i32;
        let sh = self.surface_config.height as i32;

        // --- Build all rect vertices (bg cells + selection + dividers + focus borders) ---
        let mut all_rect_vertices: Vec<RectVertex> = Vec::new();

        // When an overlay is active, skip all pane content (bg cells, text, cursor)
        // so it doesn't bleed through the dim
        let has_overlay = overlay.is_some() || session_overlay.is_some();

        if !has_overlay {
            for pane in panes {
                let ox = pane.x;
                let oy = pane.y;

                // Background cells
                for cell in &pane.bg_cells {
                    let x0 = ox + PADDING + cell.col as f32 * cw;
                    let y0 = oy + PADDING + cell.row as f32 * lh;
                    let color = [
                        Self::srgb_to_linear(cell.r),
                        Self::srgb_to_linear(cell.g),
                        Self::srgb_to_linear(cell.b),
                        1.0,
                    ];
                    all_rect_vertices.extend_from_slice(&self.build_rect_vertices(x0, y0, cw, lh, color));
                }

                // Selection cells
                let sel_color = Self::color3(SELECTION_COLOR);
                for cell in &pane.selection_cells {
                    let x0 = ox + PADDING + cell.col as f32 * cw;
                    let y0 = oy + PADDING + cell.row as f32 * lh;
                    all_rect_vertices.extend_from_slice(&self.build_rect_vertices(x0, y0, cw, lh, sel_color));
                }

                // Focus border (colored border around focused pane)
                if pane.is_focused && panes.len() > 1 {
                    let border_color = Self::color3(FOCUS_BORDER_COLOR);
                    let t = FOCUS_BORDER_THICKNESS;
                    all_rect_vertices.extend_from_slice(&self.build_rect_vertices(ox, oy, pane.width, t, border_color));
                    all_rect_vertices.extend_from_slice(&self.build_rect_vertices(ox, oy + pane.height - t, pane.width, t, border_color));
                    all_rect_vertices.extend_from_slice(&self.build_rect_vertices(ox, oy, t, pane.height, border_color));
                    all_rect_vertices.extend_from_slice(&self.build_rect_vertices(ox + pane.width - t, oy, t, pane.height, border_color));
                }
            }
        }

        // Divider lines
        let div_color = Self::color3(DIVIDER_COLOR);
        for div in dividers {
            all_rect_vertices.extend_from_slice(
                &self.build_rect_vertices(div.x, div.y, div.width, div.height, div_color),
            );
        }

        // Tab bar
        let tab_gap = TAB_GAP_LOGICAL * self.scale_factor;
        let max_tab_w = MAX_TAB_WIDTH_LOGICAL * self.scale_factor;
        let min_tab_w = MIN_TAB_WIDTH_LOGICAL * self.scale_factor;
        let surface_w = self.surface_config.width as f32;
        let tab_width = if !tabs.is_empty() {
            let available = surface_w - tab_gap;
            (available / tabs.len() as f32 - tab_gap).clamp(min_tab_w, max_tab_w)
        } else {
            max_tab_w
        };

        if !tabs.is_empty() {
            let tab_bg = Self::color3(TAB_BAR_BG);
            all_rect_vertices.extend_from_slice(
                &self.build_rect_vertices(0.0, 0.0, surface_w, tab_bar_height, tab_bg),
            );

            let tab_inner_h = tab_bar_height - tab_gap * 2.0;

            for (i, tab) in tabs.iter().enumerate() {
                let x = tab_gap + i as f32 * (tab_width + tab_gap);
                let y = tab_gap;
                let color = if tab.is_active {
                    Self::color3(TAB_ACTIVE_BG)
                } else {
                    Self::color3(TAB_INACTIVE_BG)
                };
                all_rect_vertices.extend_from_slice(
                    &self.build_rect_vertices(x, y, tab_width, tab_inner_h, color),
                );
            }
        }

        // --- Phase 1: Populate all text buffers ---
        let mono = Family::Monospace;
        let default_attrs = Attrs::new().family(mono);
        let metrics = Metrics::new(self.physical_font_size(), self.line_height());

        // Track cursor info for the focused pane
        let mut cursor_info: Option<(f32, f32, i32, i32, i32, i32)> = None;

        for pane in panes.iter() {
            let buf = self.pane_buffers.entry(pane.pane_id).or_insert_with(|| {
                Buffer::new(&mut self.font_system, metrics)
            });
            buf.set_size(&mut self.font_system, Some(pane.width), Some(pane.height));

            let rich_spans: Vec<(&str, Attrs)> = pane.spans.iter().map(|span| {
                let mut attrs = Attrs::new()
                    .family(mono)
                    .color(Color::rgb(span.r, span.g, span.b));
                if span.bold { attrs = attrs.weight(Weight::BOLD); }
                if span.italic { attrs = attrs.style(Style::Italic); }
                (span.text.as_str(), attrs)
            }).collect();

            buf.set_rich_text(&mut self.font_system, rich_spans, &default_attrs, Shaping::Advanced, None);
            buf.shape_until_scroll(&mut self.font_system, false);

            if pane.is_focused {
                let cursor_x = pane.x + PADDING + pane.cursor_col as f32 * cw;
                let cursor_y = pane.y + PADDING + pane.cursor_row as f32 * lh;
                let bl = pane.x as i32;
                let bt = pane.y as i32;
                let br = (pane.x + pane.width) as i32;
                let bb = (pane.y + pane.height) as i32;
                cursor_info = Some((cursor_x, cursor_y, bl, bt, br, bb));

                self.cursor_buffer.set_text(
                    &mut self.font_system,
                    "\u{2588}",
                    &default_attrs,
                    Shaping::Advanced,
                    None,
                );
                self.cursor_buffer.shape_until_scroll(&mut self.font_system, false);
            }
        }

        // --- Phase 2: Build TextAreas from populated buffers ---
        let mut text_areas: Vec<TextArea> = Vec::new();

        // Skip pane text when an overlay covers the screen (text would render on top of dim)
        if !has_overlay {
            for pane in panes.iter() {
                if let Some(buf) = self.pane_buffers.get(&pane.pane_id) {
                    text_areas.push(TextArea {
                        buffer: buf,
                        left: pane.x + PADDING,
                        top: pane.y + PADDING,
                        scale: 1.0,
                        bounds: TextBounds {
                            left: (pane.x as i32).max(0),
                            top: (pane.y as i32).max(0),
                            right: ((pane.x + pane.width) as i32).min(sw),
                            bottom: ((pane.y + pane.height) as i32).min(sh),
                        },
                        default_color: glyphon_rgb(DEFAULT_TEXT_COLOR),
                        custom_glyphs: &[],
                    });
                }
            }

            if let Some((cx, cy, bl, bt, br, bb)) = cursor_info {
                text_areas.push(TextArea {
                    buffer: &self.cursor_buffer,
                    left: cx,
                    top: cy,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: bl.max(0),
                        top: bt.max(0),
                        right: br.min(sw),
                        bottom: bb.min(sh),
                    },
                    default_color: Color::rgba(CURSOR_COLOR[0], CURSOR_COLOR[1], CURSOR_COLOR[2], CURSOR_COLOR[3]),
                custom_glyphs: &[],
            });
        }
        } // end if !has_overlay

        // Tab titles — one buffer per tab, positioned to match its rectangle
        if !tabs.is_empty() {
            let text_x_inset = TAB_TEXT_INSET_LOGICAL * self.scale_factor;
            let text_y = tab_gap + TAB_TEXT_Y_PAD_LOGICAL * self.scale_factor;
            let tab_font_size = TAB_FONT_SIZE_LOGICAL * self.scale_factor;
            let tab_line_height = TAB_LINE_HEIGHT_LOGICAL * self.scale_factor;
            let tab_metrics = Metrics::new(tab_font_size, tab_line_height);

            // Estimate max chars that fit in the tab
            let char_w = tab_font_size * TAB_CHAR_WIDTH_RATIO;
            let max_chars = ((tab_width - text_x_inset * 2.0) / char_w).floor().max(3.0) as usize;

            while self.tab_buffers.len() < tabs.len() {
                self.tab_buffers.push(Buffer::new(&mut self.font_system, tab_metrics));
            }
            self.tab_buffers.truncate(tabs.len());

            for (i, tab) in tabs.iter().enumerate() {
                let prefix = format!("{}. ", i + 1);
                let title_budget = max_chars.saturating_sub(prefix.len());
                let title = if tab.title.chars().count() > title_budget && title_budget > 1 {
                    let truncated: String = tab.title.chars().take(title_budget - 1).collect();
                    format!("{truncated}…")
                } else {
                    tab.title.clone()
                };
                let label = format!("{prefix}{title}");
                let color = if tab.is_active {
                    glyphon_rgb(TAB_ACTIVE_TEXT_COLOR)
                } else {
                    glyphon_rgb(TAB_INACTIVE_TEXT_COLOR)
                };

                let buf = &mut self.tab_buffers[i];
                buf.set_metrics(&mut self.font_system, tab_metrics);
                buf.set_size(&mut self.font_system, Some(tab_width), Some(tab_line_height));
                buf.set_text(
                    &mut self.font_system, &label,
                    &Attrs::new().family(mono).color(color),
                    Shaping::Advanced, None,
                );
                buf.shape_until_scroll(&mut self.font_system, false);
            }

            for (i, buf) in self.tab_buffers.iter().enumerate() {
                let tab_x = tab_gap + i as f32 * (tab_width + tab_gap) + text_x_inset;
                text_areas.push(TextArea {
                    buffer: buf,
                    left: tab_x,
                    top: text_y,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: 0, top: 0,
                        right: sw, bottom: tab_bar_height as i32,
                    },
                    default_color: glyphon_rgb(SECONDARY_TEXT_COLOR),
                    custom_glyphs: &[],
                });
            }
        }

        // --- Overlay (session browser takes priority over tab switcher) ---
        let show_tab_overlay = overlay.is_some() && session_overlay.is_none();
        if let Some(session_ov) = session_overlay {
            let scale = self.scale_factor;
            let panel_w = SESSION_PANEL_WIDTH_LOGICAL * scale;
            let entry_h = SESSION_ENTRY_HEIGHT_LOGICAL * scale;
            let search_h = SESSION_SEARCH_HEIGHT_LOGICAL * scale;
            let panel_padding = SESSION_PADDING_LOGICAL * scale;

            // Cap visible entries to fit within screen height
            let max_panel_h = sh as f32 * SESSION_MAX_HEIGHT_RATIO;
            let content_budget = max_panel_h - search_h - panel_padding * 2.0;
            let max_visible = (content_budget / entry_h).floor().max(1.0) as usize;
            let total = session_ov.entries.len();
            let visible_count = if total == 0 { 0 } else { total.min(max_visible) };
            let is_empty = total == 0;

            // Scroll window: keep selected entry visible
            let scroll_offset = if session_ov.selected_index >= visible_count {
                (session_ov.selected_index - visible_count + 1).min(total.saturating_sub(visible_count))
            } else {
                0
            };

            let empty_msg_h = if is_empty { entry_h } else { 0.0 };
            let panel_h = search_h + entry_h * visible_count as f32 + empty_msg_h + panel_padding * 2.0;
            let panel_x = (sw as f32 - panel_w) / 2.0;
            let panel_y = (sh as f32 - panel_h) / 2.0;

            // Dimmed background
            let dim = [0.0_f32, 0.0, 0.0, OVERLAY_DIM_ALPHA];
            all_rect_vertices.extend_from_slice(
                &self.build_rect_vertices(0.0, 0.0, sw as f32, sh as f32, dim),
            );

            // Panel background
            let panel_bg = Self::color3(OVERLAY_PANEL_BG);
            all_rect_vertices.extend_from_slice(
                &self.build_rect_vertices(panel_x, panel_y, panel_w, panel_h, panel_bg),
            );

            // Search bar background
            let search_bg = Self::color3(SESSION_SEARCH_BAR_BG);
            all_rect_vertices.extend_from_slice(
                &self.build_rect_vertices(
                    panel_x + SESSION_ENTRY_INSET_LOGICAL * scale,
                    panel_y + panel_padding,
                    panel_w - SESSION_ENTRY_INSET_LOGICAL * 2.0 * scale,
                    search_h - SESSION_ENTRY_GAP_LOGICAL * scale,
                    search_bg,
                ),
            );

            // Entry backgrounds
            let entries_top = panel_y + panel_padding + search_h;
            for vi in 0..visible_count {
                let entry_idx = scroll_offset + vi;
                let ey = entries_top + vi as f32 * entry_h;
                let entry = &session_ov.entries[entry_idx];
                let is_header = matches!(entry.kind, SessionEntryKind::ProjectHeader { .. });
                let color = if entry.is_selected {
                    if is_header {
                        Self::color3(SESSION_HEADER_SELECTED_BG)
                    } else {
                        Self::color3(OVERLAY_SELECTED_BG)
                    }
                } else {
                    Self::color3(OVERLAY_NORMAL_BG)
                };
                all_rect_vertices.extend_from_slice(
                    &self.build_rect_vertices(
                        panel_x + SESSION_ENTRY_INSET_LOGICAL * scale,
                        ey,
                        panel_w - SESSION_ENTRY_INSET_LOGICAL * 2.0 * scale,
                        entry_h - SESSION_ENTRY_GAP_LOGICAL * scale,
                        color,
                    ),
                );
            }

            // Build text spans for the session overlay buffer
            let primary_font_size = SESSION_FONT_SIZE_LOGICAL * scale;
            // Each entry is 2 text lines, so line height = entry_h / 2
            let text_line_height = entry_h / 2.0;
            let session_metrics = Metrics::new(primary_font_size, text_line_height);
            self.session_overlay_buffer.set_metrics(&mut self.font_system, session_metrics);
            self.session_overlay_buffer.set_size(&mut self.font_system, Some(panel_w), Some(panel_h));
            self.session_overlay_buffer.set_wrap(&mut self.font_system, cosmic_text::Wrap::None);

            // Estimate max chars per line
            let char_w = primary_font_size * SESSION_CHAR_WIDTH_RATIO;
            let usable_w = panel_w - panel_padding * 2.0;
            let max_chars = (usable_w / char_w).floor().max(10.0) as usize;

            let mut rich_spans: Vec<(String, Attrs)> = Vec::new();

            // Search bar text
            if session_ov.filter.is_empty() {
                rich_spans.push((
                    "Search sessions...".to_string(),
                    Attrs::new().family(mono).color(glyphon_rgb(SESSION_SEARCH_PLACEHOLDER_COLOR)),
                ));
            } else {
                rich_spans.push((
                    session_ov.filter.clone(),
                    Attrs::new().family(mono).color(glyphon_rgb(SESSION_SEARCH_TEXT_COLOR)),
                ));
            }

            // Empty state message
            if is_empty {
                rich_spans.push(("\n".to_string(), Attrs::new().family(mono)));
                rich_spans.push((
                    "  No sessions found".to_string(),
                    Attrs::new().family(mono).color(glyphon_rgb(SESSION_SEARCH_PLACEHOLDER_COLOR)),
                ));
            }

            // Entry lines
            for vi in 0..visible_count {
                let entry_idx = scroll_offset + vi;
                let entry = &session_ov.entries[entry_idx];

                let primary_color = if entry.is_selected {
                    glyphon_rgb(OVERLAY_SELECTED_TEXT_COLOR)
                } else {
                    glyphon_rgb(OVERLAY_NORMAL_TEXT_COLOR)
                };
                let secondary_color = if entry.is_selected {
                    glyphon_rgb(OVERLAY_NORMAL_TEXT_COLOR)
                } else {
                    glyphon_rgb(SESSION_SECONDARY_TEXT_COLOR)
                };

                match &entry.kind {
                    SessionEntryKind::ProjectHeader { name, session_count, expanded } => {
                        let arrow = if *expanded { "▾" } else { "▸" };
                        // Line 1: "▾ project_name (N sessions)"
                        rich_spans.push(("\n".to_string(), Attrs::new().family(mono)));
                        rich_spans.push((
                            format!("{arrow} {name} ({session_count} sessions)"),
                            Attrs::new().family(mono).weight(Weight::BOLD).color(primary_color),
                        ));
                        // Line 2: empty (fills the 2-line entry height)
                        rich_spans.push(("\n".to_string(), Attrs::new().family(mono)));
                    }
                    SessionEntryKind::Session { summary, is_active, tree_prefix, time_ago, message_count, model, .. } => {
                        // Line 1: tree_prefix + [●] "summary..."
                        rich_spans.push(("\n".to_string(), Attrs::new().family(mono)));

                        if !tree_prefix.is_empty() {
                            rich_spans.push((
                                tree_prefix.clone(),
                                Attrs::new().family(mono).color(secondary_color),
                            ));
                        }

                        let active_prefix = if *is_active { "● " } else { "  " };
                        if *is_active {
                            rich_spans.push((
                                active_prefix.to_string(),
                                Attrs::new().family(mono).color(glyphon_rgb(SESSION_ACTIVE_INDICATOR_COLOR)),
                            ));
                        } else {
                            rich_spans.push((
                                active_prefix.to_string(),
                                Attrs::new().family(mono).color(primary_color),
                            ));
                        }

                        if !summary.is_empty() {
                            let prefix_len = tree_prefix.chars().count() + active_prefix.len() + 1; // +1 for opening quote
                            let summary_budget = max_chars.saturating_sub(prefix_len + 1); // +1 for closing quote
                            let summary_display = if summary.chars().count() > summary_budget && summary_budget > 4 {
                                let truncated: String = summary.chars().take(summary_budget - 1).collect();
                                format!("{truncated}…")
                            } else {
                                summary.clone()
                            };
                            rich_spans.push((
                                format!("\"{summary_display}\""),
                                Attrs::new().family(mono).color(primary_color),
                            ));
                        }

                        // Line 2: indent + time_ago · N messages · model
                        // Use char count, not byte length (tree chars like ├─ are multi-byte)
                        let indent = " ".repeat(tree_prefix.chars().count() + active_prefix.len());
                        let detail_line = format!(
                            "\n{indent}{time_ago} · {message_count} messages · {model}",
                        );
                        rich_spans.push((
                            detail_line,
                            Attrs::new()
                                .family(mono)
                                .color(secondary_color),
                        ));
                    }
                }
            }

            let rich_refs: Vec<(&str, Attrs)> = rich_spans.iter().map(|(t, a)| (t.as_str(), a.clone())).collect();
            self.session_overlay_buffer.set_rich_text(
                &mut self.font_system, rich_refs, &default_attrs, Shaping::Advanced, None,
            );
            self.session_overlay_buffer.shape_until_scroll(&mut self.font_system, false);

            text_areas.push(TextArea {
                buffer: &self.session_overlay_buffer,
                left: panel_x + panel_padding,
                top: panel_y + panel_padding,
                scale: 1.0,
                bounds: TextBounds {
                    left: panel_x as i32,
                    top: panel_y as i32,
                    right: (panel_x + panel_w) as i32,
                    bottom: (panel_y + panel_h) as i32,
                },
                default_color: glyphon_rgb(SECONDARY_TEXT_COLOR),
                custom_glyphs: &[],
            });
        }

        if show_tab_overlay {
            let overlay = overlay.unwrap(); // safe: checked above
            let scale = self.scale_factor;
            let panel_w = OVERLAY_PANEL_WIDTH_LOGICAL * scale;
            let entry_h = OVERLAY_ENTRY_HEIGHT_LOGICAL * scale;
            let panel_padding = OVERLAY_PADDING_LOGICAL * scale;

            // Cap visible entries to fit within 70% of screen height
            let max_panel_h = sh as f32 * OVERLAY_MAX_HEIGHT_RATIO;
            let max_visible = ((max_panel_h - panel_padding * 2.0) / entry_h).floor().max(1.0) as usize;
            let total = overlay.entries.len();
            let visible_count = total.min(max_visible);

            // Scroll window: keep selected entry visible
            let scroll_offset = if overlay.selected_index >= visible_count {
                (overlay.selected_index - visible_count + 1).min(total - visible_count)
            } else {
                0
            };

            let panel_h = entry_h * visible_count as f32 + panel_padding * 2.0;
            let panel_x = (sw as f32 - panel_w) / 2.0;
            let panel_y = (sh as f32 - panel_h) / 2.0;

            // Dimmed background
            let dim = [0.0_f32, 0.0, 0.0, OVERLAY_DIM_ALPHA];
            all_rect_vertices.extend_from_slice(
                &self.build_rect_vertices(0.0, 0.0, sw as f32, sh as f32, dim),
            );

            // Panel background
            let panel_bg = Self::color3(OVERLAY_PANEL_BG);
            all_rect_vertices.extend_from_slice(
                &self.build_rect_vertices(panel_x, panel_y, panel_w, panel_h, panel_bg),
            );

            // Visible entry backgrounds
            for vi in 0..visible_count {
                let entry_idx = scroll_offset + vi;
                let ey = panel_y + panel_padding + vi as f32 * entry_h;
                let color = if entry_idx == overlay.selected_index {
                    Self::color3(OVERLAY_SELECTED_BG)
                } else {
                    Self::color3(OVERLAY_NORMAL_BG)
                };
                all_rect_vertices.extend_from_slice(
                    &self.build_rect_vertices(
                        panel_x + OVERLAY_ENTRY_INSET_LOGICAL * scale,
                        ey,
                        panel_w - OVERLAY_ENTRY_INSET_LOGICAL * 2.0 * scale,
                        entry_h - OVERLAY_ENTRY_GAP_LOGICAL * scale,
                        color,
                    ),
                );
            }

            // Overlay text (only visible entries)
            let overlay_font_size = OVERLAY_FONT_SIZE_LOGICAL * scale;
            let overlay_line_height = entry_h;
            let overlay_metrics = Metrics::new(overlay_font_size, overlay_line_height);
            self.overlay_buffer.set_metrics(&mut self.font_system, overlay_metrics);
            self.overlay_buffer.set_size(&mut self.font_system, Some(panel_w), Some(panel_h));

            let has_above = scroll_offset > 0;
            let has_below = scroll_offset + visible_count < total;

            // Max chars that fit in the panel (estimate from font size)
            let overlay_char_w = OVERLAY_FONT_SIZE_LOGICAL * scale * 0.6;
            let max_label_chars = ((panel_w - OVERLAY_PADDING_LOGICAL * scale * 2.0) / overlay_char_w)
                .floor().max(10.0) as usize;

            let truncate_label = |label: &str, suffix: &str| -> String {
                let budget = max_label_chars.saturating_sub(suffix.len());
                if label.chars().count() > budget && budget > 4 {
                    let truncated: String = label.chars().take(budget - 1).collect();
                    format!("{truncated}…{suffix}")
                } else {
                    format!("{label}{suffix}")
                }
            };

            let mut spans: Vec<(String, bool)> = Vec::new();
            for vi in 0..visible_count {
                let entry_idx = scroll_offset + vi;
                let entry = &overlay.entries[entry_idx];
                if vi > 0 {
                    spans.push(("\n".to_string(), false));
                }
                let prefix = if entry.is_active { " ● " } else { "   " };
                let prefixed = format!("{prefix}{}", entry.label);
                let text = if vi == 0 && has_above {
                    truncate_label(&prefixed, &format!(" ↑{scroll_offset}"))
                } else if vi == visible_count - 1 && has_below {
                    let below = total - scroll_offset - visible_count;
                    truncate_label(&prefixed, &format!(" ↓{below}"))
                } else {
                    truncate_label(&prefixed, "")
                };
                spans.push((text, entry_idx == overlay.selected_index));
            }

            let rich_spans: Vec<(&str, Attrs)> = spans.iter().map(|(text, is_selected)| {
                let color = if *is_selected {
                    glyphon_rgb(OVERLAY_SELECTED_TEXT_COLOR)
                } else {
                    glyphon_rgb(OVERLAY_NORMAL_TEXT_COLOR)
                };
                (text.as_str(), Attrs::new().family(mono).color(color))
            }).collect();

            self.overlay_buffer.set_rich_text(
                &mut self.font_system, rich_spans, &default_attrs, Shaping::Advanced, None,
            );
            self.overlay_buffer.shape_until_scroll(&mut self.font_system, false);

            text_areas.push(TextArea {
                buffer: &self.overlay_buffer,
                left: panel_x + panel_padding,
                top: panel_y + panel_padding,
                scale: 1.0,
                bounds: TextBounds {
                    left: panel_x as i32,
                    top: panel_y as i32,
                    right: (panel_x + panel_w) as i32,
                    bottom: (panel_y + panel_h) as i32,
                },
                default_color: glyphon_rgb(SECONDARY_TEXT_COLOR),
                custom_glyphs: &[],
            });
        }

        // --- Prepare text renderer ---
        self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.text_atlas,
            &self.viewport,
            text_areas,
            &mut self.swash_cache,
        )?;

        // --- Encode render pass ---
        let mut encoder = self.device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("nexterm_render"),
        });

        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("nexterm_render_pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color {
                            r: BG_R as f64 / 255.0,
                            g: BG_G as f64 / 255.0,
                            b: BG_B as f64 / 255.0,
                            a: 1.0,
                        }),
                        store: StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // 1. Draw rects (backgrounds, selection, dividers, focus borders)
            if !all_rect_vertices.is_empty() {
                let buf = self.device.create_buffer_init(&BufferInitDescriptor {
                    label: Some("rect_vertices"),
                    contents: bytemuck::cast_slice(&all_rect_vertices),
                    usage: BufferUsages::VERTEX,
                });
                pass.set_pipeline(&self.rect_pipeline);
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..all_rect_vertices.len() as u32, 0..1);
            }

            // 2. Draw text (all panes + cursors)
            self.text_renderer
                .render(&self.text_atlas, &self.viewport, &mut pass)
                .unwrap();
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();
        self.text_atlas.trim();

        Ok(())
    }
}
