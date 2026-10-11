// SPDX-License-Identifier: GPL-3.0-only
//! One adapter-availability policy for every headless GPU test.

use crate::native::gpu::{HeadlessGpuFixture, headless_gpu_lifetime, headless_test_instance};

/// Environment variable that makes a missing GPU prerequisite fail the test
/// instead of skipping it. CI's Linux validation step sets it, where a
/// software Vulkan adapter is installed.
pub(in crate::native) const REQUIRE_GPU_TESTS_ENV: &str = "ODYTTY_REQUIRE_GPU_TESTS";

/// A headless device for `test`, or `None` when the test must skip.
///
/// Without a usable adapter, or with `hdr` set and no filterable HDR render
/// target, the test prints `VALIDATION_SKIP test=<name> ...` and returns
/// early; with [`REQUIRE_GPU_TESTS_ENV`] set it fails instead, and the adapter
/// is the software Vulkan fallback rather than the default selection. A
/// returned device prints `VALIDATION_EXECUTED test=<name>`, so a required
/// run can prove that the test got past its availability check.
pub(in crate::native) fn headless_device(
    test: &str,
    label: &'static str,
    hdr: bool,
) -> Option<HeadlessGpuFixture> {
    headless_device_with_limits(test, label, hdr, wgpu::Limits::default())
}

/// [`headless_device`] with explicit device limits, for a test that needs a
/// limit lower than the default.
pub(in crate::native) fn headless_device_with_limits(
    test: &str,
    label: &'static str,
    hdr: bool,
    required_limits: wgpu::Limits,
) -> Option<HeadlessGpuFixture> {
    let required = std::env::var_os(REQUIRE_GPU_TESTS_ENV).is_some();
    let lifetime = headless_gpu_lifetime();
    // Serialize driver init against every other parallel test creating a device.
    let init_guard = crate::test_lock::device_creation_lock();
    let backends = if required {
        wgpu::Backends::VULKAN
    } else {
        wgpu::Backends::all()
    };
    let instance = headless_test_instance(backends);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        force_fallback_adapter: required,
        compatible_surface: None,
    }));
    let available = super::availability::available;
    if !available(test, "GPU-adapter", adapter.is_ok(), required) {
        return None;
    }
    let adapter = adapter.expect("availability checked");
    if hdr
        && !available(
            test,
            "HDR-render-target",
            crate::native::gpu::post::supported_format(&adapter).is_some(),
            required,
        )
    {
        return None;
    }
    let device = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some(label),
        required_features: wgpu::Features::empty(),
        required_limits,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }));
    if !available(test, "GPU-device", device.is_ok(), required) {
        return None;
    }
    drop(init_guard);
    let (device, queue) = device.expect("availability checked");
    eprintln!("VALIDATION_EXECUTED test={test}");
    Some(HeadlessGpuFixture::new(device, queue, lifetime))
}
