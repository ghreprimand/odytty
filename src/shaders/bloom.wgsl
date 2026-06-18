// SPDX-License-Identifier: GPL-3.0-only
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct Bloom {
    threshold: f32,
    intensity: f32,
    radius: f32,
    _pad: f32,
};

struct Crt {
    enabled: f32,
    scanline_intensity: f32,
    scanline_period: f32,
    vignette_strength: f32,
    curvature: f32,
};

@group(0) @binding(0) var source_tex: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@group(0) @binding(2) var<uniform> bloom: Bloom;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    let pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let uv = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var out: VsOut;
    out.pos = vec4<f32>(pos[vertex_index], 0.0, 1.0);
    out.uv = uv[vertex_index];
    return out;
}

fn luma(rgb: vec3<f32>) -> f32 {
    return dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@fragment
fn fs_bright(in: VsOut) -> @location(0) vec4<f32> {
    let scene = textureSample(source_tex, source_sampler, in.uv).rgb;
    let y = luma(scene);
    let factor = max(y - bloom.threshold, 0.0) / max(y, 0.0001);
    return vec4<f32>(scene * factor, 1.0);
}

fn blur_sample(uv: vec2<f32>, direction: vec2<f32>) -> vec3<f32> {
    let dims = vec2<f32>(textureDimensions(source_tex));
    let texel = direction / max(dims, vec2<f32>(1.0, 1.0));
    let scale = max(bloom.radius, 0.5) / 3.0;
    var color = textureSample(source_tex, source_sampler, uv).rgb * 0.22702703;
    color += textureSample(source_tex, source_sampler, uv + texel * scale * 1.0).rgb * 0.19459459;
    color += textureSample(source_tex, source_sampler, uv - texel * scale * 1.0).rgb * 0.19459459;
    color += textureSample(source_tex, source_sampler, uv + texel * scale * 2.0).rgb * 0.12162162;
    color += textureSample(source_tex, source_sampler, uv - texel * scale * 2.0).rgb * 0.12162162;
    color += textureSample(source_tex, source_sampler, uv + texel * scale * 3.0).rgb * 0.05405405;
    color += textureSample(source_tex, source_sampler, uv - texel * scale * 3.0).rgb * 0.05405405;
    color += textureSample(source_tex, source_sampler, uv + texel * scale * 4.0).rgb * 0.01621622;
    color += textureSample(source_tex, source_sampler, uv - texel * scale * 4.0).rgb * 0.01621622;
    return color;
}

@fragment
fn fs_blur_h(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(blur_sample(in.uv, vec2<f32>(1.0, 0.0)), 1.0);
}

@fragment
fn fs_blur_v(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(blur_sample(in.uv, vec2<f32>(0.0, 1.0)), 1.0);
}

@group(0) @binding(3) var bloom_tex: texture_2d<f32>;
@group(0) @binding(4) var bloom_sampler: sampler;
@group(0) @binding(5) var<uniform> crt: Crt;

const PI: f32 = 3.141592653589793;
const CRT_SOFT_DIM_MAX: f32 = 0.30;
const CRT_SOFT_KNEE_WIDTH: f32 = 0.08;
const CRT_DITHER_AMPLITUDE: f32 = 1.0 / 255.0;

fn crt_brightness(uv: vec2<f32>) -> f32 {
    if crt.enabled <= 0.5 {
        return 1.0;
    }

    let dims = vec2<f32>(textureDimensions(source_tex));
    let y_px = uv.y * max(dims.y, 1.0);
    let period = clamp(crt.scanline_period, 2.0, 12.0);
    let wave = 0.5 + 0.5 * cos((y_px / period) * 2.0 * PI);
    let scanline_dim = clamp(crt.scanline_intensity, 0.0, 0.35) * wave;

    let centered = uv * 2.0 - vec2<f32>(1.0, 1.0);
    let edge = smoothstep(0.25, 1.45, dot(centered, centered));
    let vignette_dim = clamp(crt.vignette_strength, 0.0, 0.45) * edge;

    let total_dim = clamp(1.0 - (1.0 - scanline_dim) * (1.0 - vignette_dim), 0.0, 1.0);
    let knee_start = CRT_SOFT_DIM_MAX - CRT_SOFT_KNEE_WIDTH;
    let over_knee = max(total_dim - knee_start, 0.0);
    let soft_dim = select(
        total_dim,
        knee_start + CRT_SOFT_KNEE_WIDTH * (1.0 - exp(-over_knee / CRT_SOFT_KNEE_WIDTH)),
        total_dim > knee_start,
    );
    return 1.0 - min(soft_dim, CRT_SOFT_DIM_MAX);
}

fn crt_dither(pos: vec2<f32>) -> f32 {
    let bayer = array<f32, 64>(
         0.0, 48.0, 12.0, 60.0,  3.0, 51.0, 15.0, 63.0,
        32.0, 16.0, 44.0, 28.0, 35.0, 19.0, 47.0, 31.0,
         8.0, 56.0,  4.0, 52.0, 11.0, 59.0,  7.0, 55.0,
        40.0, 24.0, 36.0, 20.0, 43.0, 27.0, 39.0, 23.0,
         2.0, 50.0, 14.0, 62.0,  1.0, 49.0, 13.0, 61.0,
        34.0, 18.0, 46.0, 30.0, 33.0, 17.0, 45.0, 29.0,
        10.0, 58.0,  6.0, 54.0,  9.0, 57.0,  5.0, 53.0,
        42.0, 26.0, 38.0, 22.0, 41.0, 25.0, 37.0, 21.0,
    );
    let pixel = vec2<u32>(u32(pos.x), u32(pos.y));
    let index = (pixel.y & 7u) * 8u + (pixel.x & 7u);
    return (((bayer[index] + 0.5) / 64.0) - 0.5) * CRT_DITHER_AMPLITUDE;
}

// Barrel distortion for CRT screen curvature. Maps the flat NDC-space UV to
// a curved sample location so the composited frame appears to bulge toward
// the viewer at the center and recede at the edges. `amount` is clamped
// server-side (0.0–0.5); the shader re-clamps defensively and returns the
// original UV at zero so the flat path stays pixel-identical. UVs are then
// clamped to [0,1] to avoid black seams at the frame border.
fn crt_curved_uv(uv: vec2<f32>) -> vec2<f32> {
    let amount = clamp(crt.curvature, 0.0, 0.5);
    if amount <= 0.0 {
        return uv;
    }
    let ndc = uv * 2.0 - vec2<f32>(1.0, 1.0);
    let r2 = dot(ndc, ndc);
    let corner_r2 = 2.0;
    let scale = (1.0 + amount * r2) / (1.0 + amount * corner_r2);
    let curved = ndc * scale;
    return clamp(curved * 0.5 + vec2<f32>(0.5, 0.5), vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0));
}

@fragment
fn fs_composite_bloom(in: VsOut) -> @location(0) vec4<f32> {
    let curved = crt_curved_uv(in.uv);
    let scene = textureSample(source_tex, source_sampler, curved);
    var rgb = scene.rgb;
    if bloom.intensity > 0.0 {
        let glow = textureSample(bloom_tex, bloom_sampler, curved).rgb;
        rgb += glow * bloom.intensity;
    }
    rgb *= crt_brightness(in.uv);
    if crt.enabled > 0.5 {
        let channel_gate = select(vec3<f32>(0.0), vec3<f32>(1.0), rgb > vec3<f32>(0.0));
        rgb = max(rgb + vec3<f32>(crt_dither(in.pos.xy)) * channel_gate, vec3<f32>(0.0));
    }
    return vec4<f32>(rgb, scene.a);
}
