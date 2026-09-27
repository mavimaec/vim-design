//! wgpu renderer shared by the demo and the authoring app (wasm only).
//!
//! Right-handed, Z-up (docs/ARCHITECTURE.md §7). Prefers the browser's
//! WebGPU backend and falls back to WebGL2 (wgpu `webgl` feature) when no
//! WebGPU adapter is available — the chosen path is reported by
//! [`Renderer::backend_name`]. The projection is the caller's business
//! (`render(view_proj)`): perspective orbit and orthographic plan views
//! both work unchanged.
//!
//! Bookkeeping follows the poll facade contract: two upsert maps,
//! `mesh owner id -> GPU mesh` and `instance id -> (element id,
//! transform)`. An owner with no instances is drawn once at its base
//! transform; otherwise once per instance (`world = instance ∘ base`).
//!
//! Layers, in draw order:
//! 1. shaded fill (per-owner tint via [`MeshStyle`]),
//! 2. optional full wireframe (demo),
//! 3. feature edges (crease/boundary lines, per-owner color; off unless
//!    a style gives them alpha),
//! 4. level overlay quads (demo), grid lines (distance fade) and plain
//!    overlay lines (level outlines) — view-only, never document data,
//! 5. draw-tool preview markers/lines (depth Always).
//!
//! Wireframe: wgpu's `PolygonMode::Line` needs `NON_FILL_POLYGON_MODE`,
//! which browsers do not expose — instead each mesh carries line-list
//! index buffers over the same vertex buffer (all triangle edges for the
//! wireframe; crease/boundary edges for the feature-edge layer).

use std::collections::{HashMap, HashSet};

use glam::{Mat4, Vec3};
use vim_design_lib::eval::Mesh;
use vim_design_lib::EntityId;
use wgpu::util::DeviceExt;

/// Neutral default color for submeshes without a material.
pub const DEFAULT_COLOR: [f32; 3] = [0.78, 0.78, 0.75];

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;
/// Dynamic-offset stride for per-draw uniforms (WebGL2 requires
/// 256-byte alignment).
const MODEL_STRIDE: u64 = 256;
/// Per-draw uniform payload: model matrix + tint + edge color.
const MODEL_SIZE: u64 = 96;
const GLOBALS_SIZE: u64 = 128;
/// Dihedral angle above which a mesh edge is a feature edge.
const FEATURE_ANGLE_COS: f32 = 0.866; // 30°

const SHADER: &str = r#"
// light_dir.w doubles as the "gamma encode in shader" flag: 1.0 when the
// surface format is not sRGB (observed on the browser WebGPU path, where
// the preferred canvas format is non-sRGB), 0.0 when the hardware
// encodes. Keeps WebGPU and WebGL2 output visually identical.
struct Globals {
    view_proj: mat4x4<f32>,
    light_dir: vec4<f32>,
    wire_color: vec4<f32>,
    // Grid fade: xyz = center, w = radius (<= 0: no fade).
    fade: vec4<f32>,
    // Feature-edge depth nudge: xyz = eye (or a far point behind an
    // orthographic camera), w = relative pull toward it.
    eye: vec4<f32>,
};

fn encode(c: vec3<f32>) -> vec3<f32> {
    if (globals.light_dir.w > 0.5) {
        return pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2));
    }
    return c;
}
@group(0) @binding(0) var<uniform> globals: Globals;

struct Model {
    m: mat4x4<f32>,
    // rgb mixed into the surface color by a.
    tint: vec4<f32>,
    // Feature-edge color (a = 0: no edges drawn).
    edge: vec4<f32>,
};
@group(1) @binding(0) var<uniform> model: Model;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec3<f32>,
};

fn transform(p: vec3<f32>, n: vec3<f32>, c: vec3<f32>) -> VsOut {
    var out: VsOut;
    let world = model.m * vec4<f32>(p, 1.0);
    out.pos = globals.view_proj * world;
    out.normal = (model.m * vec4<f32>(n, 0.0)).xyz;
    out.color = c;
    return out;
}

@vertex
fn vs_main(
    @location(0) p: vec3<f32>,
    @location(1) n: vec3<f32>,
    @location(2) c: vec3<f32>,
) -> VsOut {
    return transform(p, n, c);
}

@vertex
fn vs_wire(
    @location(0) p: vec3<f32>,
    @location(1) n: vec3<f32>,
    @location(2) c: vec3<f32>,
) -> VsOut {
    var out: VsOut = transform(p, n, c);
    // Pull the wireframe slightly toward the camera so it wins the depth
    // test against the triangles it outlines.
    out.pos.z = out.pos.z - 8e-4 * out.pos.w;
    return out;
}

@vertex
fn vs_edge(
    @location(0) p: vec3<f32>,
    @location(1) n: vec3<f32>,
    @location(2) c: vec3<f32>,
) -> VsOut {
    var out: VsOut;
    let world = (model.m * vec4<f32>(p, 1.0)).xyz;
    // Pull toward the eye by a fraction of the distance: a view-space
    // offset that behaves the same for perspective and orthographic
    // projections (a clip-z bias does not).
    let eye = globals.eye.xyz;
    let pulled = eye + (world - eye) * (1.0 - globals.eye.w);
    out.pos = globals.view_proj * vec4<f32>(pulled, 1.0);
    out.normal = n;
    out.color = c;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let l = normalize(globals.light_dir.xyz);
    let key = max(dot(n, l), 0.0);
    let fill_dir = normalize(vec3<f32>(-l.x, -l.y, 0.35));
    let fill = 0.25 * max(dot(n, fill_dir), 0.0);
    let shade = 0.24 + 0.72 * key + fill;
    let base = mix(in.color, model.tint.rgb, model.tint.a);
    return vec4<f32>(encode(base * min(shade, 1.15)), 1.0);
}

@fragment
fn fs_wire(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(encode(globals.wire_color.rgb), globals.wire_color.a);
}

@fragment
fn fs_edge(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(encode(model.edge.rgb), model.edge.a);
}

// Overlay family (level squares, grid, outlines, previews): world
// coordinates, unlit, alpha-blended, never depth-written.
struct OverlayOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) world: vec3<f32>,
};

@vertex
fn vs_overlay(@location(0) p: vec3<f32>, @location(1) c: vec4<f32>) -> OverlayOut {
    var out: OverlayOut;
    out.pos = globals.view_proj * vec4<f32>(p, 1.0);
    out.color = c;
    out.world = p;
    return out;
}

@fragment
fn fs_overlay(in: OverlayOut) -> @location(0) vec4<f32> {
    return vec4<f32>(encode(in.color.rgb), in.color.a);
}

@fragment
fn fs_grid(in: OverlayOut) -> @location(0) vec4<f32> {
    var a = in.color.a;
    let r = globals.fade.w;
    if (r > 0.0) {
        let d = distance(in.world.xy, globals.fade.xy);
        a = a * (1.0 - smoothstep(r * 0.45, r, d));
    }
    return vec4<f32>(encode(in.color.rgb), a);
}
"#;

struct GpuMesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    wire_indices: wgpu::Buffer,
    wire_index_count: u32,
    edge_indices: wgpu::Buffer,
    edge_index_count: u32,
    triangle_count: u32,
    /// Base placement (translation factoring): mesh bytes are owner-
    /// local; `world = instance ∘ base`, base applied first. Identity
    /// for world-baked owners.
    base: Mat4,
    /// Local-space AABB (before base/instance transforms), for scene
    /// queries.
    bbox_min: [f32; 3],
    bbox_max: [f32; 3],
}

/// One level overlay square, in world coordinates.
pub struct OverlayQuad {
    pub elevation: f32,
    /// Half-size of the square (meters).
    pub extent: f32,
    /// RGBA; the caller pre-applies any active-level emphasis.
    pub color: [f32; 4],
}

/// Per-owner draw style. The default (all zero) is the plain shaded
/// look with no feature edges.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MeshStyle {
    /// rgb mixed into the surface color by `a` (selection, dimming).
    pub tint: [f32; 4],
    /// Feature-edge color; `a == 0` draws no edges.
    pub edge: [f32; 4],
}

/// Which overlay line layer to replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineLayer {
    /// Faded with distance from the fade center ([`Renderer::set_fade`]).
    Grid,
    /// Never faded (level outlines, guides).
    Plain,
}

/// Construction options.
#[derive(Debug, Clone, Copy, Default)]
pub struct RendererOptions {
    /// Request 4x MSAA (used when the surface format supports it).
    pub msaa: bool,
}

#[derive(Default)]
struct VertexBuf {
    buf: Option<wgpu::Buffer>,
    count: u32,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    sample_count: u32,
    depth_view: wgpu::TextureView,
    msaa_view: Option<wgpu::TextureView>,
    fill_pipeline: wgpu::RenderPipeline,
    wire_pipeline: wgpu::RenderPipeline,
    edge_pipeline: wgpu::RenderPipeline,
    overlay_pipeline: wgpu::RenderPipeline,
    grid_pipeline: wgpu::RenderPipeline,
    plain_line_pipeline: wgpu::RenderPipeline,
    overlay: VertexBuf,
    grid_lines: VertexBuf,
    plain_lines: VertexBuf,
    /// Draw-tool preview (view-only, never document entities): triangle
    /// markers + rubber-band lines, drawn on top of everything.
    preview_tri_pipeline: wgpu::RenderPipeline,
    preview_line_pipeline: wgpu::RenderPipeline,
    preview_tris: VertexBuf,
    preview_lines: VertexBuf,
    globals_buf: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    model_layout: wgpu::BindGroupLayout,
    model_buf: wgpu::Buffer,
    model_bind: wgpu::BindGroup,
    model_capacity: u32,
    meshes: HashMap<EntityId, GpuMesh>,
    instances: HashMap<EntityId, (EntityId, Mat4)>,
    styles: HashMap<EntityId, MeshStyle>,
    default_style: MeshStyle,
    clear: [f64; 3],
    fade: [f32; 4],
    edge_eye: [f32; 4],
    backend: &'static str,
    pub wireframe: bool,
}

fn f32s_to_bytes(data: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 4);
    for v in data {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn u32s_to_bytes(data: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 4);
    for v in data {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Convert the document's rigid row-major 4x3 transform to a Mat4.
pub fn mat4_from_row_major_4x3(t: &[f64; 12]) -> Mat4 {
    Mat4::from_cols_array(&[
        t[0] as f32,
        t[4] as f32,
        t[8] as f32,
        0.0,
        t[1] as f32,
        t[5] as f32,
        t[9] as f32,
        0.0,
        t[2] as f32,
        t[6] as f32,
        t[10] as f32,
        0.0,
        t[3] as f32,
        t[7] as f32,
        t[11] as f32,
        1.0,
    ])
}

/// Feature edges of a triangle list: boundary edges plus creases whose
/// adjacent face normals differ by more than ~30°. Vertices are matched
/// by quantized position (tessellators duplicate vertices per face), and
/// each line reuses one of the original vertex indices.
fn feature_edges(verts: &[f32], indices: &[u32]) -> Vec<u32> {
    let key = |i: u32| -> (i64, i64, i64) {
        let b = i as usize * 9;
        let q = |v: f32| (f64::from(v) * 1e5).round() as i64;
        (q(verts[b]), q(verts[b + 1]), q(verts[b + 2]))
    };
    let pos = |i: u32| -> Vec3 {
        let b = i as usize * 9;
        Vec3::new(verts[b], verts[b + 1], verts[b + 2])
    };
    type Key = (i64, i64, i64);
    let mut edges: HashMap<(Key, Key), (u32, u32, Vec<Vec3>)> = HashMap::new();
    for tri in indices.chunks_exact(3) {
        let n = (pos(tri[1]) - pos(tri[0])).cross(pos(tri[2]) - pos(tri[0]));
        let len = n.length();
        if len <= 1e-12 {
            continue; // degenerate sliver
        }
        let n = n / len;
        for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
            let (ka, kb) = (key(a), key(b));
            if ka == kb {
                continue;
            }
            let k = if ka < kb { (ka, kb) } else { (kb, ka) };
            edges.entry(k).or_insert_with(|| (a, b, Vec::new())).2.push(n);
        }
    }
    let mut out = Vec::new();
    for (a, b, normals) in edges.values() {
        let feature = match normals.as_slice() {
            [_] => true,
            // Any adjacent face turning by more than the threshold
            // (opposite-facing folds included) makes a crease.
            [n0, rest @ ..] => rest.iter().any(|n| n0.dot(*n) < FEATURE_ANGLE_COS),
            [] => false,
        };
        if feature {
            out.push(*a);
            out.push(*b);
        }
    }
    out
}

impl Renderer {
    pub async fn new(canvas: web_sys::HtmlCanvasElement) -> Result<Renderer, String> {
        Self::with_options(canvas, RendererOptions::default()).await
    }

    pub async fn with_options(
        canvas: web_sys::HtmlCanvasElement,
        options: RendererOptions,
    ) -> Result<Renderer, String> {
        // Prefer WebGPU. Adapter probing happens *before* the canvas is
        // touched: a canvas can hold only one context type, so we must
        // not create a webgpu surface unless a WebGPU adapter exists. The
        // WebGL2 fallback is the opposite — wgpu's GL-on-web backend only
        // discovers an adapter *through* a surface (the WebGL2 context is
        // created at surface creation), so there the surface comes first.
        let width = canvas.width().max(1);
        let height = canvas.height().max(1);
        let webgpu = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::BROWSER_WEBGPU,
            ..Default::default()
        });
        let probe = webgpu
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await;
        let (backend, surface, adapter) = match probe {
            Ok(adapter) => {
                let surface = webgpu
                    .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
                    .map_err(|e| format!("webgpu create_surface failed: {e}"))?;
                ("WebGPU", surface, adapter)
            }
            Err(_) => {
                let gl = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                    backends: wgpu::Backends::GL,
                    ..Default::default()
                });
                let surface = gl
                    .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
                    .map_err(|e| format!("webgl2 create_surface failed: {e}"))?;
                let adapter = gl
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::HighPerformance,
                        force_fallback_adapter: false,
                        compatible_surface: Some(&surface),
                    })
                    .await
                    .map_err(|e| format!("no WebGPU and no WebGL2 adapter: {e}"))?;
                ("WebGL2", surface, adapter)
            }
        };

        let limits = wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits());
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("vim-design device"),
                required_features: wgpu::Features::empty(),
                required_limits: limits,
                ..Default::default()
            })
            .await
            .map_err(|e| format!("request_device failed: {e}"))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| "surface reports no formats".to_owned())?;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps
                .alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Auto),
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let sample_count = if options.msaa
            && adapter
                .get_texture_format_features(format)
                .flags
                .sample_count_supported(4)
            && adapter
                .get_texture_format_features(DEPTH_FORMAT)
                .flags
                .sample_count_supported(4)
        {
            4
        } else {
            1
        };
        let depth_view = create_depth(&device, width, height, sample_count);
        let msaa_view = create_msaa(&device, format, width, height, sample_count);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vim-design shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(GLOBALS_SIZE),
                },
                count: None,
            }],
        });
        let model_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("model layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(MODEL_SIZE),
                },
                count: None,
            }],
        });

        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: GLOBALS_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals bind"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            }],
        });

        let model_capacity = 16u32;
        let (model_buf, model_bind) =
            create_model_buffer(&device, &model_layout, model_capacity);

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh pipeline layout"),
            bind_group_layouts: &[&globals_layout, &model_layout],
            push_constant_ranges: &[],
        });

        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: 9 * 4,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x3],
        };
        let multisample = wgpu::MultisampleState {
            count: sample_count,
            ..Default::default()
        };

        let make_pipeline = |label: &str,
                             vs: &str,
                             fs: &str,
                             topology: wgpu::PrimitiveTopology,
                             depth_write: bool,
                             blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: std::slice::from_ref(&vertex_layout),
                },
                primitive: wgpu::PrimitiveState {
                    topology,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    conservative: false,
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: depth_write,
                    depth_compare: wgpu::CompareFunction::Less,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample,
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview: None,
                cache: None,
            })
        };

        let fill_pipeline = make_pipeline(
            "fill",
            "vs_main",
            "fs_main",
            wgpu::PrimitiveTopology::TriangleList,
            true,
            None,
        );
        // The wireframe is alpha-blended: on densely tessellated curved
        // surfaces (cylinder barrel, cone) opaque lines would cover
        // nearly every pixel and blacken the shading.
        let wire_pipeline = make_pipeline(
            "wire",
            "vs_wire",
            "fs_wire",
            wgpu::PrimitiveTopology::LineList,
            false,
            Some(wgpu::BlendState::ALPHA_BLENDING),
        );
        let edge_pipeline = make_pipeline(
            "edges",
            "vs_edge",
            "fs_edge",
            wgpu::PrimitiveTopology::LineList,
            false,
            Some(wgpu::BlendState::ALPHA_BLENDING),
        );

        // Overlay-family pipelines (pos + rgba vertices in world
        // coordinates, no model matrix — globals only).
        let overlay_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("overlay pipeline layout"),
            bind_group_layouts: &[&globals_layout],
            push_constant_ranges: &[],
        });
        let overlay_vertex = wgpu::VertexBufferLayout {
            array_stride: 7 * 4,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
        };
        let make_overlay_pipeline = |label: &str,
                                     fs: &str,
                                     topology: wgpu::PrimitiveTopology,
                                     depth_compare: wgpu::CompareFunction| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&overlay_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_overlay"),
                    compilation_options: Default::default(),
                    buffers: std::slice::from_ref(&overlay_vertex),
                },
                primitive: wgpu::PrimitiveState {
                    topology,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    // Never depth-written, so the scene stays visible.
                    depth_write_enabled: false,
                    depth_compare,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample,
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview: None,
                cache: None,
            })
        };
        let overlay_pipeline = make_overlay_pipeline(
            "overlay",
            "fs_overlay",
            wgpu::PrimitiveTopology::TriangleList,
            wgpu::CompareFunction::Less,
        );
        let grid_pipeline = make_overlay_pipeline(
            "grid lines",
            "fs_grid",
            wgpu::PrimitiveTopology::LineList,
            wgpu::CompareFunction::Less,
        );
        let plain_line_pipeline = make_overlay_pipeline(
            "overlay lines",
            "fs_overlay",
            wgpu::PrimitiveTopology::LineList,
            wgpu::CompareFunction::Less,
        );
        let preview_tri_pipeline = make_overlay_pipeline(
            "preview tris",
            "fs_overlay",
            wgpu::PrimitiveTopology::TriangleList,
            wgpu::CompareFunction::Always,
        );
        let preview_line_pipeline = make_overlay_pipeline(
            "preview lines",
            "fs_overlay",
            wgpu::PrimitiveTopology::LineList,
            wgpu::CompareFunction::Always,
        );

        Ok(Renderer {
            surface,
            device,
            queue,
            config,
            sample_count,
            depth_view,
            msaa_view,
            fill_pipeline,
            wire_pipeline,
            edge_pipeline,
            overlay_pipeline,
            grid_pipeline,
            plain_line_pipeline,
            overlay: VertexBuf::default(),
            grid_lines: VertexBuf::default(),
            plain_lines: VertexBuf::default(),
            preview_tri_pipeline,
            preview_line_pipeline,
            preview_tris: VertexBuf::default(),
            preview_lines: VertexBuf::default(),
            globals_buf,
            globals_bind,
            model_layout,
            model_buf,
            model_bind,
            model_capacity,
            meshes: HashMap::new(),
            instances: HashMap::new(),
            styles: HashMap::new(),
            default_style: MeshStyle::default(),
            // Linear ~[0.090, 0.106, 0.133]: the demo's dark slate.
            clear: [0.090, 0.106, 0.133],
            fade: [0.0; 4],
            edge_eye: [0.0; 4],
            backend,
            wireframe: true,
        })
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend
    }

    /// MSAA sample count in use (1 = off).
    pub fn sample_count(&self) -> u32 {
        self.sample_count
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        if width == self.config.width && height == self.config.height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.depth_view = create_depth(&self.device, width, height, self.sample_count);
        self.msaa_view =
            create_msaa(&self.device, self.config.format, width, height, self.sample_count);
    }

    pub fn aspect(&self) -> f32 {
        self.config.width as f32 / self.config.height as f32
    }

    /// Current surface size in device pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Background color (linear RGB).
    pub fn set_clear_color(&mut self, linear: [f64; 3]) {
        self.clear = linear;
    }

    /// Grid fade: lines fade out between 45% and 100% of `radius` from
    /// `center` (xy). `radius <= 0` disables the fade.
    pub fn set_fade(&mut self, center: [f32; 3], radius: f32) {
        self.fade = [center[0], center[1], center[2], radius];
    }

    /// Feature-edge depth nudge: edges are pulled toward `eye` by
    /// `fraction` of their distance (pass a far point behind an
    /// orthographic camera).
    pub fn set_edge_nudge(&mut self, eye: [f32; 3], fraction: f32) {
        self.edge_eye = [eye[0], eye[1], eye[2], fraction];
    }

    /// Style for owners without an explicit style.
    pub fn set_default_style(&mut self, style: MeshStyle) {
        self.default_style = style;
    }

    /// Replace all per-owner styles.
    pub fn set_styles(&mut self, styles: HashMap<EntityId, MeshStyle>) {
        self.styles = styles;
    }

    pub fn remove_mesh(&mut self, id: EntityId) {
        self.meshes.remove(&id);
    }

    pub fn remove_instance(&mut self, id: EntityId) {
        self.instances.remove(&id);
    }

    /// Drop every mesh and instance (document replaced).
    pub fn clear_scene(&mut self) {
        self.meshes.clear();
        self.instances.clear();
        self.styles.clear();
    }

    pub fn upsert_instance(&mut self, id: EntityId, element: EntityId, transform: &[f64; 12]) {
        self.instances
            .insert(id, (element, mat4_from_row_major_4x3(transform)));
    }

    /// Transform-only re-placement of a previously delivered mesh (the
    /// poll's `base_transforms` — geometry unchanged, no re-upload).
    pub fn set_base_transform(&mut self, id: EntityId, transform: &[f64; 12]) {
        if let Some(mesh) = self.meshes.get_mut(&id) {
            mesh.base = mat4_from_row_major_4x3(transform);
        }
    }

    /// Upload one mesh. `submesh_colors` is parallel to `mesh.submeshes`
    /// (material colors already resolved by the caller);
    /// `base_transform` is the owner's base placement (identity for
    /// world-baked owners, the level origin for factored ones).
    pub fn upsert_mesh(
        &mut self,
        id: EntityId,
        mesh: &Mesh,
        submesh_colors: &[[f32; 3]],
        base_transform: &[f64; 12],
    ) {
        // Expand to an interleaved (pos, normal, color) vertex stream.
        // Vertices are remapped per submesh so each vertex carries its
        // submesh's color (a vertex referenced by two submeshes is
        // duplicated). Out-of-range indices are skipped (never panic on
        // a malformed mesh).
        let mut verts: Vec<f32> = Vec::with_capacity(mesh.positions.len() * 9);
        let mut indices: Vec<u32> = Vec::with_capacity(mesh.indices.len());
        for (si, sub) in mesh.submeshes.iter().enumerate() {
            let color = submesh_colors.get(si).copied().unwrap_or(DEFAULT_COLOR);
            let mut remap: HashMap<u32, u32> = HashMap::new();
            let start = sub.index_start as usize;
            let end = start.saturating_add(sub.index_count as usize);
            let Some(range) = mesh.indices.get(start..end) else {
                continue;
            };
            for tri in range.chunks_exact(3) {
                let in_range = tri.iter().all(|&i| {
                    (i as usize) < mesh.positions.len() && (i as usize) < mesh.normals.len()
                });
                if !in_range {
                    continue;
                }
                for &old in tri {
                    let next = (verts.len() / 9) as u32;
                    let new = *remap.entry(old).or_insert_with(|| {
                        let p = mesh.positions[old as usize];
                        let n = mesh.normals[old as usize];
                        verts.extend_from_slice(&[
                            p[0], p[1], p[2], n[0], n[1], n[2], color[0], color[1], color[2],
                        ]);
                        next
                    });
                    indices.push(new);
                }
            }
        }

        // Deduplicated edge list for the wireframe overlay.
        let mut edges: HashSet<(u32, u32)> = HashSet::new();
        for tri in indices.chunks_exact(3) {
            for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
                edges.insert((a.min(b), a.max(b)));
            }
        }
        let mut wire: Vec<u32> = Vec::with_capacity(edges.len() * 2);
        for (a, b) in edges {
            wire.push(a);
            wire.push(b);
        }
        let feature = feature_edges(&verts, &indices);

        let buffer = |label: &str, contents: &[u8], usage: wgpu::BufferUsages| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    // Zero-sized buffers are invalid on some backends.
                    contents: if contents.is_empty() { &[0u8; 4] } else { contents },
                    usage,
                })
        };
        let vertices = buffer("mesh vertices", &f32s_to_bytes(&verts), wgpu::BufferUsages::VERTEX);
        let index_buf = buffer("mesh indices", &u32s_to_bytes(&indices), wgpu::BufferUsages::INDEX);
        let wire_buf = buffer("mesh wire", &u32s_to_bytes(&wire), wgpu::BufferUsages::INDEX);
        let edge_buf = buffer("mesh edges", &u32s_to_bytes(&feature), wgpu::BufferUsages::INDEX);
        let mut bbox_min = [f32::INFINITY; 3];
        let mut bbox_max = [f32::NEG_INFINITY; 3];
        for p in &mesh.positions {
            for axis in 0..3 {
                bbox_min[axis] = bbox_min[axis].min(p[axis]);
                bbox_max[axis] = bbox_max[axis].max(p[axis]);
            }
        }
        self.meshes.insert(
            id,
            GpuMesh {
                vertices,
                indices: index_buf,
                index_count: indices.len() as u32,
                wire_indices: wire_buf,
                wire_index_count: wire.len() as u32,
                edge_indices: edge_buf,
                edge_index_count: feature.len() as u32,
                triangle_count: (indices.len() / 3) as u32,
                base: mat4_from_row_major_4x3(base_transform),
                bbox_min,
                bbox_max,
            },
        );
    }

    fn vertex_buf(&self, data: &[f32], label: &str) -> VertexBuf {
        VertexBuf {
            buf: (!data.is_empty()).then(|| {
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some(label),
                        contents: &f32s_to_bytes(data),
                        usage: wgpu::BufferUsages::VERTEX,
                    })
            }),
            count: (data.len() / 7) as u32,
        }
    }

    /// Replace the level-overlay quads (world coordinates; caller passes
    /// them back-to-front, i.e. ascending elevation).
    pub fn set_overlays(&mut self, quads: &[OverlayQuad]) {
        let mut verts: Vec<f32> = Vec::with_capacity(quads.len() * 6 * 7);
        for q in quads {
            let (e, z) = (q.extent, q.elevation);
            let corners = [
                [-e, -e], [e, -e], [e, e], // triangle 1
                [-e, -e], [e, e], [-e, e], // triangle 2
            ];
            for [x, y] in corners {
                verts.extend_from_slice(&[x, y, z]);
                verts.extend_from_slice(&q.color);
            }
        }
        self.overlay = self.vertex_buf(&verts, "level overlays");
    }

    /// Replace one overlay line layer (interleaved pos3 + rgba4 line-list
    /// vertices in world coordinates; empty clears it).
    pub fn set_lines(&mut self, layer: LineLayer, verts: &[f32]) {
        let buf = self.vertex_buf(verts, "overlay lines");
        match layer {
            LineLayer::Grid => self.grid_lines = buf,
            LineLayer::Plain => self.plain_lines = buf,
        }
    }

    /// Replace the draw-tool preview geometry (interleaved pos3+rgba4
    /// vertices in world coordinates; empty slices clear it). Triangles
    /// are point markers; lines are the rubber-band polyline.
    pub fn set_preview(&mut self, tris: &[f32], lines: &[f32]) {
        self.preview_tris = self.vertex_buf(tris, "preview tris");
        self.preview_lines = self.vertex_buf(lines, "preview lines");
    }

    /// World-space AABB of the drawn scene (meshes x instance
    /// transforms; overlays excluded). `None` when nothing is drawn.
    pub fn scene_bbox(&self) -> Option<([f64; 3], [f64; 3])> {
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        let mut any = false;
        for (id, m) in self.draw_list() {
            let Some(mesh) = self.meshes.get(&id) else { continue };
            if mesh.index_count == 0 {
                continue;
            }
            any = true;
            for cx in [mesh.bbox_min[0], mesh.bbox_max[0]] {
                for cy in [mesh.bbox_min[1], mesh.bbox_max[1]] {
                    for cz in [mesh.bbox_min[2], mesh.bbox_max[2]] {
                        let world = m * glam::Vec4::new(cx, cy, cz, 1.0);
                        let w = [f64::from(world.x), f64::from(world.y), f64::from(world.z)];
                        for axis in 0..3 {
                            min[axis] = min[axis].min(w[axis]);
                            max[axis] = max[axis].max(w[axis]);
                        }
                    }
                }
            }
        }
        any.then_some((min, max))
    }

    /// Draw list per the facade rule: one draw per instance of a mesh
    /// owner (`world = instance ∘ base`, base applied first); owners
    /// with no instances draw once at their base transform alone.
    fn draw_list(&self) -> Vec<(EntityId, Mat4)> {
        let mut draws: Vec<(EntityId, Mat4)> = Vec::new();
        for (id, mesh) in &self.meshes {
            let mut any = false;
            for (element, m) in self.instances.values() {
                if element == id {
                    draws.push((*id, *m * mesh.base));
                    any = true;
                }
            }
            if !any {
                draws.push((*id, mesh.base));
            }
        }
        draws
    }

    /// Total triangles that one frame draws (instanced meshes count once
    /// per instance).
    pub fn drawn_triangle_count(&self) -> u64 {
        self.draw_list()
            .iter()
            .filter_map(|(id, _)| self.meshes.get(id))
            .map(|m| u64::from(m.triangle_count))
            .sum()
    }

    pub fn render(&mut self, view_proj: Mat4) -> Result<(), String> {
        let draws = self.draw_list();

        // Grow the per-draw uniform buffer if needed.
        if draws.len() as u32 > self.model_capacity {
            self.model_capacity = (draws.len() as u32).next_power_of_two();
            let (buf, bind) =
                create_model_buffer(&self.device, &self.model_layout, self.model_capacity);
            self.model_buf = buf;
            self.model_bind = bind;
        }

        // Globals: view-proj, key light direction (world space) with the
        // gamma flag in .w (see the shader comment), wire color, grid
        // fade, edge nudge.
        let gamma_encode = !self.config.format.is_srgb();
        let mut globals = [0f32; 32];
        globals[..16].copy_from_slice(&view_proj.to_cols_array());
        globals[16..20].copy_from_slice(&[0.45, -0.55, 0.72, f32::from(gamma_encode)]);
        globals[20..24].copy_from_slice(&[0.04, 0.05, 0.07, 0.42]);
        globals[24..28].copy_from_slice(&self.fade);
        globals[28..32].copy_from_slice(&self.edge_eye);
        self.queue
            .write_buffer(&self.globals_buf, 0, &f32s_to_bytes(&globals));
        let mut any_edges = false;
        for (i, (id, m)) in draws.iter().enumerate() {
            let style = self.styles.get(id).copied().unwrap_or(self.default_style);
            any_edges |= style.edge[3] > 0.0;
            let mut data = [0f32; 24];
            data[..16].copy_from_slice(&m.to_cols_array());
            data[16..20].copy_from_slice(&style.tint);
            data[20..24].copy_from_slice(&style.edge);
            self.queue.write_buffer(
                &self.model_buf,
                i as u64 * MODEL_STRIDE,
                &f32s_to_bytes(&data),
            );
        }

        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.config);
                self.surface
                    .get_current_texture()
                    .map_err(|e| format!("surface unavailable after reconfigure: {e}"))?
            }
            Err(e) => return Err(format!("get_current_texture failed: {e}")),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        {
            let (target, resolve) = match &self.msaa_view {
                Some(msaa) => (msaa, Some(&view)),
                None => (&view, None),
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: resolve,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        // Clear values bypass the shader, so pre-encode
                        // them on non-sRGB surfaces to match.
                        load: wgpu::LoadOp::Clear(clear_color(self.clear, gamma_encode)),
                        store: if resolve.is_some() {
                            wgpu::StoreOp::Discard
                        } else {
                            wgpu::StoreOp::Store
                        },
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });

            pass.set_pipeline(&self.fill_pipeline);
            pass.set_bind_group(0, &self.globals_bind, &[]);
            for (i, (id, _)) in draws.iter().enumerate() {
                let Some(mesh) = self.meshes.get(id) else { continue };
                pass.set_bind_group(1, &self.model_bind, &[(i as u32) * MODEL_STRIDE as u32]);
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.index_count, 0, 0..1);
            }

            if self.wireframe {
                pass.set_pipeline(&self.wire_pipeline);
                for (i, (id, _)) in draws.iter().enumerate() {
                    let Some(mesh) = self.meshes.get(id) else { continue };
                    pass.set_bind_group(1, &self.model_bind, &[(i as u32) * MODEL_STRIDE as u32]);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.wire_indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.wire_index_count, 0, 0..1);
                }
            }

            if any_edges {
                pass.set_pipeline(&self.edge_pipeline);
                for (i, (id, _)) in draws.iter().enumerate() {
                    let Some(mesh) = self.meshes.get(id) else { continue };
                    if mesh.edge_index_count == 0 {
                        continue;
                    }
                    pass.set_bind_group(1, &self.model_bind, &[(i as u32) * MODEL_STRIDE as u32]);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.edge_indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.edge_index_count, 0, 0..1);
                }
            }

            // Overlays last: translucent geometry blended over the
            // opaque scene, depth-tested against it.
            for (pipeline, layer) in [
                (&self.overlay_pipeline, &self.overlay),
                (&self.grid_pipeline, &self.grid_lines),
                (&self.plain_line_pipeline, &self.plain_lines),
                // Draw-tool preview on top of everything (depth Always).
                (&self.preview_line_pipeline, &self.preview_lines),
                (&self.preview_tri_pipeline, &self.preview_tris),
            ] {
                if let Some(buf) = &layer.buf {
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(0, &self.globals_bind, &[]);
                    pass.set_vertex_buffer(0, buf.slice(..));
                    pass.draw(0..layer.count, 0..1);
                }
            }
        }
        self.queue.submit([encoder.finish()]);
        frame.present();
        Ok(())
    }
}

/// Background clear color from linear RGB; pre-encoded to sRGB when the
/// surface format will not encode it in hardware.
fn clear_color(linear: [f64; 3], gamma_encode: bool) -> wgpu::Color {
    let encode = |v: f64| {
        if gamma_encode { v.powf(1.0 / 2.2) } else { v }
    };
    wgpu::Color {
        r: encode(linear[0]),
        g: encode(linear[1]),
        b: encode(linear[2]),
        a: 1.0,
    }
}

fn create_depth(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    sample_count: u32,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

fn create_msaa(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    sample_count: u32,
) -> Option<wgpu::TextureView> {
    (sample_count > 1).then(|| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("msaa color"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default())
    })
}

fn create_model_buffer(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    capacity: u32,
) -> (wgpu::Buffer, wgpu::BindGroup) {
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("per-draw uniforms"),
        size: u64::from(capacity) * MODEL_STRIDE,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("per-draw bind"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &buf,
                offset: 0,
                size: wgpu::BufferSize::new(MODEL_SIZE),
            }),
        }],
    });
    (buf, bind)
}
