pub(super) const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

pub(super) struct PostProcessResources {
    pub(super) offscreen_view: wgpu::TextureView,
    pub(super) bind_group: wgpu::BindGroup,
    pub(super) pipeline: wgpu::RenderPipeline,
    offscreen: wgpu::Texture,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl PostProcessResources {
    pub(super) fn new(
        device: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
        offscreen_format: wgpu::TextureFormat,
    ) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("odytty-composite-bgl"),
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
        let pipeline = create_composite_pipeline(device, config.format, &bind_group_layout);
        let (offscreen, offscreen_view, bind_group) = create_offscreen(
            device,
            config,
            offscreen_format,
            &bind_group_layout,
            &sampler,
        );
        Self {
            offscreen_view,
            bind_group,
            pipeline,
            offscreen,
            bind_group_layout,
            sampler,
        }
    }

    pub(super) fn resize(
        &mut self,
        device: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
        offscreen_format: wgpu::TextureFormat,
    ) {
        let (offscreen, offscreen_view, bind_group) = create_offscreen(
            device,
            config,
            offscreen_format,
            &self.bind_group_layout,
            &self.sampler,
        );
        self.offscreen = offscreen;
        self.offscreen_view = offscreen_view;
        self.bind_group = bind_group;
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
    bind_group_layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
) -> (wgpu::Texture, wgpu::TextureView, wgpu::BindGroup) {
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
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("odytty-composite-bg"),
        layout: bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&offscreen_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    (offscreen, offscreen_view, bind_group)
}

fn create_composite_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    bind_group_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("odytty-composite-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../../shaders/composite.wgsl").into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("odytty-composite-pl"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("odytty-composite-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
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
            module: &shader,
            entry_point: Some("fs_main"),
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
