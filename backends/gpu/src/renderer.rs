//! A small wgpu renderer: instanced rounded quads (fills, gradients, images, inner and outer shadows)
//! and opacity layers. It renders in the target's encoded color space, as browsers blend, so
//! semi-transparent colors look the same as in CSS.
use bytemuck::{Pod, Zeroable};
use jsnir::{host::Corners, text::TextBitmap};
use std::{
    collections::HashMap,
    io::Write,
    ops::Range,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
};

pub type Rect = [f32; 4];
pub type Color = [u8; 4];
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const WHITE: Color = [255; 4];
const WHITE_TEX_ID: u64 = 1;
const ATLAS_TEX_ID: u64 = 2;
const FIRST_DYNAMIC_TEX_ID: u64 = 3;

/// Per-frame GPU rendering telemetry recorded by [`Renderer::draw`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderStats {
    pub quads: usize,
    pub draw_calls: usize,
    pub bind_group_switches: usize,
    pub scissor_switches: usize,
}

#[derive(Clone)]
pub struct Image {
    pub size: [usize; 2],
    uv: [f32; 4],
    texture_id: u64,
    bindings: wgpu::BindGroup,
}

/// One quad: a fill, an image or a shadow. Rectangles and lengths are in logical pixels.
#[derive(Clone)]
pub struct Shape {
    rect: Rect,
    // Cuts the quad to this rectangle, axis-aligned, with the scissor.
    clip: Rect,
    // Rounded rectangle and corner radii of an ancestor clip, anti-aliased; None for none.
    round_clip: Option<(Rect, Corners)>,
    color: Color,
    end_color: Color,
    uv: [f32; 4],
    radius: Corners,
    image: Option<Image>,
    // The mask's alpha, stretched to the rectangle it is paired with.
    mask: Option<(Image, Rect)>,
    mode: Mode,
}

#[derive(Clone, Copy)]
enum Mode {
    Fill,
    /// Inner shadow without blur: `spread` and `offset`.
    Inset {
        spread: f32,
        offset: [f32; 2],
    },
    /// Blurred outer shadow of the element `element`, with its own corner radius.
    Shadow {
        element: Rect,
        element_radius: f32,
        blur: f32,
        spread: f32,
        y: f32,
    },
}

/// A drawing command. Layers composite everything between `PushLayer` and `PopLayer` with an opacity.
// Shapes are built once per frame and copied into the instances: boxing them would allocate for each one.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum Draw {
    Shape(Shape),
    PushLayer {
        opacity: f32,
    },
    PopLayer,
    PushSurface {
        source: Rect,
        dest: Rect,
        radius: Corners,
        opacity: f32,
        clip: Rect,
        round_clip: Option<(Rect, Corners)>,
        additive: bool,
        cache_key: Option<(u64, u64)>,
    },
    PopSurface,
}

impl Draw {
    pub fn fill(rect: Rect, clip: Rect, color: Color) -> Self {
        Self::gradient(rect, clip, [0.; 4], color, color)
    }

    pub fn rounded(rect: Rect, clip: Rect, radius: f32, color: Color) -> Self {
        Self::gradient(rect, clip, [radius; 4], color, color)
    }

    /// Rounded rectangle with a vertical gradient from `top` to `bottom`.
    pub fn gradient(rect: Rect, clip: Rect, radius: Corners, top: Color, bottom: Color) -> Self {
        Draw::Shape(Shape {
            rect,
            clip,
            round_clip: None,
            color: top,
            end_color: bottom,
            uv: [0., 0., 1., 1.],
            radius,
            image: None,
            mask: None,
            mode: Mode::Fill,
        })
    }

    /// Cuts the shape to the rounded rectangle `round` (rectangle and corner radii, logical pixels), anti-aliased.
    pub fn rounded_clip(mut self, round: Option<(Rect, Corners)>) -> Self {
        if let Draw::Shape(shape) = &mut self {
            shape.round_clip = round;
        }
        self
    }

    /// Multiplies the shape by the alpha of the mask, stretched to its rectangle (logical pixels). Nothing shows outside it.
    pub fn masked(mut self, mask: Option<(Image, Rect)>) -> Self {
        if let Draw::Shape(shape) = &mut self {
            shape.mask = mask;
        }
        self
    }

    /// An image at its natural size, `scale` device pixels per logical pixel.
    pub fn image(image: &Image, [x, y]: [f32; 2], scale: f32, clip: Rect) -> Self {
        let [w, h] = image.size.map(|side| side as f32 / scale);
        Self::image_in(image, [x, y, w, h], [0.; 4], clip, WHITE)
    }

    /// An image stretched to `rect`, with corners rounded by `radius`, multiplied by `tint` (straight RGBA).
    pub fn image_in(image: &Image, rect: Rect, radius: Corners, clip: Rect, tint: Color) -> Self {
        Draw::Shape(Shape {
            rect,
            clip,
            round_clip: None,
            color: tint,
            end_color: tint,
            uv: image.uv,
            radius,
            image: Some(image.clone()),
            mask: None,
            mode: Mode::Fill,
        })
    }

    /// Inner shadow without blur: the part of the shape that the shape, shrunk by `spread` and moved by
    /// `offset`, does not cover (CSS `box-shadow: inset`).
    pub fn inset(
        rect: Rect,
        clip: Rect,
        radius: Corners,
        spread: f32,
        offset: [f32; 2],
        color: Color,
    ) -> Self {
        Draw::Shape(Shape {
            rect,
            clip,
            round_clip: None,
            color,
            end_color: color,
            uv: [0., 0., 1., 1.],
            radius,
            image: None,
            mask: None,
            mode: Mode::Inset { spread, offset },
        })
    }

    /// Outer shadow of a rounded rectangle, not drawn under the rectangle itself (CSS `box-shadow`).
    pub fn shadow(
        element: Rect,
        radius: f32,
        y: f32,
        blur: f32,
        spread: f32,
        color: Color,
        clip: Rect,
    ) -> Self {
        // The quad covers the blur: three standard deviations past the grown and moved element.
        let margin = 1.5 * blur + spread.max(0.) + y.abs();
        let rect = [
            element[0] - margin,
            element[1] - margin,
            element[2] + 2. * margin,
            element[3] + 2. * margin,
        ];
        Draw::Shape(Shape {
            rect,
            clip,
            round_clip: None,
            color,
            end_color: color,
            uv: [0., 0., 1., 1.],
            radius: [0.; 4],
            image: None,
            mask: None,
            mode: Mode::Shadow {
                element: [margin, margin, element[2], element[3]],
                element_radius: radius,
                blur,
                spread,
                y,
            },
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Quad {
    edges: [f32; 4],
    color: [f32; 4],
    end_color: [f32; 4],
    uv: [f32; 4],
    shape: [f32; 4],
    params: [f32; 4],
    params2: [f32; 4],
    // The rounded clip's rectangle: the scissor cuts the rectangle clip, so this slot needs no other.
    round_clip: [f32; 4],
    mask: [f32; 4],
    clip_radii: [f32; 4],
    radii: [f32; 4],
}

// Offscreen texture for one nesting level of layers.
struct Layer {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    bindings: wgpu::BindGroup,
    texture_id: u64,
    size: [u32; 2],
}

struct Shelf {
    y: u32,
    height: u32,
    x: u32,
}

// Persistent single-texture shelf atlas for text and icon bitmaps.
// Reserves `(0..4, 0..4)` permanently for an opaque white block so fills, shadows, insets,
// icons, and text share a single Group-0 bind group and coalesce into one instanced draw call.
struct Atlas {
    texture: wgpu::Texture,
    bindings: wgpu::BindGroup,
    width: u32,
    height: u32,
    white_uv: [f32; 4],
    shelves: Vec<Shelf>,
    next_y: u32,
    epoch: u64,
    staging: Vec<u8>,
}

/// Result of trying to pack a bitmap into [`Renderer`]'s persistent texture atlas.
pub enum AtlasAlloc {
    /// Packed into the atlas; shares the atlas bind group with fills, icons, and other text.
    Packed(Image),
    /// The atlas has no free shelf space left; caller should fall back to standalone for this frame
    /// and call [`Renderer::clear_atlas`] before emitting draws on the next frame.
    Full,
    /// The bitmap is larger than the atlas tile threshold; use a standalone texture without clearing.
    Oversized,
}

impl Atlas {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
    ) -> Self {
        let limit = device.limits().max_texture_dimension_2d;
        // Wide aspect (up to 4096x2048 = 32 MB) accommodates full-width HiDPI lines while staying
        // within downlevel 2048x2048 limits where needed.
        let width = limit.min(4096);
        let height = limit.min(2048);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("JSNIR atlas"),
            size: extent([width, height]),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // Upload a 4x4 opaque white block at `(0..4, 0..4)` sampled at its interior center `(1.5, 1.5)`.
        let white_block = [255u8; 4 * 4 * 4];
        queue.write_texture(
            texture.as_image_copy(),
            &white_block,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * 4),
                rows_per_image: None,
            },
            extent([4, 4]),
        );
        let bindings = Renderer::bind(
            device,
            layout,
            sampler,
            &texture.create_view(&Default::default()),
        );
        let cx = 1.5 / width as f32;
        let cy = 1.5 / height as f32;
        Self {
            texture,
            bindings,
            width,
            height,
            white_uv: [cx, cy, cx, cy],
            shelves: Vec::new(),
            next_y: 4,
            epoch: 1,
            staging: Vec::new(),
        }
    }

    fn clear(&mut self) {
        self.shelves.clear();
        self.next_y = 4;
        self.epoch = self.epoch.wrapping_add(1).max(1);
    }

    fn allocate(&mut self, queue: &wgpu::Queue, bitmap: &TextBitmap) -> AtlasAlloc {
        let Ok(w) = u32::try_from(bitmap.size[0]) else {
            return AtlasAlloc::Oversized;
        };
        let Ok(h) = u32::try_from(bitmap.size[1]) else {
            return AtlasAlloc::Oversized;
        };
        if w == 0 || h == 0 {
            return AtlasAlloc::Oversized;
        }
        // 1px transparent gutter on all four sides prevents bleed across adjacent or recycled tiles.
        let pw = w + 2;
        let ph = h + 2;
        let max_area = (u64::from(self.width) * u64::from(self.height)) / 4;
        if pw > self.width || ph > self.height / 2 || u64::from(pw) * u64::from(ph) > max_area {
            return AtlasAlloc::Oversized;
        }
        let mut best: Option<(usize, u32)> = None;
        for (idx, shelf) in self.shelves.iter().enumerate() {
            if shelf.height >= ph
                && shelf.height <= ph + (ph / 2).max(16)
                && shelf.x + pw <= self.width
            {
                let waste = shelf.height - ph;
                if best.is_none_or(|(_, best_waste)| waste < best_waste) {
                    best = Some((idx, waste));
                }
            }
        }
        let (slot_x, slot_y) = if let Some((idx, _)) = best {
            let shelf = &mut self.shelves[idx];
            let pos = (shelf.x, shelf.y);
            shelf.x += pw;
            pos
        } else {
            let shelf_h = ph.max(16);
            if self.next_y + shelf_h > self.height {
                return AtlasAlloc::Full;
            }
            let y = self.next_y;
            self.next_y += shelf_h;
            self.shelves.push(Shelf {
                y,
                height: shelf_h,
                x: pw,
            });
            (0, y)
        };

        let row_bytes = (pw * 4) as usize;
        let total_bytes = row_bytes * ph as usize;
        if self.staging.len() < total_bytes {
            self.staging.resize(total_bytes, 0);
        }
        self.staging[..row_bytes].fill(0);
        self.staging[(ph as usize - 1) * row_bytes..total_bytes].fill(0);
        let src_row_bytes = (w * 4) as usize;
        for y in 0..h as usize {
            let dst_start = (y + 1) * row_bytes;
            let dst = &mut self.staging[dst_start..dst_start + row_bytes];
            dst[0..4].fill(0);
            dst[4..4 + src_row_bytes]
                .copy_from_slice(&bitmap.rgba[y * src_row_bytes..(y + 1) * src_row_bytes]);
            dst[4 + src_row_bytes..row_bytes].fill(0);
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: slot_x,
                    y: slot_y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &self.staging[..total_bytes],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pw * 4),
                rows_per_image: None,
            },
            extent([pw, ph]),
        );

        let inv_w = 1.0 / self.width as f32;
        let inv_h = 1.0 / self.height as f32;
        let ox = (slot_x + 1) as f32;
        let oy = (slot_y + 1) as f32;
        AtlasAlloc::Packed(Image {
            size: bitmap.size,
            uv: [
                ox * inv_w,
                oy * inv_h,
                (ox + w as f32) * inv_w,
                (oy + h as f32) * inv_h,
            ],
            texture_id: ATLAS_TEX_ID,
            bindings: self.bindings.clone(),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TargetRef {
    Root,
    Layer(usize),
    TransientSurface(usize),
    CachedSurface((u64, u64)),
}

struct CachedSurface {
    layer: Layer,
    last_used: u64,
}

enum BatchCmd {
    Draw {
        instances: Range<u32>,
        scissor: [u32; 4],
        image_id: u64,
        // `u32::MAX` => `&self.atlas.bindings`; otherwise index into `draws` whose shape holds `image`.
        image_draw: u32,
        mask_id: u64,
        // `u32::MAX` => `&self.white.bindings`; otherwise index into `draws` whose shape holds `mask`.
        mask_draw: u32,
    },
    Composite {
        source: TargetRef,
        instance: u32,
        scissor: [u32; 4],
        additive: bool,
    },
    PushTarget {
        target: TargetRef,
    },
    PopTo {
        target: TargetRef,
        source: TargetRef,
        composite: Option<(u32, [u32; 4], bool)>,
    },
}

pub struct Renderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    additive_pipeline: wgpu::RenderPipeline,
    // Shared by the image (group 0) and the mask (group 1) of every quad.
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    format: wgpu::TextureFormat,
    white: Image,
    atlas: Atlas,
    next_texture_id: AtomicU64,
    instances: wgpu::Buffer,
    layers: Vec<Layer>,
    surfaces: Vec<Layer>,
    surface_cache: HashMap<(u64, u64), CachedSurface>,
    surface_frame_id: u64,
    // Reusable per-frame scratch buffers to avoid heap allocations during `draw`.
    quads: Vec<Quad>,
    batches: Vec<BatchCmd>,
    opacities: Vec<f32>,
    last_stats: RenderStats,
}

impl Renderer {
    /// `format` must be a format without the sRGB suffix: colors are written already encoded.
    pub fn new(adapter: &wgpu::Adapter, format: wgpu::TextureFormat) -> Result<Self> {
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("JSNIR"),
                required_limits:
                    wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
                ..Default::default()
            }))?;
        let shader = device.create_shader_module(wgpu::include_wgsl!("quad.wgsl"));
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("JSNIR image"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Quads"),
            // Group 0 is the image, group 1 the mask: both use the same layout.
            bind_group_layouts: &[Some(&layout), Some(&layout)],
            immediate_size: 0,
        });
        let make_pipeline = |label: &str, blend: wgpu::BlendState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex"),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: size_of::<Quad>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![
                            0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4,
                            4 => Float32x4, 5 => Float32x4, 6 => Float32x4, 7 => Float32x4,
                            8 => Float32x4, 9 => Float32x4, 10 => Float32x4
                        ],
                    })],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fragment"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipeline = make_pipeline("Quads", wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING);
        let additive_pipeline = make_pipeline(
            "Quads additive",
            wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
            },
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let white = Self::upload(
            &device,
            &queue,
            &layout,
            &sampler,
            [1, 1],
            &WHITE,
            WHITE_TEX_ID,
        );
        let atlas = Atlas::new(&device, &queue, &layout, &sampler);
        let instances = Self::buffer(&device, 64);
        Ok(Self {
            device,
            queue,
            pipeline,
            additive_pipeline,
            layout,
            sampler,
            format,
            white,
            atlas,
            next_texture_id: AtomicU64::new(FIRST_DYNAMIC_TEX_ID),
            instances,
            layers: Vec::new(),
            surfaces: Vec::new(),
            surface_cache: HashMap::new(),
            surface_frame_id: 0,
            quads: Vec::new(),
            batches: Vec::new(),
            opacities: Vec::new(),
            last_stats: RenderStats::default(),
        })
    }

    pub fn last_stats(&self) -> RenderStats {
        self.last_stats
    }

    pub fn atlas_epoch(&self) -> u64 {
        self.atlas.epoch
    }

    /// Resets the shelf allocator in the persistent texture atlas without reallocating GPU memory.
    pub fn clear_atlas(&mut self) {
        self.atlas.clear();
    }

    /// Packs a bitmap into the persistent texture atlas so it batches with fills, icons, and other text.
    pub fn atlas_image(&mut self, bitmap: &TextBitmap) -> AtlasAlloc {
        self.atlas.allocate(&self.queue, bitmap)
    }

    fn buffer(device: &wgpu::Device, count: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Scene quads"),
            size: (size_of::<Quad>() * count) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub fn image(&self, bitmap: TextBitmap) -> Image {
        let id = self.next_texture_id.fetch_add(1, Ordering::Relaxed);
        Self::upload(
            &self.device,
            &self.queue,
            &self.layout,
            &self.sampler,
            bitmap.size,
            &bitmap.rgba,
            id,
        )
    }

    /// An image from straight RGBA8 pixels. The texture is premultiplied, as the blend expects.
    pub fn rgba_image(&self, rgba: &[u8], size: [usize; 2]) -> Image {
        let mut premultiplied = vec![0u8; rgba.len()];
        for (dst, src) in premultiplied.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
            let a = u32::from(src[3]);
            dst[0] = ((u32::from(src[0]) * a + 127) / 255) as u8;
            dst[1] = ((u32::from(src[1]) * a + 127) / 255) as u8;
            dst[2] = ((u32::from(src[2]) * a + 127) / 255) as u8;
            dst[3] = src[3];
        }
        self.image(TextBitmap {
            size,
            rgba: premultiplied,
        })
    }

    fn bind(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        view: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }

    fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        size: [usize; 2],
        rgba: &[u8],
        texture_id: u64,
    ) -> Image {
        let pixels = extent(size.map(|side| side as u32));
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("JSNIR image"),
            size: pixels,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pixels.width * 4),
                rows_per_image: None,
            },
            pixels,
        );
        let bindings = Self::bind(
            device,
            layout,
            sampler,
            &texture.create_view(&Default::default()),
        );
        Image {
            size,
            uv: [0., 0., 1., 1.],
            texture_id,
            bindings,
        }
    }

    pub fn has_cached_surface(&self, key: (u64, u64), size: [u32; 2]) -> bool {
        self.surface_cache
            .get(&key)
            .is_some_and(|entry| entry.layer.size == size)
    }

    pub fn cached_surface_count(&self) -> usize {
        self.surface_cache.len() + self.surfaces.len()
    }

    fn create_layer(&self, label: &str, size: [u32; 2]) -> Layer {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: extent(size),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let bindings = Self::bind(&self.device, &self.layout, &self.sampler, &view);
        let texture_id = self.next_texture_id.fetch_add(1, Ordering::Relaxed);
        Layer {
            _texture: texture,
            view,
            bindings,
            texture_id,
            size,
        }
    }

    fn acquire_pool_layer(
        pool: &mut Vec<Layer>,
        active: &[bool],
        touched: &mut Vec<bool>,
        size: [u32; 2],
        create: impl FnOnce([u32; 2]) -> Layer,
    ) -> usize {
        if touched.len() < pool.len() {
            touched.resize(pool.len(), false);
        }
        // Prefer an inactive slot that already has the exact size (reusable even within one frame).
        for (i, layer) in pool.iter().enumerate() {
            if !active.get(i).copied().unwrap_or(false) && layer.size == size {
                touched[i] = true;
                return i;
            }
        }
        // Otherwise repurpose a slot not yet used in this frame's command buffer.
        for i in 0..pool.len() {
            if !touched[i] && !active.get(i).copied().unwrap_or(false) {
                pool[i] = create(size);
                touched[i] = true;
                return i;
            }
        }
        let idx = pool.len();
        pool.push(create(size));
        touched.push(true);
        idx
    }

    /// Draws into `target`, viewed in the renderer's format.
    pub fn draw(&mut self, target: &wgpu::Texture, size: [u32; 2], scale: f32, draws: &[Draw]) {
        self.surface_frame_id = self.surface_frame_id.wrapping_add(1).max(1);
        let frame_id = self.surface_frame_id;
        let max_dim = self.device.limits().max_texture_dimension_2d;
        let white_uv = self.atlas.white_uv;
        let build_quad = |shape: &Shape, uv: [f32; 4], origin: [f32; 2], tsize: [u32; 2]| -> Quad {
            let [px, py] = tsize.map(|side| side as f32 / scale);
            let inv_px = 2. / px;
            let inv_py = 2. / py;
            let [sx, sy, w, h] = shape.rect;
            let x = sx - origin[0];
            let y = sy - origin[1];
            let premultiply = |rgba: Color| {
                let a = f32::from(rgba[3]) * (1. / 255.);
                let s = a * (1. / 255.);
                [
                    f32::from(rgba[0]) * s,
                    f32::from(rgba[1]) * s,
                    f32::from(rgba[2]) * s,
                    a,
                ]
            };
            let (mode, params, params2) = match shape.mode {
                Mode::Fill => (0., [0.; 4], [0.; 4]),
                Mode::Inset { spread, offset } => (
                    1.,
                    [spread * scale, offset[0] * scale, offset[1] * scale, 0.],
                    [0.; 4],
                ),
                Mode::Shadow {
                    element,
                    element_radius,
                    blur,
                    spread,
                    y,
                } => (
                    2.,
                    element.map(|v| v * scale),
                    [
                        blur * 0.5 * scale,
                        spread * scale,
                        y * scale,
                        element_radius * scale,
                    ],
                ),
            };
            // The mask rectangle relative to the quad; the whole quad without a mask (white texture).
            let mask = match &shape.mask {
                Some((_, [mx, my, mw, mh])) => {
                    [(mx - sx) * scale, (my - sy) * scale, mw * scale, mh * scale]
                }
                None => [0., 0., w * scale, h * scale],
            };
            Quad {
                edges: [
                    x * inv_px - 1.,
                    1. - y * inv_py,
                    (x + w) * inv_px - 1.,
                    1. - (y + h) * inv_py,
                ],
                color: premultiply(shape.color),
                end_color: premultiply(shape.end_color),
                uv,
                shape: [w * scale, h * scale, 0., mode],
                params,
                params2,
                round_clip: shape.round_clip.map_or([0.; 4], |(rect, _)| {
                    [
                        (rect[0] - origin[0]) * scale,
                        (rect[1] - origin[1]) * scale,
                        rect[2] * scale,
                        rect[3] * scale,
                    ]
                }),
                mask,
                clip_radii: shape
                    .round_clip
                    .map_or([0.; 4], |(_, radii)| radii.map(|r| r * scale)),
                radii: shape.radius.map(|r| r * scale),
            }
        };

        #[derive(Clone, Copy)]
        struct TargetFrame {
            target: TargetRef,
            origin: [f32; 2],
            size: [u32; 2],
        }
        struct PendingSurface {
            source: TargetRef,
            dest: Rect,
            radius: Corners,
            opacity: f32,
            clip: Rect,
            round_clip: Option<(Rect, Corners)>,
            additive: bool,
            uv_max: [f32; 2],
        }

        self.quads.clear();
        self.batches.clear();
        self.opacities.clear();

        let mut target_stack = vec![TargetFrame {
            target: TargetRef::Root,
            origin: [0., 0.],
            size,
        }];
        let mut layer_active = vec![false; self.layers.len()];
        let mut layer_touched = vec![false; self.layers.len()];
        let mut surface_active = vec![false; self.surfaces.len()];
        let mut surface_touched = vec![false; self.surfaces.len()];
        let mut surface_pending: Vec<PendingSurface> = Vec::new();
        let mut skip_surface_depth = 0usize;
        let mut used_transient_surface = false;

        let shift_clip = |clip: Rect, origin: [f32; 2]| -> Rect {
            [clip[0] - origin[0], clip[1] - origin[1], clip[2], clip[3]]
        };

        for (draw_idx, draw) in draws.iter().enumerate() {
            if skip_surface_depth > 0 {
                match draw {
                    Draw::PushSurface { .. } => skip_surface_depth += 1,
                    Draw::PopSurface => skip_surface_depth -= 1,
                    _ => {}
                }
                continue;
            }
            match draw {
                Draw::Shape(shape) => {
                    if shape.rect[2] <= 0. || shape.rect[3] <= 0. {
                        continue;
                    }
                    let cur = *target_stack.last().unwrap();
                    if let Some(scissor) = scissor(shift_clip(shape.clip, cur.origin), cur.size, scale)
                    {
                        let quad_idx = self.quads.len() as u32;
                        let (image_id, image_draw, uv) = match &shape.image {
                            Some(img) => (img.texture_id, draw_idx as u32, shape.uv),
                            None => (ATLAS_TEX_ID, u32::MAX, white_uv),
                        };
                        let (mask_id, mask_draw) = match &shape.mask {
                            Some((img, _)) => (img.texture_id, draw_idx as u32),
                            None => (WHITE_TEX_ID, u32::MAX),
                        };
                        self.quads
                            .push(build_quad(shape, uv, cur.origin, cur.size));
                        if let Some(BatchCmd::Draw {
                            instances,
                            scissor: prev_scissor,
                            image_id: prev_img,
                            mask_id: prev_mask,
                            ..
                        }) = self.batches.last_mut()
                            && *prev_scissor == scissor
                            && *prev_img == image_id
                            && *prev_mask == mask_id
                            && instances.end == quad_idx
                        {
                            instances.end = quad_idx + 1;
                        } else {
                            self.batches.push(BatchCmd::Draw {
                                instances: quad_idx..quad_idx + 1,
                                scissor,
                                image_id,
                                image_draw,
                                mask_id,
                                mask_draw,
                            });
                        }
                    }
                }
                Draw::PushLayer { opacity } => {
                    self.opacities.push(*opacity);
                    let cur = *target_stack.last().unwrap();
                    let device = &self.device;
                    let layout = &self.layout;
                    let sampler = &self.sampler;
                    let format = self.format;
                    let next_id = &self.next_texture_id;
                    let slot = Self::acquire_pool_layer(
                        &mut self.layers,
                        &layer_active,
                        &mut layer_touched,
                        cur.size,
                        |s| {
                            let texture = device.create_texture(&wgpu::TextureDescriptor {
                                label: Some("JSNIR layer"),
                                size: extent(s),
                                mip_level_count: 1,
                                sample_count: 1,
                                dimension: wgpu::TextureDimension::D2,
                                format,
                                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                                    | wgpu::TextureUsages::TEXTURE_BINDING,
                                view_formats: &[],
                            });
                            let view = texture.create_view(&Default::default());
                            let bindings = Self::bind(device, layout, sampler, &view);
                            let texture_id = next_id.fetch_add(1, Ordering::Relaxed);
                            Layer {
                                _texture: texture,
                                view,
                                bindings,
                                texture_id,
                                size: s,
                            }
                        },
                    );
                    if layer_active.len() <= slot {
                        layer_active.resize(slot + 1, false);
                    }
                    layer_active[slot] = true;
                    let target_ref = TargetRef::Layer(slot);
                    target_stack.push(TargetFrame {
                        target: target_ref,
                        origin: cur.origin,
                        size: cur.size,
                    });
                    self.batches.push(BatchCmd::PushTarget { target: target_ref });
                }
                Draw::PopLayer => {
                    let opacity = self.opacities.pop().unwrap_or(1.).clamp(0., 1.);
                    let popped = target_stack.pop().unwrap_or(TargetFrame {
                        target: TargetRef::Root,
                        origin: [0., 0.],
                        size,
                    });
                    if let TargetRef::Layer(slot) = popped.target {
                        layer_active[slot] = false;
                    }
                    let parent = *target_stack.last().unwrap();
                    let [px, py] = parent.size.map(|side| side as f32 / scale);
                    let full = [parent.origin[0], parent.origin[1], px, py];
                    let tint = [255, 255, 255, (opacity * 255.).round() as u8];
                    let instance = self.quads.len() as u32;
                    self.quads.push(build_quad(
                        &Shape {
                            rect: full,
                            clip: full,
                            round_clip: None,
                            color: tint,
                            end_color: tint,
                            uv: [0., 0., 1., 1.],
                            radius: [0.; 4],
                            image: None,
                            mask: None,
                            mode: Mode::Fill,
                        },
                        [0., 0., 1., 1.],
                        parent.origin,
                        parent.size,
                    ));
                    self.batches.push(BatchCmd::PopTo {
                        target: parent.target,
                        source: popped.target,
                        composite: Some((instance, [0, 0, parent.size[0], parent.size[1]], false)),
                    });
                }
                Draw::PushSurface {
                    source,
                    dest,
                    radius,
                    opacity,
                    clip,
                    round_clip,
                    additive,
                    cache_key,
                } => {
                    let surf_px = [
                        (source[2] * scale).ceil().max(1.) as u32,
                        (source[3] * scale).ceil().max(1.) as u32,
                    ];
                    if surf_px[0] > max_dim || surf_px[1] > max_dim {
                        skip_surface_depth = 1;
                        continue;
                    }
                    let uv_max = [
                        ((source[2] * scale) / surf_px[0] as f32).min(1.),
                        ((source[3] * scale) / surf_px[1] as f32).min(1.),
                    ];
                    let cur = *target_stack.last().unwrap();
                    if let Some(key) = cache_key
                        && let Some(entry) = self.surface_cache.get_mut(key)
                        && entry.layer.size == surf_px
                    {
                        entry.last_used = frame_id;
                        let alpha = opacity.clamp(0., 1.);
                        if alpha > 0.001
                            && dest[2] > 0.
                            && dest[3] > 0.
                            && let Some(sc) =
                                scissor(shift_clip(*clip, cur.origin), cur.size, scale)
                        {
                            let tint = [255, 255, 255, (alpha * 255.).round() as u8];
                            let instance = self.quads.len() as u32;
                            self.quads.push(build_quad(
                                &Shape {
                                    rect: *dest,
                                    clip: *clip,
                                    round_clip: *round_clip,
                                    color: tint,
                                    end_color: tint,
                                    uv: [0., 0., uv_max[0], uv_max[1]],
                                    radius: *radius,
                                    image: None,
                                    mask: None,
                                    mode: Mode::Fill,
                                },
                                [0., 0., uv_max[0], uv_max[1]],
                                cur.origin,
                                cur.size,
                            ));
                            self.batches.push(BatchCmd::Composite {
                                source: TargetRef::CachedSurface(*key),
                                instance,
                                scissor: sc,
                                additive: *additive,
                            });
                        }
                        skip_surface_depth = 1;
                        continue;
                    }
                    let surf_ref = if let Some(key) = *cache_key {
                        let layer = self.create_layer("JSNIR cached surface", surf_px);
                        self.surface_cache.insert(
                            key,
                            CachedSurface {
                                layer,
                                last_used: frame_id,
                            },
                        );
                        TargetRef::CachedSurface(key)
                    } else {
                        used_transient_surface = true;
                        let device = &self.device;
                        let layout = &self.layout;
                        let sampler = &self.sampler;
                        let format = self.format;
                        let next_id = &self.next_texture_id;
                        let slot = Self::acquire_pool_layer(
                            &mut self.surfaces,
                            &surface_active,
                            &mut surface_touched,
                            surf_px,
                            |s| {
                                let texture = device.create_texture(&wgpu::TextureDescriptor {
                                    label: Some("JSNIR surface"),
                                    size: extent(s),
                                    mip_level_count: 1,
                                    sample_count: 1,
                                    dimension: wgpu::TextureDimension::D2,
                                    format,
                                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                                        | wgpu::TextureUsages::TEXTURE_BINDING,
                                    view_formats: &[],
                                });
                                let view = texture.create_view(&Default::default());
                                let bindings = Self::bind(device, layout, sampler, &view);
                                let texture_id = next_id.fetch_add(1, Ordering::Relaxed);
                                Layer {
                                    _texture: texture,
                                    view,
                                    bindings,
                                    texture_id,
                                    size: s,
                                }
                            },
                        );
                        if surface_active.len() <= slot {
                            surface_active.resize(slot + 1, false);
                        }
                        surface_active[slot] = true;
                        TargetRef::TransientSurface(slot)
                    };
                    target_stack.push(TargetFrame {
                        target: surf_ref,
                        origin: [source[0], source[1]],
                        size: surf_px,
                    });
                    surface_pending.push(PendingSurface {
                        source: surf_ref,
                        dest: *dest,
                        radius: *radius,
                        opacity: *opacity,
                        clip: *clip,
                        round_clip: *round_clip,
                        additive: *additive,
                        uv_max,
                    });
                    self.batches.push(BatchCmd::PushTarget { target: surf_ref });
                }
                Draw::PopSurface => {
                    let Some(pending) = surface_pending.pop() else {
                        continue;
                    };
                    let popped = target_stack.pop().unwrap_or(TargetFrame {
                        target: TargetRef::Root,
                        origin: [0., 0.],
                        size,
                    });
                    if let TargetRef::TransientSurface(slot) = popped.target {
                        surface_active[slot] = false;
                    }
                    let parent = *target_stack.last().unwrap();
                    let alpha = pending.opacity.clamp(0., 1.);
                    let composite = if alpha > 0.001
                        && pending.dest[2] > 0.
                        && pending.dest[3] > 0.
                        && let Some(sc) =
                            scissor(shift_clip(pending.clip, parent.origin), parent.size, scale)
                    {
                        let tint = [255, 255, 255, (alpha * 255.).round() as u8];
                        let instance = self.quads.len() as u32;
                        self.quads.push(build_quad(
                            &Shape {
                                rect: pending.dest,
                                clip: pending.clip,
                                round_clip: pending.round_clip,
                                color: tint,
                                end_color: tint,
                                uv: [0., 0., pending.uv_max[0], pending.uv_max[1]],
                                radius: pending.radius,
                                image: None,
                                mask: None,
                                mode: Mode::Fill,
                            },
                            [0., 0., pending.uv_max[0], pending.uv_max[1]],
                            parent.origin,
                            parent.size,
                        ));
                        Some((instance, sc, pending.additive))
                    } else {
                        None
                    };
                    self.batches.push(BatchCmd::PopTo {
                        target: parent.target,
                        source: pending.source,
                        composite,
                    });
                }
            }
        }
        let bytes = bytemuck::cast_slice(&self.quads);
        if bytes.len() as u64 > self.instances.size() {
            self.instances = Self::buffer(&self.device, self.quads.len().next_power_of_two());
        }
        if !bytes.is_empty() {
            self.queue.write_buffer(&self.instances, 0, bytes);
        }
        let target_view = target.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.format),
            ..Default::default()
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        // Each layer/surface renders in its own pass; popping composites it into the target below.
        let begin = |encoder: &mut wgpu::CommandEncoder,
                     view: &wgpu::TextureView,
                     clear: Option<wgpu::Color>| {
            let load = clear.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear);
            encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                })
                .forget_lifetime()
        };
        let layers = &self.layers;
        let surfaces = &self.surfaces;
        let surface_cache = &self.surface_cache;
        let view_of = |t: TargetRef| -> &wgpu::TextureView {
            match t {
                TargetRef::Root => &target_view,
                TargetRef::Layer(i) => &layers[i].view,
                TargetRef::TransientSurface(i) => &surfaces[i].view,
                TargetRef::CachedSurface(k) => &surface_cache[&k].layer.view,
            }
        };
        let layer_of = |t: TargetRef| -> &Layer {
            match t {
                TargetRef::Layer(i) => &layers[i],
                TargetRef::TransientSurface(i) => &surfaces[i],
                TargetRef::CachedSurface(k) => &surface_cache[&k].layer,
                TargetRef::Root => unreachable!("cannot sample root target"),
            }
        };
        let mut pass = begin(&mut encoder, &target_view, Some(wgpu::Color::BLACK));
        pass.set_pipeline(&self.pipeline);
        let stride = size_of::<Quad>() as u64;
        let mut cur_scissor: Option<[u32; 4]> = None;
        let mut cur_img: Option<u64> = None;
        let mut cur_mask: Option<u64> = None;
        let mut cur_additive = false;
        let mut stats = RenderStats {
            quads: self.quads.len(),
            ..Default::default()
        };
        for cmd in &self.batches {
            match cmd {
                BatchCmd::Draw {
                    instances,
                    scissor: rect,
                    image_id,
                    image_draw,
                    mask_id,
                    mask_draw,
                } => {
                    if cur_additive {
                        pass.set_pipeline(&self.pipeline);
                        cur_additive = false;
                    }
                    let start = u64::from(instances.start) * stride;
                    let end = u64::from(instances.end) * stride;
                    let count = instances.end - instances.start;
                    pass.set_vertex_buffer(0, self.instances.slice(start..end));
                    if cur_scissor != Some(*rect) {
                        pass.set_scissor_rect(rect[0], rect[1], rect[2], rect[3]);
                        cur_scissor = Some(*rect);
                        stats.scissor_switches += 1;
                    }
                    if cur_img != Some(*image_id) {
                        let bg = if *image_draw == u32::MAX {
                            &self.atlas.bindings
                        } else {
                            let Draw::Shape(shape) = &draws[*image_draw as usize] else {
                                unreachable!()
                            };
                            &shape.image.as_ref().unwrap().bindings
                        };
                        pass.set_bind_group(0, bg, &[]);
                        cur_img = Some(*image_id);
                        stats.bind_group_switches += 1;
                    }
                    if cur_mask != Some(*mask_id) {
                        let bg = if *mask_draw == u32::MAX {
                            &self.white.bindings
                        } else {
                            let Draw::Shape(shape) = &draws[*mask_draw as usize] else {
                                unreachable!()
                            };
                            &shape.mask.as_ref().unwrap().0.bindings
                        };
                        pass.set_bind_group(1, bg, &[]);
                        cur_mask = Some(*mask_id);
                        stats.bind_group_switches += 1;
                    }
                    pass.draw(0..4, 0..count);
                    stats.draw_calls += 1;
                }
                BatchCmd::Composite {
                    source,
                    instance,
                    scissor: rect,
                    additive,
                } => {
                    if cur_additive != *additive {
                        pass.set_pipeline(if *additive {
                            &self.additive_pipeline
                        } else {
                            &self.pipeline
                        });
                        cur_additive = *additive;
                    }
                    let src = layer_of(*source);
                    let offset = u64::from(*instance) * stride;
                    pass.set_vertex_buffer(0, self.instances.slice(offset..offset + stride));
                    if cur_scissor != Some(*rect) {
                        pass.set_scissor_rect(rect[0], rect[1], rect[2], rect[3]);
                        cur_scissor = Some(*rect);
                        stats.scissor_switches += 1;
                    }
                    if cur_img != Some(src.texture_id) {
                        pass.set_bind_group(0, &src.bindings, &[]);
                        cur_img = Some(src.texture_id);
                        stats.bind_group_switches += 1;
                    }
                    if cur_mask != Some(WHITE_TEX_ID) {
                        pass.set_bind_group(1, &self.white.bindings, &[]);
                        cur_mask = Some(WHITE_TEX_ID);
                        stats.bind_group_switches += 1;
                    }
                    pass.draw(0..4, 0..1);
                    stats.draw_calls += 1;
                }
                BatchCmd::PushTarget { target } => {
                    drop(pass);
                    pass = begin(&mut encoder, view_of(*target), Some(wgpu::Color::TRANSPARENT));
                    pass.set_pipeline(&self.pipeline);
                    cur_scissor = None;
                    cur_img = None;
                    cur_mask = None;
                    cur_additive = false;
                }
                BatchCmd::PopTo {
                    target,
                    source,
                    composite,
                } => {
                    drop(pass);
                    pass = begin(&mut encoder, view_of(*target), None);
                    if let Some((instance, rect, additive)) = *composite {
                        pass.set_pipeline(if additive {
                            &self.additive_pipeline
                        } else {
                            &self.pipeline
                        });
                        cur_additive = additive;
                        let src = layer_of(*source);
                        let offset = u64::from(instance) * stride;
                        pass.set_vertex_buffer(0, self.instances.slice(offset..offset + stride));
                        pass.set_scissor_rect(rect[0], rect[1], rect[2], rect[3]);
                        pass.set_bind_group(0, &src.bindings, &[]);
                        pass.set_bind_group(1, &self.white.bindings, &[]);
                        pass.draw(0..4, 0..1);
                        cur_scissor = Some(rect);
                        cur_img = Some(src.texture_id);
                        cur_mask = Some(WHITE_TEX_ID);
                        stats.draw_calls += 1;
                        stats.scissor_switches += 1;
                        stats.bind_group_switches += 2;
                    } else {
                        pass.set_pipeline(&self.pipeline);
                        cur_scissor = None;
                        cur_img = None;
                        cur_mask = None;
                        cur_additive = false;
                    }
                }
            }
        }
        drop(pass);
        self.last_stats = stats;
        self.queue.submit([encoder.finish()]);
        // Free cached resize textures no longer referenced in this frame and transient surfaces when idle.
        if !self.surface_cache.is_empty() {
            self.surface_cache.retain(|_, entry| entry.last_used == frame_id);
        }
        if !used_transient_surface && !self.surfaces.is_empty() {
            self.surfaces.clear();
        }
    }

    /// Draws offscreen and returns the pixels row by row, `stride` bytes apart, in the renderer's format.
    fn read(&mut self, size: [u32; 2], scale: f32, draws: &[Draw]) -> Result<(usize, Vec<u8>)> {
        let extent = extent(size);
        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("JSNIR capture"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        self.draw(&target, size, scale, draws);
        let [width, height] = size;
        let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let stride = (width * 4).div_ceil(alignment) * alignment;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Capture pixels"),
            size: u64::from(stride) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: None,
                },
            },
            extent,
        );
        self.queue.submit([encoder.finish()]);
        let (send, receive) = mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = send.send(result);
            });
        self.device.poll(wgpu::PollType::wait_indefinitely())?;
        receive.recv()??;
        let pixels = buffer.slice(..).get_mapped_range()?.to_vec();
        Ok((stride as usize, pixels))
    }

    /// Draws offscreen at `scale` device pixels per logical pixel and writes a binary PPM (`P6`) to a new file at `path`.
    pub fn capture(
        &mut self,
        size: [u32; 2],
        scale: f32,
        draws: &[Draw],
        path: &Path,
    ) -> Result<()> {
        let (stride, pixels) = self.read(size, scale, draws)?;
        let [width, height] = size;
        let bgra = matches!(self.format, wgpu::TextureFormat::Bgra8Unorm);
        let mut file = std::io::BufWriter::new(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?,
        );
        write!(file, "P6\n{width} {height}\n255\n")?;
        for row in pixels.chunks_exact(stride) {
            for pixel in row[..width as usize * 4].as_chunks::<4>().0 {
                let rgb = if bgra {
                    [pixel[2], pixel[1], pixel[0]]
                } else {
                    [pixel[0], pixel[1], pixel[2]]
                };
                file.write_all(&rgb)?;
            }
        }
        file.flush()?;
        Ok(())
    }
}

fn extent([width, height]: [u32; 2]) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}

fn scissor([x, y, w, h]: Rect, [width, height]: [u32; 2], scale: f32) -> Option<[u32; 4]> {
    let start = |v: f32, limit| ((v * scale).floor().max(0.) as u32).min(limit);
    let end = |v: f32, limit| ((v * scale).ceil().max(0.) as u32).min(limit);
    let (x0, x1) = (start(x, width), end(x + w, width));
    let (y0, y1) = (start(y, height), end(y + h, height));
    (x1 > x0 && y1 > y0).then(|| [x0, y0, x1 - x0, y1 - y0])
}

#[cfg(test)]
mod tests {
    use super::*;

    // The target is cleared to black, which is what an empty pixel shows.
    const BLACK: Color = [0, 0, 0, 255];
    const RED: Color = [255, 0, 0, 255];

    fn renderer() -> Renderer {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
            .expect("a GPU adapter");
        Renderer::new(&adapter, wgpu::TextureFormat::Rgba8Unorm).expect("a renderer")
    }

    // The pixel at (x, y) of a frame read back with `Renderer::read`.
    fn pixel((stride, bytes): &(usize, Vec<u8>), x: usize, y: usize) -> Color {
        let at = y * stride + x * 4;
        bytes[at..at + 4].try_into().unwrap()
    }

    fn assert_close(actual: Color, expected: Color) {
        assert!(
            actual.iter().zip(expected).all(|(a, e)| a.abs_diff(e) <= 2),
            "{actual:?} is not {expected:?}"
        );
    }

    #[test]
    fn a_mask_keeps_the_box_where_its_alpha_is_one() {
        let mut renderer = renderer();
        // Alpha 0 inside a 10 px circle at the bottom-right corner, 1 everywhere else.
        let rgba: Vec<u8> = (0..40 * 40)
            .flat_map(|i| {
                let (x, y) = ((i % 40) as f32 + 0.5, (i / 40) as f32 + 0.5);
                let inside = (x - 30.).powi(2) + (y - 30.).powi(2) < 100.;
                [255, 255, 255, if inside { 0 } else { 255 }]
            })
            .collect();
        let mask = renderer.rgba_image(&rgba, [40, 40]);
        let rect = [0., 0., 40., 40.];
        let draws = [Draw::gradient(rect, rect, [0.; 4], RED, RED).masked(Some((mask, rect)))];
        let frame = renderer.read([40, 40], 1., &draws).unwrap();
        assert_close(pixel(&frame, 30, 30), BLACK);
        assert_close(pixel(&frame, 20, 20), RED);
    }

    // Radii are top-left, top-right, bottom-right, bottom-left: the top-right and bottom-left corners are round.
    #[test]
    fn per_corner_radii_round_only_the_corners_they_name() {
        let mut renderer = renderer();
        let rect = [0., 0., 40., 40.];
        let corners = [0., 20., 0., 20.];
        let draws = [Draw::gradient(rect, rect, corners, RED, RED)];
        let frame = renderer.read([40, 40], 1., &draws).unwrap();
        assert_close(pixel(&frame, 1, 1), RED);
        assert_close(pixel(&frame, 38, 1), BLACK);
        assert_close(pixel(&frame, 1, 38), BLACK);
        assert_close(pixel(&frame, 38, 38), RED);

        // The same corners cut a square child of a clip.
        let draws =
            [Draw::gradient(rect, rect, [0.; 4], RED, RED).rounded_clip(Some((rect, corners)))];
        let frame = renderer.read([40, 40], 1., &draws).unwrap();
        assert_close(pixel(&frame, 1, 1), RED);
        assert_close(pixel(&frame, 38, 1), BLACK);
        assert_close(pixel(&frame, 1, 38), BLACK);
        assert_close(pixel(&frame, 38, 38), RED);
    }

    #[test]
    fn a_rounded_clip_cuts_the_corners_of_its_children() {
        let mut renderer = renderer();
        let rect = [0., 0., 40., 40.];
        // A 40 px red child in a clip of the same rectangle rounded by 20 px.
        let draws =
            [Draw::gradient(rect, rect, [0.; 4], RED, RED).rounded_clip(Some((rect, [20.; 4])))];
        let frame = renderer.read([40, 40], 1., &draws).unwrap();
        assert_close(pixel(&frame, 1, 1), BLACK);
        assert_close(pixel(&frame, 20, 20), RED);
    }

    #[test]
    fn a_square_child_of_a_rounded_clip_keeps_its_corners() {
        let mut renderer = renderer();
        // The parent is rounded by 20 px. Its child has a rectangular clip of its own, so its corner is
        // not rounded: only the parent's rounded box cuts it.
        let parent = [0., 0., 60., 60.];
        let child = [20., 20., 30., 30.];
        let draws =
            [Draw::gradient(child, child, [0.; 4], RED, RED)
                .rounded_clip(Some((parent, [20.; 4])))];
        let frame = renderer.read([60, 60], 1., &draws).unwrap();
        assert_close(pixel(&frame, 21, 21), RED);
        assert_close(pixel(&frame, 1, 1), BLACK);
    }

    #[test]
    fn atlas_tiles_and_fills_batch_into_a_single_draw_call_with_exact_pixels() {
        let mut renderer = renderer();
        let green = [0, 255, 0, 255];
        let blue = [0, 0, 255, 255];
        let AtlasAlloc::Packed(tile_a) = renderer.atlas_image(&TextBitmap {
            size: [10, 10],
            rgba: green.repeat(10 * 10),
        }) else {
            panic!("expected tile_a in atlas");
        };
        let AtlasAlloc::Packed(tile_b) = renderer.atlas_image(&TextBitmap {
            size: [10, 10],
            rgba: blue.repeat(10 * 10),
        }) else {
            panic!("expected tile_b in atlas");
        };
        let screen = [0., 0., 60., 40.];
        let draws = [
            Draw::fill(screen, screen, RED),
            Draw::image(&tile_a, [5., 5.], 1., screen),
            Draw::fill([20., 5., 10., 10.], screen, WHITE),
            Draw::image(&tile_b, [35., 5.], 1., screen),
        ];
        let frame = renderer.read([60, 40], 1., &draws).unwrap();
        let stats = renderer.last_stats();
        assert_eq!(stats.quads, 4);
        assert_eq!(
            stats.draw_calls, 1,
            "fills and atlas tiles with matching scissor must coalesce into 1 draw call"
        );
        assert_eq!(stats.bind_group_switches, 2);
        assert_eq!(stats.scissor_switches, 1);

        assert_close(pixel(&frame, 1, 1), RED);
        assert_close(pixel(&frame, 10, 10), green);
        assert_close(pixel(&frame, 25, 10), WHITE);
        assert_close(pixel(&frame, 40, 10), blue);
    }

    #[test]
    fn clearing_the_atlas_reuses_slots_without_bleeding_previous_epoch_pixels() {
        let mut renderer = renderer();
        let green = [0, 255, 0, 255];
        let epoch_before = renderer.atlas_epoch();
        let AtlasAlloc::Packed(_) = renderer.atlas_image(&TextBitmap {
            size: [16, 16],
            rgba: RED.repeat(16 * 16),
        }) else {
            panic!("expected initial atlas tile");
        };

        renderer.clear_atlas();
        assert_ne!(renderer.atlas_epoch(), epoch_before);
        let AtlasAlloc::Packed(small_green) = renderer.atlas_image(&TextBitmap {
            size: [8, 8],
            rgba: green.repeat(8 * 8),
        }) else {
            panic!("expected reused atlas tile");
        };

        let screen = [0., 0., 32., 32.];
        let draws = [Draw::image(&small_green, [8., 8.], 1., screen)];
        let frame = renderer.read([32, 32], 1., &draws).unwrap();
        // Edges of the 8x8 tile must be pure green with zero red bleed from the old 16x16 epoch.
        assert_close(pixel(&frame, 8, 8), green);
        assert_close(pixel(&frame, 15, 15), green);
        assert_close(pixel(&frame, 7, 8), BLACK);
        assert_close(pixel(&frame, 16, 15), BLACK);
    }

    #[test]
    fn batches_split_cleanly_across_standalone_images_scissors_and_layers() {
        let mut renderer = renderer();
        let green = [0, 255, 0, 255];
        let blue = [0, 0, 255, 255];
        let standalone = renderer.rgba_image(&blue.repeat(10 * 10), [10, 10]);
        let screen = [0., 0., 60., 40.];
        let draws = [
            Draw::fill([0., 0., 20., 20.], screen, RED),
            Draw::image(&standalone, [22., 0.], 1., screen),
            Draw::PushLayer { opacity: 0.5 },
            Draw::fill([40., 0., 20., 20.], screen, green),
            Draw::PopLayer,
        ];
        let frame = renderer.read([60, 40], 1., &draws).unwrap();
        let stats = renderer.last_stats();
        // 1 (RED fill) + 1 (standalone blue) + 1 (layer green) + 1 (PopLayer composite) = 4
        assert_eq!(stats.draw_calls, 4);
        assert_close(pixel(&frame, 5, 5), RED);
        assert_close(pixel(&frame, 26, 5), blue);
        assert_close(pixel(&frame, 45, 5), [0, 128, 0, 255]);
    }

    #[test]
    fn crossfade_surfaces_mix_without_background_bleed_and_free_textures_when_done() {
        let mut renderer = renderer();
        let green = [0, 255, 0, 255];
        let blue = [0, 0, 255, 255];
        let screen = [0., 0., 60., 60.];
        let prev_src = [10., 10., 20., 20.];
        let next_src = [10., 10., 40., 40.];
        let dest = [10., 10., 30., 30.];
        let draws = [
            Draw::fill(screen, screen, green),
            Draw::PushLayer { opacity: 1. },
            Draw::PushSurface {
                source: prev_src,
                dest,
                radius: [10.; 4],
                opacity: 0.5,
                clip: screen,
                round_clip: None,
                additive: false,
                cache_key: Some((1, 1)),
            },
            Draw::fill(prev_src, prev_src, RED),
            Draw::PopSurface,
            Draw::PushSurface {
                source: next_src,
                dest,
                radius: [10.; 4],
                opacity: 0.5,
                clip: screen,
                round_clip: None,
                additive: true,
                cache_key: None,
            },
            Draw::fill(next_src, next_src, blue),
            Draw::PopSurface,
            Draw::PopLayer,
        ];
        let frame = renderer.read([60, 60], 1., &draws).unwrap();
        // Exact mix(RED, BLUE, 0.5) = [128, 0, 128, 255] with zero green background bleed.
        assert_close(pixel(&frame, 25, 25), [128, 0, 128, 255]);
        // Rounded corner outside radius=10 shows green background.
        assert_close(pixel(&frame, 11, 11), green);
        assert!(renderer.has_cached_surface((1, 1), [20, 20]));
        assert!(renderer.cached_surface_count() >= 1);

        // Second frame reuses the cached prev surface even with empty inner draws, and applies group opacity on top.
        let draws_cached = [
            Draw::fill(screen, screen, green),
            Draw::PushLayer { opacity: 0.5 },
            Draw::PushSurface {
                source: prev_src,
                dest,
                radius: [0.; 4],
                opacity: 0.5,
                clip: screen,
                round_clip: None,
                additive: false,
                cache_key: Some((1, 1)),
            },
            Draw::PopSurface,
            Draw::PushSurface {
                source: next_src,
                dest,
                radius: [0.; 4],
                opacity: 0.5,
                clip: screen,
                round_clip: None,
                additive: true,
                cache_key: None,
            },
            Draw::fill(next_src, next_src, blue),
            Draw::PopSurface,
            Draw::PopLayer,
        ];
        let frame2 = renderer.read([60, 60], 1., &draws_cached).unwrap();
        // Group opacity 0.5 over green: 0.5 * [128, 0, 128] + 0.5 * [0, 255, 0] = [64, 128, 64].
        assert_close(pixel(&frame2, 25, 25), [64, 128, 64, 255]);

        // Frame without surfaces frees both cached and transient surface textures.
        let _ = renderer
            .read([60, 60], 1., &[Draw::fill(screen, screen, green)])
            .unwrap();
        assert_eq!(renderer.cached_surface_count(), 0);
    }
}
