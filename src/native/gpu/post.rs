use wgpu::util::DeviceExt;

pub(super) const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BloomOptions {
    pub(crate) enabled: bool,
    pub(crate) threshold: f32,
    pub(crate) intensity: f32,
    pub(crate) radius: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BloomUniform {
    threshold: f32,
    intensity: f32,
    radius: f32,
    _pad: f32,
}

pub(super) struct PostProcessResources {
    pub(super) offscreen_view: wgpu::TextureView,
    offscreen: wgpu::Texture,
    bright: wgpu::Texture,
    bright_view: wgpu::TextureView,
    ping: wgpu::Texture,
    ping_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    linear_sampler: wgpu::Sampler,
    bloom_uniform: wgpu::Buffer,
    bright_pipeline: wgpu::RenderPipeline,
    blur_h_pipeline: wgpu::RenderPipeline,
    blur_v_pipeline: wgpu::RenderPipeline,
    bloom_composite_pipeline: wgpu::RenderPipeline,
    bright_bind_group_layout: wgpu::BindGroupLayout,
    blur_bind_group_layout: wgpu::BindGroupLayout,
    bloom_composite_bind_group_layout: wgpu::BindGroupLayout,
    bright_bind_group: wgpu::BindGroup,
    blur_h_bind_group: wgpu::BindGroup,
    blur_v_bind_group: wgpu::BindGroup,
    bloom_composite_bind_group: wgpu::BindGroup,
}

impl PostProcessResources {
    pub(super) fn new(
        device: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
        offscreen_format: wgpu::TextureFormat,
    ) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("odytty-composite-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let linear_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("odytty-bloom-linear-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("odytty-bloom-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../../shaders/bloom.wgsl").into()),
        });
        let bright_bind_group_layout = create_bright_bind_group_layout(device);
        let blur_bind_group_layout = create_blur_bind_group_layout(device);
        let bloom_composite_bind_group_layout = create_bloom_composite_bind_group_layout(device);
        let bright_pipeline = create_bloom_pipeline(
            device,
            &shader,
            "odytty-bloom-bright-pipeline",
            "fs_bright",
            offscreen_format,
            &[Some(&bright_bind_group_layout)],
        );
        let blur_h_pipeline = create_bloom_pipeline(
            device,
            &shader,
            "odytty-bloom-blur-h-pipeline",
            "fs_blur_h",
            offscreen_format,
            &[Some(&blur_bind_group_layout)],
        );
        let blur_v_pipeline = create_bloom_pipeline(
            device,
            &shader,
            "odytty-bloom-blur-v-pipeline",
            "fs_blur_v",
            offscreen_format,
            &[Some(&blur_bind_group_layout)],
        );
        let bloom_composite_pipeline = create_bloom_pipeline(
            device,
            &shader,
            "odytty-bloom-composite-pipeline",
            "fs_composite_bloom",
            config.format,
            &[Some(&bloom_composite_bind_group_layout)],
        );
        let bloom_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("odytty-bloom-uniform"),
            contents: bytemuck::bytes_of(&BloomUniform {
                threshold: 1.0,
                intensity: 0.0,
                radius: 3.0,
                _pad: 0.0,
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let (offscreen, offscreen_view) = create_offscreen(device, config, offscreen_format);
        let (bright, bright_view) =
            create_half_res_texture(device, config, offscreen_format, "odytty-bloom-bright");
        let (ping, ping_view) =
            create_half_res_texture(device, config, offscreen_format, "odytty-bloom-ping");
        let bright_bind_group = create_bright_bind_group(
            device,
            &bright_bind_group_layout,
            &offscreen_view,
            &sampler,
            &bloom_uniform,
        );
        let blur_h_bind_group = create_blur_bind_group(
            device,
            &blur_bind_group_layout,
            &bright_view,
            &linear_sampler,
            &bloom_uniform,
        );
        let blur_v_bind_group = create_blur_bind_group(
            device,
            &blur_bind_group_layout,
            &ping_view,
            &linear_sampler,
            &bloom_uniform,
        );
        let bloom_composite_bind_group = create_bloom_composite_bind_group(
            device,
            &bloom_composite_bind_group_layout,
            &offscreen_view,
            &bright_view,
            &sampler,
            &linear_sampler,
            &bloom_uniform,
        );
        Self {
            offscreen_view,
            offscreen,
            bright,
            bright_view,
            ping,
            ping_view,
            sampler,
            linear_sampler,
            bloom_uniform,
            bright_pipeline,
            blur_h_pipeline,
            blur_v_pipeline,
            bloom_composite_pipeline,
            bright_bind_group_layout,
            blur_bind_group_layout,
            bloom_composite_bind_group_layout,
            bright_bind_group,
            blur_h_bind_group,
            blur_v_bind_group,
            bloom_composite_bind_group,
        }
    }

    pub(super) fn resize(
        &mut self,
        device: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
        offscreen_format: wgpu::TextureFormat,
    ) {
        let (offscreen, offscreen_view) = create_offscreen(device, config, offscreen_format);
        self.offscreen = offscreen;
        self.offscreen_view = offscreen_view;

        let (bright, bright_view) =
            create_half_res_texture(device, config, offscreen_format, "odytty-bloom-bright");
        let (ping, ping_view) =
            create_half_res_texture(device, config, offscreen_format, "odytty-bloom-ping");
        self.bright = bright;
        self.bright_view = bright_view;
        self.ping = ping;
        self.ping_view = ping_view;
        self.rebuild_bloom_bind_groups(device);
    }

    pub(super) fn encode_bloom(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        queue: &wgpu::Queue,
        swapchain_view: &wgpu::TextureView,
        options: BloomOptions,
    ) {
        queue.write_buffer(
            &self.bloom_uniform,
            0,
            bytemuck::bytes_of(&BloomUniform {
                threshold: options.threshold,
                intensity: options.intensity,
                radius: options.radius,
                _pad: 0.0,
            }),
        );
        encode_fullscreen_pass(
            encoder,
            "odytty-bloom-bright-pass",
            &self.bright_view,
            &self.bright_pipeline,
            &self.bright_bind_group,
        );
        encode_fullscreen_pass(
            encoder,
            "odytty-bloom-blur-h-pass",
            &self.ping_view,
            &self.blur_h_pipeline,
            &self.blur_h_bind_group,
        );
        encode_fullscreen_pass(
            encoder,
            "odytty-bloom-blur-v-pass",
            &self.bright_view,
            &self.blur_v_pipeline,
            &self.blur_v_bind_group,
        );
        encode_fullscreen_pass(
            encoder,
            "odytty-bloom-composite-pass",
            swapchain_view,
            &self.bloom_composite_pipeline,
            &self.bloom_composite_bind_group,
        );
    }

    fn rebuild_bloom_bind_groups(&mut self, device: &wgpu::Device) {
        self.bright_bind_group = create_bright_bind_group(
            device,
            &self.bright_bind_group_layout,
            &self.offscreen_view,
            &self.sampler,
            &self.bloom_uniform,
        );
        self.blur_h_bind_group = create_blur_bind_group(
            device,
            &self.blur_bind_group_layout,
            &self.bright_view,
            &self.linear_sampler,
            &self.bloom_uniform,
        );
        self.blur_v_bind_group = create_blur_bind_group(
            device,
            &self.blur_bind_group_layout,
            &self.ping_view,
            &self.linear_sampler,
            &self.bloom_uniform,
        );
        self.bloom_composite_bind_group = create_bloom_composite_bind_group(
            device,
            &self.bloom_composite_bind_group_layout,
            &self.offscreen_view,
            &self.bright_view,
            &self.sampler,
            &self.linear_sampler,
            &self.bloom_uniform,
        );
    }
}

pub(super) fn supported_format(adapter: &wgpu::Adapter) -> Option<wgpu::TextureFormat> {
    let features = adapter.get_texture_format_features(HDR_FORMAT);
    let required_usages =
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
    if features.allowed_usages.contains(required_usages)
        && features
            .flags
            .contains(wgpu::TextureFormatFeatureFlags::FILTERABLE)
    {
        Some(HDR_FORMAT)
    } else {
        None
    }
}

fn create_offscreen(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
    format: wgpu::TextureFormat,
) -> (wgpu::Texture, wgpu::TextureView) {
    let offscreen = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("odytty-post-offscreen"),
        size: wgpu::Extent3d {
            width: config.width.max(1),
            height: config.height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let offscreen_view = offscreen.create_view(&wgpu::TextureViewDescriptor::default());
    (offscreen, offscreen_view)
}

fn create_half_res_texture(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
    format: wgpu::TextureFormat,
    label: &'static str,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: (config.width / 2).max(1),
            height: (config.height / 2).max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn create_bright_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("odytty-bloom-bright-bgl"),
        entries: &[texture_entry(0), sampler_entry(1), uniform_entry(2)],
    })
}

fn create_blur_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("odytty-bloom-blur-bgl"),
        entries: &[texture_entry(0), sampler_entry(1), uniform_entry(2)],
    })
}

fn create_bloom_composite_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("odytty-bloom-composite-bgl"),
        entries: &[
            texture_entry(0),
            sampler_entry(1),
            uniform_entry(2),
            texture_entry(3),
            sampler_entry(4),
        ],
    })
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn create_bright_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    scene: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    uniform: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("odytty-bloom-bright-bg"),
        layout,
        entries: &[
            bind_texture(0, scene),
            bind_sampler(1, sampler),
            bind_uniform(2, uniform),
        ],
    })
}

fn create_blur_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    source: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    uniform: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("odytty-bloom-blur-bg"),
        layout,
        entries: &[
            bind_texture(0, source),
            bind_sampler(1, sampler),
            bind_uniform(2, uniform),
        ],
    })
}

fn create_bloom_composite_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    scene: &wgpu::TextureView,
    bloom: &wgpu::TextureView,
    scene_sampler: &wgpu::Sampler,
    bloom_sampler: &wgpu::Sampler,
    uniform: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("odytty-bloom-composite-bg"),
        layout,
        entries: &[
            bind_texture(0, scene),
            bind_sampler(1, scene_sampler),
            bind_uniform(2, uniform),
            bind_texture(3, bloom),
            bind_sampler(4, bloom_sampler),
        ],
    })
}

fn bind_texture<'a>(binding: u32, view: &'a wgpu::TextureView) -> wgpu::BindGroupEntry<'a> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(view),
    }
}

fn bind_sampler<'a>(binding: u32, sampler: &'a wgpu::Sampler) -> wgpu::BindGroupEntry<'a> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::Sampler(sampler),
    }
}

fn bind_uniform<'a>(binding: u32, uniform: &'a wgpu::Buffer) -> wgpu::BindGroupEntry<'a> {
    wgpu::BindGroupEntry {
        binding,
        resource: uniform.as_entire_binding(),
    }
}

fn create_bloom_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    label: &'static str,
    fragment_entry: &'static str,
    format: wgpu::TextureFormat,
    bind_group_layouts: &[Option<&wgpu::BindGroupLayout>],
) -> wgpu::RenderPipeline {
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts,
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment_entry),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn encode_fullscreen_pass(
    encoder: &mut wgpu::CommandEncoder,
    label: &'static str,
    view: &wgpu::TextureView,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, &[]);
    pass.draw(0..3, 0..1);
}
