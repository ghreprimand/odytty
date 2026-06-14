// SPDX-License-Identifier: GPL-3.0-only
enable dual_source_blending;

// Cell renderer variant for RGB/BGR subpixel text. Background and decoration
// quads remain opaque solids; glyph quads sample RGB coverage from an RGBA atlas
// and use dual-source blending for per-channel destination weights.

struct Viewport {
    // Physical surface size in pixels (x = width, y = height).
    size: vec2<f32>,
    // Optional ambient scanline wash: x = strength (0.0 disables), y = period.
    effect: vec2<f32>,
    // Text rendering params: x = glyph coverage gamma.
    text: vec4<f32>,
};

@group(0) @binding(0) var<uniform> viewport: Viewport;
@group(0) @binding(1) var atlas_tex: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct VsIn {
    @location(0) pos_px: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) is_glyph: f32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) is_glyph: f32,
};

struct FsOut {
    @location(0) @blend_src(0) color: vec4<f32>,
    @location(0) @blend_src(1) weight: vec4<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    let ndc_x = (in.pos_px.x / viewport.size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (in.pos_px.y / viewport.size.y) * 2.0;
    out.clip = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    out.is_glyph = in.is_glyph;
    return out;
}

fn apply_gamma(coverage: vec3<f32>) -> vec3<f32> {
    let gamma = max(viewport.text.x, 0.0001);
    if (gamma == 1.0) {
        return coverage;
    }
    return pow(coverage, vec3<f32>(1.0 / gamma));
}

@fragment
fn fs_main(in: VsOut) -> FsOut {
    var out: FsOut;
    if (in.is_glyph > 0.5) {
        let coverage = apply_gamma(textureSample(atlas_tex, atlas_sampler, in.uv).rgb);
        let weight = coverage * in.color.a;
        let alpha = max(max(weight.r, weight.g), weight.b);
        out.color = vec4<f32>(in.color.rgb, alpha);
        out.weight = vec4<f32>(weight, alpha);
        return out;
    }

    let strength = viewport.effect.x;
    let period = max(viewport.effect.y, 1.0);
    let TAU = 6.2831853;
    let trough = 0.5 - 0.5 * cos(TAU * in.clip.y / period);
    let factor = 1.0 - strength * trough;
    out.color = vec4<f32>(in.color.rgb * factor, in.color.a);
    out.weight = vec4<f32>(in.color.a);
    return out;
}
