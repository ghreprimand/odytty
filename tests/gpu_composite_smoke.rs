use std::sync::mpsc;

const WIDTH: u32 = 8;
const HEIGHT: u32 = 6;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

const SCENE_SHADER: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    var out: VsOut;
    let pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    out.pos = vec4<f32>(pos[vertex_index], 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let x_even = (u32(pos.x) & 1u) == 0u;
    let y_even = (u32(pos.y) & 1u) == 0u;
    let r = select(0.0, 1.0, x_even);
    let g = select(0.0, 1.0, y_even);
    let b = select(0.0, 1.0, x_even != y_even);
    return vec4<f32>(r, g, b, 1.0);
}
"#;

#[test]
fn passthrough_composite_matches_direct_render_bytes() {
    let Some((device, queue)) = gpu_device() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };

    let scene_pipeline = create_scene_pipeline(&device);
    let composite_bgl = create_composite_bind_group_layout(&device);
    let composite_pipeline = create_composite_pipeline(&device, &composite_bgl);
    let composite_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("gpu-composite-smoke-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });

    let direct = create_render_texture(&device, "gpu-composite-smoke-direct", true);
    let offscreen = create_render_texture(&device, "gpu-composite-smoke-offscreen", false);
    let composite = create_render_texture(&device, "gpu-composite-smoke-composite", true);
    let direct_view = direct.create_view(&wgpu::TextureViewDescriptor::default());
    let offscreen_view = offscreen.create_view(&wgpu::TextureViewDescriptor::default());
    let composite_view = composite.create_view(&wgpu::TextureViewDescriptor::default());
    let composite_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("gpu-composite-smoke-bg"),
        layout: &composite_bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&offscreen_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&composite_sampler),
            },
        ],
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("gpu-composite-smoke-encoder"),
    });
    encode_scene(&mut encoder, &scene_pipeline, &direct_view);
    encode_scene(&mut encoder, &scene_pipeline, &offscreen_view);
    encode_composite(
        &mut encoder,
        &composite_pipeline,
        &composite_bind_group,
        &composite_view,
    );

    let direct_buffer = create_readback_buffer(&device);
    let composite_buffer = create_readback_buffer(&device);
    copy_texture_to_buffer(&mut encoder, &direct, &direct_buffer);
    copy_texture_to_buffer(&mut encoder, &composite, &composite_buffer);
    queue.submit(std::iter::once(encoder.finish()));

    let direct_bytes = readback(&device, &direct_buffer);
    let composite_bytes = readback(&device, &composite_buffer);
    assert_eq!(direct_bytes, composite_bytes);
}

fn gpu_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("gpu-composite-smoke-device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }))
    .ok()
}

fn create_scene_pipeline(device: &wgpu::Device) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("gpu-composite-smoke-scene-shader"),
        source: wgpu::ShaderSource::Wgsl(SCENE_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("gpu-composite-smoke-scene-pl"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("gpu-composite-smoke-scene-pipeline"),
        layout: Some(&layout),
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
                format: FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn create_composite_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("gpu-composite-smoke-bgl"),
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
    })
}

fn create_composite_pipeline(
    device: &wgpu::Device,
    bind_group_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("gpu-composite-smoke-composite-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../src/shaders/composite.wgsl").into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("gpu-composite-smoke-composite-pl"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("gpu-composite-smoke-composite-pipeline"),
        layout: Some(&layout),
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
                format: FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn create_render_texture(
    device: &wgpu::Device,
    label: &'static str,
    copy_src: bool,
) -> wgpu::Texture {
    let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
    if copy_src {
        usage |= wgpu::TextureUsages::COPY_SRC;
    }
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage,
        view_formats: &[],
    })
}

fn encode_scene(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::RenderPipeline,
    view: &wgpu::TextureView,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("gpu-composite-smoke-scene-pass"),
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
    pass.draw(0..3, 0..1);
}

fn encode_composite(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
    view: &wgpu::TextureView,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("gpu-composite-smoke-composite-pass"),
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

fn create_readback_buffer(device: &wgpu::Device) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu-composite-smoke-readback"),
        size: padded_bytes_per_row() as u64 * HEIGHT as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    })
}

fn copy_texture_to_buffer(
    encoder: &mut wgpu::CommandEncoder,
    texture: &wgpu::Texture,
    buffer: &wgpu::Buffer,
) {
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row()),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
}

fn readback(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Vec<u8> {
    let slice = buffer.slice(..);
    let (tx, rx) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        tx.send(result).expect("map callback send");
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll");
    rx.recv()
        .expect("map callback")
        .expect("map readback buffer");
    let mapped = slice.get_mapped_range();
    let mut rows = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for row in mapped
        .chunks(padded_bytes_per_row() as usize)
        .take(HEIGHT as usize)
    {
        rows.extend_from_slice(&row[..(WIDTH * 4) as usize]);
    }
    drop(mapped);
    buffer.unmap();
    rows
}

fn padded_bytes_per_row() -> u32 {
    let unpadded = WIDTH * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    unpadded.div_ceil(align) * align
}
