//! A small wgpu renderer: instanced rounded quads (fills, gradients, images, inner and outer shadows)
//! and opacity layers. It renders in the target's encoded color space, as browsers blend, so
//! semi-transparent colors look the same as in CSS.
use bytemuck::{Pod, Zeroable};
use jsnir::text::TextBitmap;
use std::{io::Write, path::Path, sync::mpsc};

pub type Rect = [f32; 4];
pub type Color = [u8; 4];
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const WHITE: Color = [255; 4];

#[derive(Clone)]
pub struct Image {
    pub size: [usize; 2],
    bindings: wgpu::BindGroup,
}

/// One quad: a fill, an image or a shadow. Rectangles and lengths are in logical pixels.
#[derive(Clone)]
pub struct Shape {
    rect: Rect,
    // Cuts the quad to this rectangle, axis-aligned, with the scissor.
    clip: Rect,
    // Rounded rectangle and corner radius of an ancestor clip, anti-aliased; None for none.
    round_clip: Option<(Rect, f32)>,
    color: Color,
    end_color: Color,
    uv: [f32; 4],
    radius: f32,
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
#[derive(Clone)]
pub enum Draw {
    Shape(Shape),
    PushLayer { opacity: f32 },
    PopLayer,
}

impl Draw {
    pub fn fill(rect: Rect, clip: Rect, color: Color) -> Self {
        Self::gradient(rect, clip, 0., color, color)
    }

    pub fn rounded(rect: Rect, clip: Rect, radius: f32, color: Color) -> Self {
        Self::gradient(rect, clip, radius, color, color)
    }

    /// Rounded rectangle with a vertical gradient from `top` to `bottom`.
    pub fn gradient(rect: Rect, clip: Rect, radius: f32, top: Color, bottom: Color) -> Self {
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

    /// Cuts the shape to the rounded rectangle `round` (rectangle and corner radius, logical pixels), anti-aliased.
    pub fn rounded_clip(mut self, round: Option<(Rect, f32)>) -> Self {
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
        Self::image_in(image, [x, y, w, h], 0., clip, WHITE)
    }

    /// An image stretched to `rect`, with corners rounded by `radius`, multiplied by `tint` (straight RGBA).
    pub fn image_in(image: &Image, rect: Rect, radius: f32, clip: Rect, tint: Color) -> Self {
        Draw::Shape(Shape {
            rect,
            clip,
            round_clip: None,
            color: tint,
            end_color: tint,
            uv: [0., 0., 1., 1.],
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
        radius: f32,
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
            radius: 0.,
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
    clip_radius: f32,
}

// Offscreen texture for one nesting level of layers.
struct Layer {
    texture: wgpu::Texture,
    bindings: wgpu::BindGroup,
    size: [u32; 2],
}

pub struct Renderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    // Shared by the image (group 0) and the mask (group 1) of every quad.
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    format: wgpu::TextureFormat,
    white: Image,
    instances: wgpu::Buffer,
    layers: Vec<Layer>,
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
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Quads"),
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
                        8 => Float32x4, 9 => Float32
                    ],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
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
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let white = Self::upload(&device, &queue, &layout, &sampler, [1, 1], &WHITE);
        let instances = Self::buffer(&device, 64);
        Ok(Self {
            device,
            queue,
            pipeline,
            layout,
            sampler,
            format,
            white,
            instances,
            layers: Vec::new(),
        })
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
        Self::upload(
            &self.device,
            &self.queue,
            &self.layout,
            &self.sampler,
            bitmap.size,
            &bitmap.rgba,
        )
    }

    /// An image from straight RGBA8 pixels. The texture is premultiplied, as the blend expects.
    pub fn rgba_image(&self, rgba: &[u8], size: [usize; 2]) -> Image {
        let mut premultiplied = Vec::with_capacity(rgba.len());
        for pixel in rgba.as_chunks::<4>().0 {
            let a = u32::from(pixel[3]);
            premultiplied.extend(
                pixel[..3]
                    .iter()
                    .map(|&c| ((u32::from(c) * a + 127) / 255) as u8),
            );
            premultiplied.push(pixel[3]);
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
        Image { size, bindings }
    }

    // The layer texture for nesting `depth`, recreated when the target size changes.
    fn layer(&mut self, depth: usize, size: [u32; 2]) {
        if self
            .layers
            .get(depth)
            .is_some_and(|layer| layer.size == size)
        {
            return;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("JSNIR layer"),
            size: extent(size),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let bindings = Self::bind(
            &self.device,
            &self.layout,
            &self.sampler,
            &texture.create_view(&Default::default()),
        );
        let layer = Layer {
            texture,
            bindings,
            size,
        };
        if depth < self.layers.len() {
            self.layers[depth] = layer;
        } else {
            self.layers.push(layer);
        }
    }

    /// Draws into `target`, viewed in the renderer's format.
    pub fn draw(&mut self, target: &wgpu::Texture, size: [u32; 2], scale: f32, draws: &[Draw]) {
        let [px, py] = size.map(|side| side as f32 / scale);
        let quad = |shape: &Shape| -> Quad {
            let [x, y, w, h] = shape.rect;
            let x_edge = |at| at * 2. / px - 1.;
            let y_edge = |at| 1. - at * 2. / py;
            let premultiply = |rgba: Color| {
                let a = f32::from(rgba[3]) / 255.;
                [
                    f32::from(rgba[0]) / 255. * a,
                    f32::from(rgba[1]) / 255. * a,
                    f32::from(rgba[2]) / 255. * a,
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
                        blur / 2. * scale,
                        spread * scale,
                        y * scale,
                        element_radius * scale,
                    ],
                ),
            };
            // The mask rectangle relative to the quad; the whole quad without a mask (white texture).
            let mask = match &shape.mask {
                Some((_, [mx, my, mw, mh])) => {
                    [(mx - x) * scale, (my - y) * scale, mw * scale, mh * scale]
                }
                None => [0., 0., w * scale, h * scale],
            };
            Quad {
                edges: [x_edge(x), y_edge(y), x_edge(x + w), y_edge(y + h)],
                color: premultiply(shape.color),
                end_color: premultiply(shape.end_color),
                uv: shape.uv,
                shape: [w * scale, h * scale, shape.radius * scale, mode],
                params,
                params2,
                round_clip: shape
                    .round_clip
                    .map_or([0.; 4], |(rect, _)| rect.map(|v| v * scale)),
                mask,
                clip_radius: shape.round_clip.map_or(0., |(_, radius)| radius * scale),
            }
        };
        // Commands in order; each shape keeps its instance index and scissor.
        enum Step<'a> {
            Shape(usize, [u32; 4], Option<&'a Image>, Option<&'a Image>),
            Push,
            Pop(usize),
        }
        let full = [0., 0., px, py];
        let mut quads = Vec::new();
        let mut steps = Vec::new();
        let mut opacities = Vec::new();
        for draw in draws {
            match draw {
                Draw::Shape(shape) => {
                    if shape.rect[2] <= 0. || shape.rect[3] <= 0. {
                        continue;
                    }
                    if let Some(scissor) = scissor(shape.clip, size, scale) {
                        let mask = shape.mask.as_ref().map(|(image, _)| image);
                        steps.push(Step::Shape(
                            quads.len(),
                            scissor,
                            shape.image.as_ref(),
                            mask,
                        ));
                        quads.push(quad(shape));
                    }
                }
                Draw::PushLayer { opacity } => {
                    opacities.push(*opacity);
                    steps.push(Step::Push);
                }
                Draw::PopLayer => {
                    let opacity = opacities.pop().unwrap_or(1.).clamp(0., 1.);
                    let tint = [255, 255, 255, (opacity * 255.).round() as u8];
                    steps.push(Step::Pop(quads.len()));
                    quads.push(quad(&Shape {
                        rect: full,
                        clip: full,
                        round_clip: None,
                        color: tint,
                        end_color: tint,
                        uv: [0., 0., 1., 1.],
                        radius: 0.,
                        image: None,
                        mask: None,
                        mode: Mode::Fill,
                    }));
                }
            }
        }
        let depth = steps.iter().fold((0, 0), |(depth, max), step| match step {
            Step::Push => (depth + 1, max.max(depth + 1)),
            Step::Pop(_) => (depth - 1, max),
            Step::Shape(..) => (depth, max),
        });
        for level in 0..depth.1 {
            self.layer(level, size);
        }
        let bytes = bytemuck::cast_slice(&quads);
        if bytes.len() as u64 > self.instances.size() {
            self.instances = Self::buffer(&self.device, quads.len().next_power_of_two());
        }
        if !bytes.is_empty() {
            self.queue.write_buffer(&self.instances, 0, bytes);
        }
        let target_view = target.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.format),
            ..Default::default()
        });
        let layer_views: Vec<wgpu::TextureView> = self
            .layers
            .iter()
            .take(depth.1)
            .map(|layer| layer.texture.create_view(&Default::default()))
            .collect();
        let mut encoder = self.device.create_command_encoder(&Default::default());
        // Each layer level renders in its own pass; popping a layer composites it into the level below.
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
        let mut level = 0;
        let view_at = |level: usize| {
            if level == 0 {
                &target_view
            } else {
                &layer_views[level - 1]
            }
        };
        let mut pass = begin(&mut encoder, &target_view, Some(wgpu::Color::BLACK));
        pass.set_pipeline(&self.pipeline);
        let stride = size_of::<Quad>() as u64;
        for step in steps {
            match step {
                Step::Shape(index, [x, y, w, h], image, mask) => {
                    let offset = index as u64 * stride;
                    pass.set_vertex_buffer(0, self.instances.slice(offset..offset + stride));
                    pass.set_scissor_rect(x, y, w, h);
                    pass.set_bind_group(0, &image.unwrap_or(&self.white).bindings, &[]);
                    pass.set_bind_group(1, &mask.unwrap_or(&self.white).bindings, &[]);
                    pass.draw(0..4, 0..1);
                }
                Step::Push => {
                    drop(pass);
                    level += 1;
                    pass = begin(&mut encoder, view_at(level), Some(wgpu::Color::TRANSPARENT));
                    pass.set_pipeline(&self.pipeline);
                }
                Step::Pop(index) => {
                    drop(pass);
                    level -= 1;
                    pass = begin(&mut encoder, view_at(level), None);
                    pass.set_pipeline(&self.pipeline);
                    let offset = index as u64 * stride;
                    pass.set_vertex_buffer(0, self.instances.slice(offset..offset + stride));
                    pass.set_scissor_rect(0, 0, size[0], size[1]);
                    pass.set_bind_group(0, &self.layers[level].bindings, &[]);
                    pass.set_bind_group(1, &self.white.bindings, &[]);
                    pass.draw(0..4, 0..1);
                }
            }
        }
        drop(pass);
        self.queue.submit([encoder.finish()]);
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
        let draws = [Draw::gradient(rect, rect, 0., RED, RED).masked(Some((mask, rect)))];
        let frame = renderer.read([40, 40], 1., &draws).unwrap();
        assert_close(pixel(&frame, 30, 30), BLACK);
        assert_close(pixel(&frame, 20, 20), RED);
    }

    #[test]
    fn a_rounded_clip_cuts_the_corners_of_its_children() {
        let mut renderer = renderer();
        let rect = [0., 0., 40., 40.];
        // A 40 px red child in a clip of the same rectangle rounded by 20 px.
        let draws = [Draw::gradient(rect, rect, 0., RED, RED).rounded_clip(Some((rect, 20.)))];
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
        let draws = [Draw::gradient(child, child, 0., RED, RED).rounded_clip(Some((parent, 20.)))];
        let frame = renderer.read([60, 60], 1., &draws).unwrap();
        assert_close(pixel(&frame, 21, 21), RED);
        assert_close(pixel(&frame, 1, 1), BLACK);
    }
}
