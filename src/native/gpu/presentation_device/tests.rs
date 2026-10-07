// SPDX-License-Identifier: GPL-3.0-only
use super::*;

#[test]
#[ignore = "requires a working headless EGL/GL adapter; run explicitly on a GL host"]
fn gl_sibling_release_preserves_drawn_pixels() {
    let _creation = crate::test_lock::device_creation_lock();
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::GL;
    // This fixture submits direct draws only. A headless GL 3.3 context
    // cannot compile wgpu's compute shader for indirect-draw validation.
    descriptor
        .flags
        .remove(wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL);
    let instance = wgpu::Instance::new(descriptor);
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
        .expect("headless GL adapter required");
    assert_eq!(adapter.get_info().backend, wgpu::Backend::Gl);
    let limits = super::super::pipeline_policy::required_limits_for_adapter(&adapter.limits()).0;
    let (first, queue) =
        request_device(&instance, &adapter, wgpu::Features::empty(), limits.clone())
            .expect("first device");
    assert_eq!(draw_pixel(&first, &queue), [255, 0, 0, 255]);
    for _ in 0..3 {
        let (second, second_queue) =
            request_device(&instance, &adapter, wgpu::Features::empty(), limits.clone())
                .expect("sibling device");
        assert_eq!(second, first, "GL windows reuse the presentation device");
        assert_eq!(
            second_queue, queue,
            "GL windows reuse the presentation queue"
        );
        assert_eq!(cached_adapter(&instance), Some(adapter.clone()));
        assert_eq!(draw_pixel(&second, &second_queue), [255, 0, 0, 255]);
        second
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("sibling idle");
        drop(second_queue);
        drop(second);
        first
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("survivor idle");
        assert_eq!(
            draw_pixel(&first, &queue),
            [255, 0, 0, 255],
            "closing a sibling must leave the survivor drawing, not only clearing"
        );
    }
}

fn draw_pixel(device: &wgpu::Device, queue: &wgpu::Queue) -> [u8; 4] {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sibling-release-shader"),
        source: wgpu::ShaderSource::Wgsl(
            r#"
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(p[i], 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return vec4(1.0, 0.0, 0.0, 1.0); }
"#
            .into(),
        ),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("sibling-release-pipeline"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });
    let size = wgpu::Extent3d {
        width: 4,
        height: 4,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sibling-release-target"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sibling-release-readback"),
        size: 1024,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let view = texture.create_view(&Default::default());
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLUE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&pipeline);
        pass.draw(0..3, 0..1);
    }
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(4),
            },
        },
        size,
    );
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).unwrap();
        });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("readback poll");
    rx.recv().expect("map callback").expect("map success");
    let mapped = buffer.slice(..).get_mapped_range();
    let pixel = mapped[260..264].try_into().expect("RGBA pixel");
    drop(mapped);
    buffer.unmap();
    pixel
}

#[test]
fn shared_loss_reaches_live_subscribers_and_stays_sticky_for_new_windows() {
    let subscribers = Subscribers::default();
    let first_lost = Arc::new(AtomicBool::new(false));
    let closed_lost = Arc::new(AtomicBool::new(false));
    let second_lost = Arc::new(AtomicBool::new(false));
    let first = subscribers.attach(first_lost.clone(), None);
    let closed = subscribers.attach(closed_lost.clone(), None);
    let second = subscribers.attach(second_lost.clone(), None);
    drop(closed);
    assert!(!first_lost.load(Ordering::Acquire));
    assert!(!second_lost.load(Ordering::Acquire));
    subscribers.notify_loss();
    assert!(first_lost.load(Ordering::Acquire));
    assert!(second_lost.load(Ordering::Acquire));
    assert!(
        !closed_lost.load(Ordering::Acquire),
        "closed subscription is not notified"
    );
    assert_eq!(subscribers.windows.lock().unwrap().len(), 2);
    let new_lost = Arc::new(AtomicBool::new(false));
    let new = subscribers.attach(new_lost.clone(), None);
    assert!(
        new_lost.load(Ordering::Acquire),
        "device loss cannot be cleared by a new window"
    );
    drop((first, second, new));
    subscribers.notify_loss();
    assert!(subscribers.windows.lock().unwrap().is_empty());
}

#[test]
fn closing_one_subscription_keeps_the_others_registered_without_loss() {
    let subscribers = Subscribers::default();
    let survivor_lost = Arc::new(AtomicBool::new(false));
    let survivor = subscribers.attach(survivor_lost.clone(), None);
    for _ in 2..12 {
        let transient = subscribers.attach(Arc::new(AtomicBool::new(false)), None);
        assert_eq!(subscribers.windows.lock().unwrap().len(), 2);
        drop(transient);
        assert!(!survivor_lost.load(Ordering::Acquire));
        assert!(!subscribers.lost.load(Ordering::Acquire));
    }
    assert!(Arc::ptr_eq(&survivor.lost, &survivor_lost));
}
