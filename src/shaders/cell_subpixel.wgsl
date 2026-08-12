// SPDX-License-Identifier: GPL-3.0-only
enable dual_source_blending;

// Cell renderer variant for RGB/BGR subpixel text. Background and decoration
// quads remain opaque solids; glyph quads sample RGB coverage from an RGBA atlas
// and use dual-source blending for per-channel destination weights.

struct Viewport {
    // Physical surface size in pixels (x = width, y = height).
    size: vec2<f32>,
    // Legacy scanline-wash slot (x = strength, y = period). Retained for
    // uniform layout stability; never sampled — the CRT post-process is the
    // only scanline implementation now.
    effect: vec2<f32>,
    // Text rendering params: x = glyph coverage gamma.
    text: vec4<f32>,
};

@group(0) @binding(0) var<uniform> viewport: Viewport;
@group(0) @binding(1) var atlas_tex: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct VsIn {
    @location(0) pos_px: vec2<f32>,
    @location(1) end_pos_px: vec2<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) end_uv: vec2<f32>,
    @location(4) color: vec4<f32>,
    @location(5) is_glyph: f32,
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
fn vs_main(in: VsIn, @builtin(vertex_index) vertex_index: u32) -> VsOut {
    var out: VsOut;
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 1.0),
    );
    let corner = corners[vertex_index];
    let pos_px = mix(in.pos_px, in.end_pos_px, corner);
    let ndc_x = (pos_px.x / viewport.size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (pos_px.y / viewport.size.y) * 2.0;
    out.clip = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = mix(in.uv, in.end_uv, corner);
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

    // Legacy ambient scanline wash retired here (UX5): the scanline look is now
    // produced by the unified CRT post-process, so the background is no longer
    // modulated by `viewport.effect`. This is the previous off-path output
    // (factor 1.0), so default rendering is pixel-identical. `viewport.effect`
    // is retained in the uniform for layout stability but is no longer sampled.
    out.color = in.color;
    out.weight = vec4<f32>(in.color.a);
    return out;
}
