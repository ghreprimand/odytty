// SPDX-License-Identifier: GPL-3.0-only
//! Match the Windows window style to its DX12 presentation system.

use wgpu::{Backends, Dx12SwapchainKind};

pub(in crate::native) fn presentation_attempts(
    windows: bool,
    backends: Option<Backends>,
    requested: Option<Dx12SwapchainKind>,
) -> Vec<Dx12SwapchainKind> {
    let supports_dx12 = backends.is_none_or(|set| set.contains(Backends::DX12));
    if windows
        && supports_dx12
        && requested.unwrap_or(Dx12SwapchainKind::DxgiFromVisual)
            == Dx12SwapchainKind::DxgiFromVisual
    {
        vec![
            Dx12SwapchainKind::DxgiFromVisual,
            Dx12SwapchainKind::DxgiFromHwnd,
        ]
    } else {
        vec![Dx12SwapchainKind::DxgiFromHwnd]
    }
}

/// Preserve ordinary adapter fallback: a software DX12 Visual must not hide
/// an accelerated Vulkan/GL adapter unless the caller explicitly chose DX12 only.
pub(in crate::native) fn retry_software_visual(
    presentation: Dx12SwapchainKind,
    backends: Option<Backends>,
    software: bool,
) -> bool {
    software
        && presentation == Dx12SwapchainKind::DxgiFromVisual
        && backends.is_none_or(|set| !set.difference(Backends::DX12).is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_visual_preserves_accelerated_backend_fallback_and_explicit_dx12() {
        use Dx12SwapchainKind::{DxgiFromHwnd as Hwnd, DxgiFromVisual as Visual};
        assert!(retry_software_visual(Visual, None, true));
        assert!(retry_software_visual(
            Visual,
            Some(Backends::DX12 | Backends::VULKAN),
            true
        ));
        assert!(!retry_software_visual(Visual, Some(Backends::DX12), true));
        assert!(!retry_software_visual(Visual, None, false));
        assert!(!retry_software_visual(Hwnd, None, true));
    }

    #[test]
    fn windows_presentation_explicit_hwnd_and_other_backends_keep_redirection() {
        use Dx12SwapchainKind::{DxgiFromHwnd as Hwnd, DxgiFromVisual as Visual};
        assert_eq!(presentation_attempts(true, None, Some(Hwnd)), [Hwnd]);
        assert_eq!(
            presentation_attempts(true, Some(Backends::VULKAN), Some(Visual)),
            [Hwnd]
        );
        assert_eq!(presentation_attempts(false, None, None), [Hwnd]);
        assert_eq!(
            presentation_attempts(true, Some(Backends::DX12), Some(Visual)),
            [Visual, Hwnd]
        );
    }

    #[test]
    fn windows_presentation_defaults_to_visual_then_recreates_for_hwnd() {
        assert_eq!(
            presentation_attempts(true, None, None),
            [
                Dx12SwapchainKind::DxgiFromVisual,
                Dx12SwapchainKind::DxgiFromHwnd
            ]
        );
    }
}
