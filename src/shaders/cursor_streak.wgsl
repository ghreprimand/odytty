// SPDX-License-Identifier: GPL-3.0-only

struct Viewport {
    size: vec2<f32>,
    effect: vec2<f32>,
    text: vec4<f32>,
};

@group(0) @binding(0) var<uniform> viewport: Viewport;

struct VsIn {
    @location(0) pos_px: vec2<f32>,
    @location(1) segment: vec4<f32>,
    @location(2) ribbon: vec4<f32>,
    @location(3) color: vec4<f32>,
    @location(4) clip_rect: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) segment: vec4<f32>,
    @location(1) @interpolate(flat) ribbon: vec4<f32>,
    @location(2) @interpolate(flat) color: vec4<f32>,
    @location(3) @interpolate(flat) clip_rect: vec4<f32>,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
    let ndc = vec2<f32>(
        input.pos_px.x / viewport.size.x * 2.0 - 1.0,
        1.0 - input.pos_px.y / viewport.size.y * 2.0,
    );
    var output: VsOut;
    output.pos = vec4<f32>(ndc, 0.0, 1.0);
    output.segment = input.segment;
    output.ribbon = input.ribbon;
    output.color = input.color;
    output.clip_rect = input.clip_rect;
    return output;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    let pixel = input.pos.xy;
    if pixel.x < input.clip_rect.x || pixel.y < input.clip_rect.y ||
       pixel.x >= input.clip_rect.z || pixel.y >= input.clip_rect.w {
        discard;
    }

    let tail = input.segment.xy;
    let destination = input.segment.zw;
    let axis = destination - tail;
    let length_sq = max(dot(axis, axis), 0.0001);
    let projected = dot(pixel - tail, axis) / length_sq;
    let u = clamp(projected, 0.0, 1.0);
    let nearest = tail + axis * u;
    let radius = max(1.0, mix(input.ribbon.x, input.ribbon.y, u));
    let distance = length(pixel - nearest);
    let aa = max(fwidth(distance), 0.75);
    let coverage = 1.0 - smoothstep(radius - aa, radius + aa, distance);
    let destination_fade = 1.0 - smoothstep(0.88, 1.0, u);
    let alpha = input.ribbon.z * coverage * destination_fade;
    if alpha <= 0.0001 {
        discard;
    }
    return vec4<f32>(input.color.rgb, alpha);
}
