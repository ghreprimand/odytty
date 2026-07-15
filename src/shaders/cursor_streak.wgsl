// SPDX-License-Identifier: GPL-3.0-only

struct Viewport {
    size: vec2<f32>,
    effect: vec2<f32>,
    text: vec4<f32>,
};

@group(0) @binding(0) var<uniform> viewport: Viewport;

struct VsIn {
    @location(0) pos_px: vec2<f32>,
    @location(1) source_rect: vec4<f32>,
    @location(2) follower: vec4<f32>,
    @location(3) color: vec4<f32>,
    @location(4) clip_rect: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) source_rect: vec4<f32>,
    @location(1) @interpolate(flat) follower: vec4<f32>,
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
    output.source_rect = input.source_rect;
    output.follower = input.follower;
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

    let outside = max(input.source_rect.xy - pixel, pixel - input.source_rect.zw);
    let signed_distance = max(outside.x, outside.y);
    let aa = max(fwidth(signed_distance), 0.75);
    let coverage = 1.0 - smoothstep(-aa, aa, signed_distance);
    let alpha = input.follower.x * coverage;
    if alpha <= 0.0001 {
        discard;
    }
    return vec4<f32>(input.color.rgb, alpha);
}
