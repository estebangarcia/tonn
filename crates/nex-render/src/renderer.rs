//! Core renderer using wgpu for GPU rendering and glyphon for text.

use glyphon::{
    Attrs, Buffer, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache, Style,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};
use wgpu::{
    util::{BufferInitDescriptor, DeviceExt},
    BufferUsages, CommandEncoderDescriptor, Device, DeviceDescriptor, Instance, InstanceDescriptor,
    LoadOp, MultisampleState, Operations, PipelineCompilationOptions, PowerPreference, Queue,
    RenderPassColorAttachment, RenderPassDescriptor, RenderPipeline, RequestAdapterOptions,
    StoreOp, Surface, SurfaceConfiguration, TextureUsages, TextureViewDescriptor,
};

/// Default logical font size in points.
pub const DEFAULT_FONT_SIZE: f32 = 14.0;
pub const MIN_FONT_SIZE: f32 = 8.0;
pub const MAX_FONT_SIZE: f32 = 40.0;
pub const FONT_SIZE_STEP: f32 = 1.0;
const LINE_HEIGHT_RATIO: f32 = 1.3;
const CELL_WIDTH_RATIO: f32 = 0.6;
const PADDING: f32 = 8.0;

// Background color of the terminal window (used to skip drawing default bg cells).
const BG_R: u8 = 13;
const BG_G: u8 = 13;
const BG_B: u8 = 18;

/// A colored text span from the terminal.
pub struct RenderSpan {
    pub text: String,
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub bold: bool,
    pub italic: bool,
}

/// A cell with a non-default background color.
pub struct BgCell {
    pub row: usize,
    pub col: usize,
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// A cell that is part of the selection highlight.
pub struct SelectionCell {
    pub row: usize,
    pub col: usize,
}

/// Vertex for the rect pipeline: position (x, y) + color (r, g, b, a).
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct RectVertex {
    position: [f32; 2],
    color: [f32; 4],
}

/// The main terminal renderer.
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
    text_buffer: Buffer,
    cursor_buffer: Buffer,
    rect_pipeline: RenderPipeline,
    /// Logical font size in points (what the user controls).
    font_size: f32,
    /// Display scale factor (e.g., 2.0 on Retina).
    scale_factor: f32,
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

        // Rect pipeline for cell backgrounds
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

        // Text rendering
        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = glyphon::Cache::new(&device);
        let mut text_atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut text_atlas, &device, MultisampleState::default(), None);
        let viewport = Viewport::new(&device, &cache);

        let font_size = DEFAULT_FONT_SIZE;
        let physical_size = font_size * scale_factor;
        let line_height = physical_size * LINE_HEIGHT_RATIO;
        let metrics = Metrics::new(physical_size, line_height);

        let mut text_buffer = Buffer::new(&mut font_system, metrics);
        text_buffer.set_size(
            &mut font_system,
            Some(size.width as f32),
            Some(size.height as f32),
        );

        let mut cursor_buffer = Buffer::new(&mut font_system, metrics);
        cursor_buffer.set_size(&mut font_system, Some(physical_size), Some(line_height));

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
            text_buffer,
            cursor_buffer,
            rect_pipeline,
            font_size,
            scale_factor,
        })
    }

    /// Logical font size in points (what the user sees/controls).
    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    /// Physical font size in pixels (logical * scale_factor).
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
        self.text_buffer.set_metrics(&mut self.font_system, metrics);
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
            self.text_buffer.set_size(
                &mut self.font_system,
                Some(width as f32),
                Some(height as f32),
            );
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

    pub fn set_content(&mut self, spans: &[RenderSpan]) {
        let mono = Family::Monospace;

        let rich_spans: Vec<(&str, Attrs)> = spans
            .iter()
            .map(|span| {
                let mut attrs = Attrs::new()
                    .family(mono)
                    .color(Color::rgb(span.r, span.g, span.b));
                if span.bold {
                    attrs = attrs.weight(Weight::BOLD);
                }
                if span.italic {
                    attrs = attrs.style(Style::Italic);
                }
                (span.text.as_str(), attrs)
            })
            .collect();

        let default_attrs = Attrs::new().family(mono);
        self.text_buffer.set_rich_text(
            &mut self.font_system,
            rich_spans,
            &default_attrs,
            Shaping::Advanced,
            None,
        );
        self.text_buffer
            .shape_until_scroll(&mut self.font_system, false);

        self.cursor_buffer.set_text(
            &mut self.font_system,
            "\u{2588}",
            &default_attrs,
            Shaping::Advanced,
            None,
        );
        self.cursor_buffer
            .shape_until_scroll(&mut self.font_system, false);
    }

    /// Convert pixel coordinates to NDC (normalized device coordinates).
    fn pixel_to_ndc(&self, x: f32, y: f32) -> [f32; 2] {
        let w = self.surface_config.width as f32;
        let h = self.surface_config.height as f32;
        [x / w * 2.0 - 1.0, 1.0 - y / h * 2.0]
    }

    /// Convert an sRGB u8 component to linear float for the GPU.
    /// The sRGB surface will convert back from linear to sRGB on output,
    /// so we must pass linear values to avoid double gamma correction.
    fn srgb_to_linear(c: u8) -> f32 {
        let c = c as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    /// Build vertex data for background-colored cells.
    fn build_bg_vertices(&self, bg_cells: &[BgCell]) -> Vec<RectVertex> {
        let cw = self.cell_width();
        let lh = self.line_height();
        let mut vertices = Vec::with_capacity(bg_cells.len() * 6);

        for cell in bg_cells {
            let x0 = PADDING + cell.col as f32 * cw;
            let y0 = PADDING + cell.row as f32 * lh;
            let x1 = x0 + cw;
            let y1 = y0 + lh;

            let tl = self.pixel_to_ndc(x0, y0);
            let tr = self.pixel_to_ndc(x1, y0);
            let bl = self.pixel_to_ndc(x0, y1);
            let br = self.pixel_to_ndc(x1, y1);

            let color = [
                Self::srgb_to_linear(cell.r),
                Self::srgb_to_linear(cell.g),
                Self::srgb_to_linear(cell.b),
                1.0,
            ];

            // Two triangles per cell
            vertices.push(RectVertex { position: tl, color });
            vertices.push(RectVertex { position: bl, color });
            vertices.push(RectVertex { position: tr, color });
            vertices.push(RectVertex { position: tr, color });
            vertices.push(RectVertex { position: bl, color });
            vertices.push(RectVertex { position: br, color });
        }

        vertices
    }

    pub fn render(
        &mut self,
        cursor_row: usize,
        cursor_col: usize,
        bg_cells: &[BgCell],
        selection_cells: &[SelectionCell],
        bell_active: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let output = self.surface.get_current_texture()?;
        let view = output.texture.create_view(&TextureViewDescriptor::default());

        let resolution = Resolution {
            width: self.surface_config.width,
            height: self.surface_config.height,
        };
        self.viewport.update(&self.queue, resolution);

        let cursor_x = PADDING + cursor_col as f32 * self.cell_width();
        let cursor_y = PADDING + cursor_row as f32 * self.line_height();
        let w = self.surface_config.width as i32;
        let h = self.surface_config.height as i32;

        self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.text_atlas,
            &self.viewport,
            [
                TextArea {
                    buffer: &self.text_buffer,
                    left: PADDING,
                    top: PADDING,
                    scale: 1.0,
                    bounds: TextBounds { left: 0, top: 0, right: w, bottom: h },
                    default_color: Color::rgb(204, 204, 204),
                    custom_glyphs: &[],
                },
                TextArea {
                    buffer: &self.cursor_buffer,
                    left: cursor_x,
                    top: cursor_y,
                    scale: 1.0,
                    bounds: TextBounds { left: 0, top: 0, right: w, bottom: h },
                    default_color: Color::rgba(200, 200, 200, 180),
                    custom_glyphs: &[],
                },
            ],
            &mut self.swash_cache,
        )?;

        // Build background vertex buffer
        let bg_vertices = self.build_bg_vertices(bg_cells);

        // Build selection highlight vertices (semi-transparent white overlay)
        let sel_cells: Vec<BgCell> = selection_cells
            .iter()
            .map(|s| BgCell { row: s.row, col: s.col, r: 80, g: 120, b: 200 })
            .collect();
        let sel_vertices = self.build_bg_vertices(&sel_cells);

        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("nexterm_render"),
            });

        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("nexterm_render_pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(if bell_active {
                            wgpu::Color { r: 0.15, g: 0.15, b: 0.18, a: 1.0 }
                        } else {
                            wgpu::Color {
                                r: BG_R as f64 / 255.0,
                                g: BG_G as f64 / 255.0,
                                b: BG_B as f64 / 255.0,
                                a: 1.0,
                            }
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

            // 1. Draw cell backgrounds
            if !bg_vertices.is_empty() {
                let bg_buffer = self.device.create_buffer_init(&BufferInitDescriptor {
                    label: Some("bg_vertices"),
                    contents: bytemuck::cast_slice(&bg_vertices),
                    usage: BufferUsages::VERTEX,
                });
                pass.set_pipeline(&self.rect_pipeline);
                pass.set_vertex_buffer(0, bg_buffer.slice(..));
                pass.draw(0..bg_vertices.len() as u32, 0..1);
            }

            // 2. Draw selection highlights
            if !sel_vertices.is_empty() {
                let sel_buffer = self.device.create_buffer_init(&BufferInitDescriptor {
                    label: Some("sel_vertices"),
                    contents: bytemuck::cast_slice(&sel_vertices),
                    usage: BufferUsages::VERTEX,
                });
                pass.set_pipeline(&self.rect_pipeline);
                pass.set_vertex_buffer(0, sel_buffer.slice(..));
                pass.draw(0..sel_vertices.len() as u32, 0..1);
            }

            // 3. Draw text on top
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
