// SPDX-License-Identifier: GPL-3.0-only
// Cell renderer: solid background quads and glyph-coverage quads.
//
// Instances carry pixel-space and UV bounds. The vertex shader expands each
// instance into two triangles and converts positions to normalized device
// coordinates, so resizing only updates the viewport uniform.

struct Viewport {
    // Physical surface size in pixels (x = width, y = height).
    size: vec2<f32>,
    // Legacy scanline-wash slot (x = strength, y = period in physical
    // pixels). Retained for uniform layout stability; the unified CRT
    // post-process is the only scanline implementation now, so this shader
    // never samples `effect`.
    effect: vec2<f32>,
    // Text rendering params: x = glyph coverage gamma. A value of 1.0 makes
    // coverage correction exactly the legacy linear blend path.
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

@vertex
fn vs_main(in: VsIn, @builtin(vertex_index) vertex_index: u32) -> VsOut {
    var out: VsOut;
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 1.0),
    );
    let corner = corners[vertex_index];
    let pos_px = mix(in.pos_px, in.end_pos_px, corner);
    // Pixel -> NDC. Y is flipped: pixel origin is top-left, NDC is bottom-left.
    let ndc_x = (pos_px.x / viewport.size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (pos_px.y / viewport.size.y) * 2.0;
    out.clip = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = mix(in.uv, in.end_uv, corner);
    out.color = in.color;
    out.is_glyph = in.is_glyph;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (in.is_glyph > 0.5) {
        // Glyphs stay crisp and full-contrast: the cell shader applies no
        // background wash (the legacy ambient path was retired into the CRT
        // post-process, which dims the full composited scene downstream).
        let coverage = textureSample(atlas_tex, atlas_sampler, in.uv).r;
        let gamma = max(viewport.text.x, 0.0001);
        var corrected = coverage;
        if (gamma != 1.0) {
            corrected = pow(coverage, 1.0 / gamma);
        }
        return vec4<f32>(in.color.rgb, in.color.a * corrected);
    }

    // Background fill. The legacy ambient scanline wash was retired here (UX5):
    // the scanline look is now produced by the unified CRT post-process, so the
    // cell shader no longer modulates the background by `viewport.effect`. This
    // is the previous off-path output (factor 1.0), so default rendering is
    // pixel-identical. `viewport.effect` is retained in the uniform for layout
    // stability but is no longer sampled.
    return in.color;
}
