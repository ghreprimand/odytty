// SPDX-License-Identifier: GPL-3.0-only
//
// ID3/U5 background-image pass. A full-window textured quad drawn BEFORE the
// background cell quads so it sits behind all terminal content. A readability
// scrim (a solid black or white overlay at `scrim_alpha`) is blended over the
// image here, before the translucent cell layer composites on top — so the
// effective luminance behind text stays on the safe side of the theme
// background and the per-cell RV1 floor remains valid (see
// `color::readability_scrim_for`).
//
// The fullscreen quad is hardcoded from `@builtin(vertex_index)`; no vertex
// buffer is bound. The fragment outputs the window background alpha (`1.0`
// when opaque — byte-identical to the pre-transparency output; `window_alpha`
// while the window is translucent, so the desktop shows through the wallpaper
// instead of the image repainting the transparent clear opaque). The cell
// layer above provides the behind-text see-through via `cell_bg_opacity`.

struct BgImageUniform {
    // Scrim overlay strength in [0, 1]. 0.0 = image shown unscrimmed.
    scrim_alpha: f32,
    // > 0.5 selects a white scrim (light themes); otherwise a black scrim.
    scrim_is_white: f32,
    // TRANSPARENCY: window background alpha in [0, 1]. `1.0` (the default and
    // the opaque path) is byte-identical to the pre-transparency output. While
    // the window is translucent this scales the wallpaper quad so the desktop
    // shows through it rather than the image repainting the transparent clear.
    window_alpha: f32,
    _pad1: f32,
};

@group(0) @binding(0)
var bg_tex: texture_2d<f32>;
@group(0) @binding(1)
var bg_sampler: sampler;
@group(0) @binding(2)
var<uniform> u: BgImageUniform;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Two triangles covering NDC [-1, 1] x [-1, 1]; UV [0, 1] x [0, 1] with V
    // flipped so the image's top row maps to the top of the window.
    var positions = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(1.0, -1.0),  vec2<f32>(1.0, 1.0),  vec2<f32>(-1.0, 1.0),
    );
    var uvs = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 0.0),
    );
    var out: VsOut;
    out.pos = vec4<f32>(positions[vi], 0.0, 1.0);
    out.uv = uvs[vi];
    return out;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    let img = textureSample(bg_tex, bg_sampler, input.uv);
    let scrim_col = select(vec3<f32>(0.0), vec3<f32>(1.0), u.scrim_is_white > 0.5);
    let scrimmed = mix(img.rgb, scrim_col, u.scrim_alpha);
    // ALPHA_BLENDING over the scene clear (transparent while translucent)
    // premultiplies this to `(scrimmed·window_alpha, window_alpha)`, matching
    // the PreMultiplied surface composite the cell background layer targets.
    return vec4<f32>(scrimmed, u.window_alpha);
}
