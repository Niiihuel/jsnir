use crate::{Result, renderer::Renderer};
use std::sync::Arc;
use winit::{dpi::PhysicalSize, event_loop::OwnedDisplayHandle, window::Window};

/// Opens a surface for `window`, preferring Vulkan, Metal or DX12 and falling back to OpenGL.
/// Starting Mesa's OpenGL driver next to Vulkan costs ~30 MB of memory, so OpenGL is tried only when
/// needed. `WGPU_BACKEND` still decides when it is set.
pub fn open(
    display: &OwnedDisplayHandle,
    window: Arc<Window>,
) -> Result<(wgpu::Instance, Surface)> {
    let instance = |backends: wgpu::Backends| {
        let mut descriptor =
            wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(display.clone()));
        if std::env::var_os("WGPU_BACKEND").is_none() {
            descriptor.backends = backends;
        }
        wgpu::Instance::new(descriptor)
    };
    let primary = instance(wgpu::Backends::PRIMARY);
    match Surface::new(&primary, window.clone()) {
        Ok(surface) => Ok((primary, surface)),
        Err(_) => {
            let gl = instance(wgpu::Backends::GL);
            let surface = Surface::new(&gl, window)?;
            Ok((gl, surface))
        }
    }
}

pub struct Surface {
    pub window: Arc<Window>,
    pub target: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pub renderer: Renderer,
}

impl Surface {
    pub fn new(instance: &wgpu::Instance, window: Arc<Window>) -> Result<Self> {
        let target = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&target),
            ..Default::default()
        }))?;
        let PhysicalSize { width, height } = window.inner_size();
        let mut config = target
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or("wgpu found no surface format for the window")?;
        // Render through a view without the sRGB suffix: colors arrive encoded and blend as in browsers.
        let format = config.format.remove_srgb_suffix();
        config.view_formats = vec![format];
        // A backdrop blur copies what is drawn; without copies from the swapchain it is skipped.
        if target
            .get_capabilities(&adapter)
            .usages
            .contains(wgpu::TextureUsages::COPY_SRC)
        {
            config.usage |= wgpu::TextureUsages::COPY_SRC;
        }
        let renderer = Renderer::new(&adapter, format)?;
        target.configure(&renderer.device, &config);
        Ok(Self {
            window,
            target,
            config,
            renderer,
        })
    }

    pub fn resize(&mut self) {
        let size = self.window.inner_size();
        if size.width > 0 && size.height > 0 {
            self.config.width = size.width;
            self.config.height = size.height;
            self.target.configure(&self.renderer.device, &self.config);
        }
    }
}
