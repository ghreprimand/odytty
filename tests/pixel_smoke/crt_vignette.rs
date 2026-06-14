// SPDX-License-Identifier: GPL-3.0-only
//! CPU guards for the CRT vignette post-process math.

use crate::harness::{Frame, frames_match, luminance};

const PI: f32 = std::f32::consts::PI;
const CRT_SOFT_DIM_MAX: f32 = 0.30;
const CRT_SOFT_KNEE_WIDTH: f32 = 0.08;
const CRT_DITHER_AMPLITUDE: f32 = 1.0 / 255.0;
const BAYER_8X8: [f32; 64] = [
    0.0, 48.0, 12.0, 60.0, 3.0, 51.0, 15.0, 63.0, 32.0, 16.0, 44.0, 28.0, 35.0, 19.0, 47.0, 31.0,
    8.0, 56.0, 4.0, 52.0, 11.0, 59.0, 7.0, 55.0, 40.0, 24.0, 36.0, 20.0, 43.0, 27.0, 39.0, 23.0,
    2.0, 50.0, 14.0, 62.0, 1.0, 49.0, 13.0, 61.0, 34.0, 18.0, 46.0, 30.0, 33.0, 17.0, 45.0, 29.0,
    10.0, 58.0, 6.0, 54.0, 9.0, 57.0, 5.0, 53.0, 42.0, 26.0, 38.0, 22.0, 41.0, 25.0, 37.0, 21.0,
];

#[derive(Clone, Copy)]
struct CrtParams {
    enabled: bool,
    scanline_intensity: f32,
    scanline_period: f32,
    vignette_strength: f32,
}

#[test]
fn crt_off_post_process_is_byte_identical() {
    let base = gradient_frame(16, 12);
    let off = apply_crt(
        &base,
        CrtParams {
            enabled: false,
            scanline_intensity: 0.18,
            scanline_period: 2.0,
            vignette_strength: 0.16,
        },
    );

    assert!(
        frames_match(&base, &off),
        "CRT-off path must not alter pixels"
    );
}

#[test]
fn crt_vignette_soft_knee_has_no_hard_floor_step() {
    let params = CrtParams {
        enabled: true,
        scanline_intensity: 0.18,
        scanline_period: 2.0,
        vignette_strength: 0.16,
    };
    let dims = [512.0, 512.0];
    let samples = 1024;
    let mut values = Vec::new();
    for i in (samples / 2)..samples {
        let x = i as f32 / (samples - 1) as f32;
        values.push(crt_brightness([x, 0.5], dims, params));
    }

    for pair in values.windows(2) {
        assert!(
            pair[1] <= pair[0] + 1e-6,
            "brightness should dim monotonically toward the edge: {} then {}",
            pair[0],
            pair[1]
        );
        assert!(
            (pair[1] - pair[0]).abs() < 0.01,
            "adjacent CRT brightness samples should not jump: {} -> {}",
            pair[0],
            pair[1]
        );
    }

    let crossover_values: Vec<f32> = values
        .iter()
        .copied()
        .filter(|brightness| (0.72..=0.80).contains(brightness))
        .collect();
    assert!(
        crossover_values.len() > 100,
        "test should sample the former hard-floor crossover densely"
    );
    let longest_flat_run = longest_quantized_run(&crossover_values, 1e-6);
    assert!(
        longest_flat_run <= 2,
        "soft vignette should not create a hard-floor plateau; run={longest_flat_run}"
    );
}

#[test]
fn crt_corner_brightness_stays_above_readability_floor() {
    let brightness = crt_brightness(
        [0.0, 0.0],
        [512.0, 512.0],
        CrtParams {
            enabled: true,
            scanline_intensity: 0.18,
            scanline_period: 2.0,
            vignette_strength: 0.16,
        },
    );

    assert!(
        brightness >= 0.70,
        "CRT corner brightness should stay readable, got {brightness}"
    );
}

#[test]
fn crt_dither_is_sub_byte_and_preserves_black_channels() {
    let base = solid_frame(8, 8, [0.0, 0.5, 1.0]);
    let on = apply_crt(
        &base,
        CrtParams {
            enabled: true,
            scanline_intensity: 0.0,
            scanline_period: 3.0,
            vignette_strength: 0.0,
        },
    );

    let mut min_delta = f32::INFINITY;
    let mut max_delta = f32::NEG_INFINITY;
    for y in 0..on.height {
        for x in 0..on.width {
            let before = base.pixel(x, y);
            let after = on.pixel(x, y);
            assert_eq!(after[0], 0.0, "dither must not brighten zero channels");
            for channel in 1..3 {
                let delta = after[channel] - before[channel];
                min_delta = min_delta.min(delta);
                max_delta = max_delta.max(delta);
                assert!(
                    delta.abs() <= CRT_DITHER_AMPLITUDE * 0.5,
                    "dither exceeded half an 8-bit quantum: {delta}"
                );
            }
        }
    }

    assert!(
        min_delta < 0.0,
        "ordered dither should include negative offsets"
    );
    assert!(
        max_delta > 0.0,
        "ordered dither should include positive offsets"
    );
}

fn crt_brightness(uv: [f32; 2], dims: [f32; 2], params: CrtParams) -> f32 {
    if !params.enabled {
        return 1.0;
    }

    let y_px = uv[1] * dims[1].max(1.0);
    let period = params.scanline_period.clamp(2.0, 12.0);
    let wave = 0.5 + 0.5 * ((y_px / period) * 2.0 * PI).cos();
    let scanline_dim = params.scanline_intensity.clamp(0.0, 0.18) * wave;

    let centered = [uv[0] * 2.0 - 1.0, uv[1] * 2.0 - 1.0];
    let dist2 = centered[0] * centered[0] + centered[1] * centered[1];
    let edge = smoothstep(0.25, 1.45, dist2);
    let vignette_dim = params.vignette_strength.clamp(0.0, 0.16) * edge;

    let total_dim = (1.0 - (1.0 - scanline_dim) * (1.0 - vignette_dim)).clamp(0.0, 1.0);
    let knee_start = CRT_SOFT_DIM_MAX - CRT_SOFT_KNEE_WIDTH;
    let over_knee = (total_dim - knee_start).max(0.0);
    let soft_dim = if total_dim > knee_start {
        knee_start + CRT_SOFT_KNEE_WIDTH * (1.0 - (-over_knee / CRT_SOFT_KNEE_WIDTH).exp())
    } else {
        total_dim
    };
    1.0 - soft_dim.min(CRT_SOFT_DIM_MAX)
}

fn apply_crt(frame: &Frame, params: CrtParams) -> Frame {
    let mut out = Frame {
        width: frame.width,
        height: frame.height,
        px: frame.px.clone(),
        cell_w: frame.cell_w,
        cell_h: frame.cell_h,
    };
    let dims = [frame.width as f32, frame.height as f32];
    for y in 0..frame.height {
        for x in 0..frame.width {
            let idx = y * frame.width + x;
            let uv = [
                (x as f32 + 0.5) / frame.width as f32,
                (y as f32 + 0.5) / frame.height as f32,
            ];
            let brightness = crt_brightness(uv, dims, params);
            let mut rgb = frame.px[idx].map(|channel| channel * brightness);
            if params.enabled {
                let dither = ordered_dither(x, y);
                for channel in &mut rgb {
                    if *channel > 0.0 {
                        *channel = (*channel + dither).max(0.0);
                    }
                }
            }
            out.px[idx] = rgb;
        }
    }
    out
}

fn ordered_dither(x: usize, y: usize) -> f32 {
    let index = (y & 7) * 8 + (x & 7);
    (((BAYER_8X8[index] + 0.5) / 64.0) - 0.5) * CRT_DITHER_AMPLITUDE
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn longest_quantized_run(values: &[f32], quantum: f32) -> usize {
    let mut longest = 0;
    let mut current = 0;
    let mut previous = i32::MIN;
    for &value in values {
        let quantized = (value / quantum).round() as i32;
        if quantized == previous {
            current += 1;
        } else {
            previous = quantized;
            current = 1;
        }
        longest = longest.max(current);
    }
    longest
}

fn gradient_frame(width: usize, height: usize) -> Frame {
    let mut frame = solid_frame(width, height, [0.0, 0.0, 0.0]);
    for y in 0..height {
        for x in 0..width {
            let v = ((x + y) as f32 / (width + height - 2) as f32).clamp(0.0, 1.0);
            frame.px[y * width + x] = [v, 1.0 - v * 0.5, luminance([v, v, v])];
        }
    }
    frame
}

fn solid_frame(width: usize, height: usize, color: [f32; 3]) -> Frame {
    Frame {
        width,
        height,
        px: vec![color; width * height],
        cell_w: 1,
        cell_h: 1,
    }
}
