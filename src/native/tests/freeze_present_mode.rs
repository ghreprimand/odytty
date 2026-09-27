// SPDX-License-Identifier: GPL-3.0-only
//! Present-mode policy regressions for the NVIDIA Wayland freeze mitigation.

use crate::native::gpu::present_mode::select_present_mode;
use std::fs;
use std::path::{Path, PathBuf};
use wgpu::{Backend, PresentMode};

const NVIDIA_VENDOR_ID: u32 = 0x10DE;

fn select(
    offered: &[PresentMode],
    wayland: bool,
    backend: Backend,
    vendor: u32,
    driver: &str,
) -> PresentMode {
    select_present_mode(offered, wayland, backend, vendor, driver)
}

#[test]
fn nvidia_wayland_uses_mailbox_when_offered() {
    assert_eq!(
        select(
            &[PresentMode::Fifo, PresentMode::Mailbox],
            true,
            Backend::Vulkan,
            NVIDIA_VENDOR_ID,
            "NVIDIA",
        ),
        PresentMode::Mailbox,
    );
}

#[test]
fn nvidia_wayland_falls_back_to_fifo_without_mailbox() {
    assert_eq!(
        select(
            &[PresentMode::Fifo, PresentMode::Immediate],
            true,
            Backend::Vulkan,
            NVIDIA_VENDOR_ID,
            "NVIDIA",
        ),
        PresentMode::Fifo,
    );
}

#[test]
fn non_nvidia_wayland_keeps_fifo() {
    for (vendor, driver) in [(0x1002, "AMD RADV"), (NVIDIA_VENDOR_ID, "NVK")] {
        assert_eq!(
            select(
                &[PresentMode::Mailbox, PresentMode::Fifo],
                true,
                Backend::Vulkan,
                vendor,
                driver,
            ),
            PresentMode::Fifo,
            "Wayland adapter {vendor:#06x} / {driver} keeps FIFO",
        );
    }
}

#[test]
fn nvidia_x11_keeps_fifo() {
    assert_eq!(
        select(
            &[PresentMode::Mailbox, PresentMode::Fifo],
            false,
            Backend::Vulkan,
            NVIDIA_VENDOR_ID,
            "NVIDIA",
        ),
        PresentMode::Fifo,
    );
}

#[test]
fn empty_offered_modes_fall_back_to_fifo() {
    assert_eq!(
        select(&[], true, Backend::Vulkan, NVIDIA_VENDOR_ID, "NVIDIA"),
        PresentMode::Fifo,
    );
}

#[test]
fn offered_mode_order_does_not_change_the_choice() {
    let offered = [
        [PresentMode::Mailbox, PresentMode::Fifo],
        [PresentMode::Fifo, PresentMode::Mailbox],
    ];
    for modes in offered {
        assert_eq!(
            select(&modes, true, Backend::Vulkan, NVIDIA_VENDOR_ID, "NVIDIA"),
            PresentMode::Mailbox,
        );
    }
}

#[test]
fn gpu_surface_present_modes_are_selected_by_the_policy_helper() {
    let gpu_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/native/gpu");
    let files = rust_sources_recursively(&gpu_dir);
    assert!(!files.is_empty(), "GPU source scan found no Rust files");

    let mut configurations = 0;
    for path in files {
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed reading {}: {error}", path.display()));
        let mut offset = 0;
        while let Some(relative) = source[offset..].find("SurfaceConfiguration {") {
            let start = offset + relative;
            let block_start = start + "SurfaceConfiguration {".len();
            let Some(relative_end) = source[block_start..].find("};") else {
                panic!("unterminated SurfaceConfiguration in {}", path.display());
            };
            let block = &source[block_start..block_start + relative_end];
            configurations += 1;

            let mode_field = block
                .lines()
                .map(str::trim)
                .find(|line| line.starts_with("present_mode") || line.starts_with("r#present_mode"))
                .unwrap_or_else(|| {
                    panic!(
                        "SurfaceConfiguration has no present_mode in {}",
                        path.display()
                    )
                });
            let binding = if let Some((_, value)) = mode_field.split_once(':') {
                value.trim().trim_end_matches(',').trim()
            } else {
                mode_field.trim_end_matches(',').trim()
            };

            if binding.starts_with("select_present_mode(") {
                // Inline policy call: the configuration cannot bypass the helper.
            } else {
                let binding = binding.trim_end_matches(',');
                let declaration = format!("let {binding} =");
                let function_start = source[..start].rfind("fn ").unwrap_or(0);
                let function_source = &source[function_start..start];
                let declaration_start = function_source.rfind(&declaration).unwrap_or_else(|| {
                    panic!(
                        "{} has no local {binding} present-mode binding",
                        path.display()
                    )
                });
                let initializer = function_source[declaration_start + declaration.len()..]
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim();
                assert!(
                    initializer.starts_with("select_present_mode("),
                    "{} configures present_mode through {binding:?}, not select_present_mode",
                    path.display(),
                );
            }
            offset = block_start + relative_end + 2;
        }

        let mut assignment_offset = 0;
        while let Some(relative) = source[assignment_offset..].find(".present_mode") {
            let field_end = assignment_offset + relative + ".present_mode".len();
            let suffix = source[field_end..].trim_start();
            if let Some(rhs) = suffix.strip_prefix('=')
                && !rhs.starts_with('=')
            {
                let statement = rhs.split(';').next().unwrap_or(rhs);
                assert!(
                    statement.contains("revalidate_present_mode("),
                    "{} mutates a surface present_mode outside revalidate_present_mode",
                    path.display(),
                );
            }
            assignment_offset = field_end;
        }
    }
    assert!(
        configurations > 0,
        "GPU source scan found no SurfaceConfiguration literals"
    );
}

fn rust_sources_recursively(directory: &Path) -> Vec<PathBuf> {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("failed reading {}: {error}", directory.display()));
    let mut sources = Vec::new();
    for entry in entries {
        let entry = entry.expect("GPU directory entry");
        let path = entry.path();
        if path.is_dir() {
            sources.extend(rust_sources_recursively(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push(path);
        }
    }
    sources
}
