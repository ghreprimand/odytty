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

fn crt_brightness(uv: vec2<f32>) -> f32 {
    if crt.enabled <= 0.5 {
        return 1.0;
    }

    let dims = vec2<f32>(textureDimensions(source_tex));
    let y_px = uv.y * max(dims.y, 1.0);
    let period = clamp(crt.scanline_period, 2.0, 12.0);
    let wave = 0.5 + 0.5 * cos((y_px / period) * 2.0 * PI);
    let scanline_dim = clamp(crt.scanline_intensity, 0.0, 0.18) * wave;

    let centered = uv * 2.0 - vec2<f32>(1.0, 1.0);
    let edge = smoothstep(0.25, 1.45, dot(centered, centered));
    let vignette_dim = clamp(crt.vignette_strength, 0.0, 0.16) * edge;

    return max(0.75, (1.0 - scanline_dim) * (1.0 - vignette_dim));
}

@fragment
fn fs_composite_bloom(in: VsOut) -> @location(0) vec4<f32> {
    let scene = textureSample(source_tex, source_sampler, in.uv);
    var rgb = scene.rgb;
    if bloom.intensity > 0.0 {
        let glow = textureSample(bloom_tex, bloom_sampler, in.uv).rgb;
        rgb += glow * bloom.intensity;
    }
    rgb *= crt_brightness(in.uv);
    return vec4<f32>(rgb, scene.a);
}
