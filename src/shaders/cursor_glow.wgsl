// SPDX-License-Identifier: GPL-3.0-only
// Shape-aware cursor focus aura. One expanded quad carries the source cursor
// rectangle and evaluates a smooth rounded-box distance field per fragment.

struct Viewport {
    size: vec2<f32>,
    effect: vec2<f32>,
    text: vec4<f32>,
};

@group(0) @binding(0) var<uniform> viewport: Viewport;

struct VsIn {
    @location(0) pos_px: vec2<f32>,
    @location(1) source_rect: vec4<f32>,
    @location(2) aura: vec4<f32>,
    @location(3) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) @interpolate(flat) source_rect: vec4<f32>,
    @location(1) @interpolate(flat) aura: vec4<f32>,
    @location(2) @interpolate(flat) color: vec4<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    let ndc_x = (in.pos_px.x / viewport.size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (in.pos_px.y / viewport.size.y) * 2.0;
    out.clip = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.source_rect = in.source_rect;
    out.aura = in.aura;
    out.color = in.color;
    return out;
}

fn sd_round_box(point: vec2<f32>, half_extent: vec2<f32>, radius: f32) -> f32 {
    let q = abs(point) - half_extent + vec2<f32>(radius);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0))) - radius;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let source_center = (in.source_rect.xy + in.source_rect.zw) * 0.5;
    let source_half_extent = (in.source_rect.zw - in.source_rect.xy) * 0.5;
    let radius = max(in.aura.x, 0.001);
    let corner_radius = max(in.aura.y, 0.0);
    let peak_alpha = clamp(in.aura.z, 0.0, 1.0);
    let sd = sd_round_box(in.clip.xy - source_center, source_half_extent, corner_radius);
    let outside = max(sd, 0.0);
    let aa = max(fwidth(sd), 1.0);
    let exterior = smoothstep(-aa, aa, sd);
    let normalized = outside / radius;
    let falloff = exp2(-4.0 * normalized * normalized);
    let alpha = peak_alpha * falloff * exterior;
    return vec4<f32>(in.color.rgb, alpha);
}
