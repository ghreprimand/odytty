// SPDX-License-Identifier: GPL-3.0-only
// Cell renderer: solid background quads and glyph-coverage quads.
//
// Vertices carry pixel-space positions; the vertex shader converts them to
// normalized device coordinates using the viewport size uniform, so resizing
// only updates the uniform and never rebuilds geometry. Glyph quads sample the
// R8 coverage atlas as alpha; background quads ignore the texture.

struct Viewport {
    // Physical surface size in pixels (x = width, y = height).
    size: vec2<f32>,
    // Optional ambient scanline wash: x = strength (0.0 disables, making the
    // effect a no-op), y = scanline period in physical pixels. Presentation
    // only; never affects glyph coverage or terminal contents.
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

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    // Pixel -> NDC. Y is flipped: pixel origin is top-left, NDC is bottom-left.
    let ndc_x = (in.pos_px.x / viewport.size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (in.pos_px.y / viewport.size.y) * 2.0;
    out.clip = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    out.is_glyph = in.is_glyph;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (in.is_glyph > 0.5) {
        // Glyphs are never touched by the ambient effect: text stays crisp and
        // full-contrast regardless of the visual setting.
        let coverage = textureSample(atlas_tex, atlas_sampler, in.uv).r;
        let gamma = max(viewport.text.x, 0.0001);
        var corrected = coverage;
        if (gamma != 1.0) {
            corrected = pow(coverage, 1.0 / gamma);
        }
        return vec4<f32>(in.color.rgb, in.color.a * corrected);
    }

    // Background fill. When the ambient wash is enabled (strength > 0) apply a
    // faint static scanline by modulating brightness with the framebuffer Y.
    // `in.clip` in the fragment stage is the pixel coordinate. At strength 0
    // the factor is exactly 1.0, so the off path is pixel-identical.
    let strength = viewport.effect.x;
    let period = max(viewport.effect.y, 1.0);
    let TAU = 6.2831853;
    let trough = 0.5 - 0.5 * cos(TAU * in.clip.y / period);
    let factor = 1.0 - strength * trough;
    return vec4<f32>(in.color.rgb * factor, in.color.a);
}
