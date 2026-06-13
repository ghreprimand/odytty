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

@fragment
fn fs_composite_bloom(in: VsOut) -> @location(0) vec4<f32> {
    let scene = textureSample(source_tex, source_sampler, in.uv);
    let glow = textureSample(bloom_tex, bloom_sampler, in.uv).rgb;
    return vec4<f32>(scene.rgb + glow * bloom.intensity, scene.a);
}
