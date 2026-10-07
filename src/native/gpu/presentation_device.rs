// SPDX-License-Identifier: GPL-3.0-only
//! GL windows share one device and queue for their instance's EGL context.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::native::options::NativeError;
use crate::native::pty::UserEvent;

struct GlDevice {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    subscribers: Arc<Subscribers>,
    features: wgpu::Features,
    limits: wgpu::Limits,
}

static GL_DEVICES: OnceLock<Mutex<Vec<GlDevice>>> = OnceLock::new();

/// Kept only while the window's renderer exists. The shared device's callback
/// holds weak subscriptions, so it never retains a closed window or session.
pub(super) struct Subscription {
    lost: Arc<AtomicBool>,
    proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>>,
}

#[derive(Default)]
struct Subscribers {
    lost: AtomicBool,
    windows: Mutex<Vec<Weak<Subscription>>>,
}

impl Subscribers {
    fn attach(
        &self,
        lost: Arc<AtomicBool>,
        proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>>,
    ) -> Arc<Subscription> {
        let subscription = Arc::new(Subscription { lost, proxy });
        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        windows.retain(|window| window.strong_count() != 0);
        windows.push(Arc::downgrade(&subscription));
        // Attach and loss publication use the same lock, so a newly attached
        // window either joins the callback or observes the already lost device.
        if self.lost.load(Ordering::Acquire) {
            subscription.lost.store(true, Ordering::Release);
        }
        subscription
    }

    fn notify_loss(&self) {
        let windows = {
            let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
            self.lost.store(true, Ordering::Release);
            windows.retain(|window| window.strong_count() != 0);
            windows.iter().filter_map(Weak::upgrade).collect::<Vec<_>>()
        };
        for window in &windows {
            window.lost.store(true, Ordering::Release);
        }
        if let Some(proxy) = windows.iter().find_map(|window| window.proxy.as_ref()) {
            let _ = proxy.send_event(UserEvent::GpuDeviceStateChanged);
        }
    }
}

fn devices() -> &'static Mutex<Vec<GlDevice>> {
    GL_DEVICES.get_or_init(Mutex::default)
}

pub(super) fn cached_adapter(instance: &wgpu::Instance) -> Option<wgpu::Adapter> {
    devices()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|entry| entry.instance == *instance)
        .map(|entry| entry.adapter.clone())
}

pub(super) fn subscribe(
    device: &wgpu::Device,
    lost: Arc<AtomicBool>,
    proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>>,
) -> Option<Arc<Subscription>> {
    devices()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|entry| entry.device == *device)
        .map(|entry| entry.subscribers.attach(lost, proxy))
}

pub(super) fn request_device(
    instance: &wgpu::Instance,
    adapter: &wgpu::Adapter,
    features: wgpu::Features,
    limits: wgpu::Limits,
) -> Result<(wgpu::Device, wgpu::Queue), NativeError> {
    if adapter.get_info().backend != wgpu::Backend::Gl {
        return create_device(adapter, features, limits);
    }
    let mut devices = devices().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = devices.iter().find(|entry| entry.instance == *instance) {
        if entry.subscribers.lost.load(Ordering::Acquire) {
            return Err(NativeError::DeviceRequest(
                "GL presentation device is lost; restart OdyTTY".into(),
            ));
        }
        if entry.features != features || entry.limits != limits {
            return Err(NativeError::DeviceRequest(
                "inconsistent GL presentation device requirements".into(),
            ));
        }
        return Ok((entry.device.clone(), entry.queue.clone()));
    }
    // wgpu-hal binds its GL main VAO only when opening the device. A second
    // device on the same EGL context replaces that binding, and deleting it
    // leaves the survivor without a VAO. Retain one device per GL instance;
    // surfaces, pipelines, buffers and atlases remain window-owned.
    let (device, queue) = create_device(adapter, features, limits.clone())?;
    let subscribers = Arc::new(Subscribers::default());
    device.on_uncaptured_error(Arc::new(|error| {
        tracing::error!("uncaptured GPU error: {error}")
    }));
    let callback = Arc::clone(&subscribers);
    device.set_device_lost_callback(move |reason, message| {
        tracing::error!("GPU device lost ({reason:?}): {message}");
        callback.notify_loss();
    });
    devices.push(GlDevice {
        instance: instance.clone(),
        adapter: adapter.clone(),
        device: device.clone(),
        queue: queue.clone(),
        subscribers,
        features,
        limits,
    });
    Ok((device, queue))
}

fn create_device(
    adapter: &wgpu::Adapter,
    features: wgpu::Features,
    limits: wgpu::Limits,
) -> Result<(wgpu::Device, wgpu::Queue), NativeError> {
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("odytty-device"),
        required_features: features,
        required_limits: limits,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        // The integrated-GPU gate cut exact-geometry idle memory by 68%
        // without a repeatable stream-interval regression. Retain that native
        // presentation allocator policy; headless fixture devices elsewhere
        // keep their default allocator.
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
    }))
    .map_err(|err| NativeError::DeviceRequest(err.to_string()))
}

#[cfg(test)]
mod tests;
