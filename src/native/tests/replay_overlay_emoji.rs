// SPDX-License-Identifier: GPL-3.0-only
//! Emoji in the Session Replay preview when the preview is a window-level
//! overlay layer (reordering on in a single pane, or any split tab).
//!
//! Those frames hand the open overlay to the GPU as `OverlayTop` layers. Each
//! layer now builds the color glyphs its cells own and draws them over its own
//! panel, before the next layer, so a recorded emoji shows as the color glyph
//! the live terminal draws instead of a missing-glyph box, and a later layer
//! (the rename band) still covers it. The tests open replay through its
//! chord, take the layers the real redraw handed to the GPU, and draw them
//! through the production builders, draw order, and pipelines with the
//! licensed synthetic COLR fixture (`tests/fixtures/fonts`), reading back the
//! pixels.

use super::*;
use crate::emoji::{ColorGlyphAtlas, EmojiFont, EmojiRasterizer};
use crate::grid::{ColorGlyphRun, ColorGlyphVertex, Vertex};
use crate::native::gpu::{
    OverlayDrawStep, OverlayTop, OverlayTopSegment, build_overlay_top_vertices_into,
    create_atlas_bind_group, create_atlas_texture, create_cell_pipeline,
    create_color_atlas_bind_group, create_color_atlas_texture, create_color_glyph_pipeline,
    for_each_overlay_draw_step, prepare_overlay_top_glyphs, splice_overlay_top_color,
};
use wgpu::util::DeviceExt as _;

/// The fixture's only mapped scalar (U+1F525), default emoji presentation.
const FIRE: char = '\u{1F525}';
const TARGET: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

struct Glyphs {
    atlas: GlyphAtlas,
    fonts: StyleFonts,
    rasterizer: EmojiRasterizer,
    color_atlas: ColorGlyphAtlas,
}

fn glyphs() -> Glyphs {
    let fonts = StyleFonts::regular(text::load_bundled_font().expect("bundled font"));
    let atlas = GlyphAtlas::build(fonts.font_for(FontStyle::Regular), 16.0);
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fonts/color-emoji-colr-v0.ttf");
    let rasterizer = EmojiRasterizer::from_font(EmojiFont::load(path).expect("COLR fixture"));
    let color_atlas = ColorGlyphAtlas::new(atlas.cell);
    Glyphs {
        atlas,
        fonts,
        rasterizer,
        color_atlas,
    }
}

/// Record one frame holding the fixture emoji on the active session, then open
/// replay with its chord (Ctrl+Shift+R).
fn open_replay_with_emoji(app: &mut App) {
    let recorder = app.sessions_mut_for_test().active().recorder.clone();
    let mut recorded = Terminal::new(40, 6);
    recorded.advance(format!("fire {FIRE} ok").as_bytes());
    recorder.set_enabled(true);
    recorder.record(recorded.snapshot());
    app.drive_char_with_mods_for_test('r', true, true);
    assert_eq!(app.overlay_signature_for_test().mode, OverlayMode::Replay);
}

/// A single-pane window with reordering on and replay open over a frame
/// holding the emoji. Returns the layers its redraw handed to the GPU.
fn single_pane_layers(glyphs: &Glyphs, rename: bool) -> Vec<(Snapshot, [f32; 2])> {
    let (mut app, _terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(60, 16),
        Settings {
            bidi_reorder: true,
            ..Settings::default()
        },
    );
    app.set_test_cell_for_test(glyphs.atlas.cell);
    open_replay_with_emoji(&mut app);
    if rename {
        assert!(app.begin_rename_tab_for_test(0));
        assert!(
            app.overlay_open_for_test(),
            "replay stays open under the band"
        );
    }
    app.redraw_single_pane_probe_for_test()
        .expect("single-pane frame");
    app.overlay_top_layers_for_test().to_vec()
}

/// The cell holding the emoji in a layer, if any.
fn fire_cell(snapshot: &Snapshot) -> Option<(usize, usize)> {
    let columns = snapshot.dimensions.columns;
    snapshot
        .cells
        .iter()
        .position(|cell| cell.ch == FIRE && !cell.wide_continuation)
        .map(|index| (index / columns, index % columns))
}

struct Drawn {
    cells: Vec<Vertex>,
    color: Vec<ColorGlyphVertex>,
    segments: Vec<OverlayTopSegment>,
    runs: Vec<Vec<ColorGlyphRun>>,
    size: [u32; 2],
}

/// Prepare and build every layer through the production overlay builders.
fn build(glyphs: &mut Glyphs, layers: &[(Snapshot, [f32; 2])]) -> Drawn {
    let runs: Vec<Vec<ColorGlyphRun>> = layers
        .iter()
        .map(|(snapshot, _)| {
            prepare_overlay_top_glyphs(
                &mut glyphs.atlas,
                &glyphs.fonts,
                &mut glyphs.rasterizer,
                &mut glyphs.color_atlas,
                snapshot,
            )
        })
        .collect();
    let mut drawn = Drawn {
        cells: Vec::new(),
        color: Vec::new(),
        segments: Vec::new(),
        runs,
        size: [1, 1],
    };
    let (mut cells, mut color) = (Vec::new(), Vec::new());
    for ((snapshot, origin), runs) in layers.iter().zip(&drawn.runs) {
        let overlay = OverlayTop {
            snapshot,
            origin: *origin,
            treatment: crate::grid::BackgroundTreatmentParams::default(),
        };
        build_overlay_top_vertices_into(
            &mut cells,
            &mut color,
            &glyphs.atlas,
            &glyphs.color_atlas,
            &overlay,
            runs,
            1.0,
        );
        drawn.cells.extend_from_slice(&cells);
        drawn.color.extend_from_slice(&color);
        drawn.segments.push(OverlayTopSegment {
            cells: cells.len() as u32,
            color: color.len() as u32,
        });
        let cell = glyphs.atlas.cell;
        drawn.size[0] =
            drawn.size[0].max(origin[0] as u32 + snapshot.dimensions.columns as u32 * cell.width);
        drawn.size[1] =
            drawn.size[1].max(origin[1] as u32 + snapshot.dimensions.rows as u32 * cell.height);
    }
    drawn
}

/// The draw steps the frame issues for `drawn`, in order.
fn steps(drawn: &Drawn) -> Vec<OverlayDrawStep> {
    let mut steps = Vec::new();
    for_each_overlay_draw_step(
        0,
        drawn.cells.len() as u32,
        0,
        drawn.color.len() as u32,
        &drawn.segments,
        |step| steps.push(step),
    );
    steps
}

fn layout(device: &wgpu::Device, viewport: wgpu::ShaderStages) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("replay-emoji-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: viewport,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

/// Draw `drawn` with the production cell and color-glyph pipelines in the
/// production trailing-segment order and read the target back. With
/// `with_color` off the color steps are skipped: the controlled reference
/// with every panel pixel the emoji is drawn over.
fn render(
    gpu: &crate::native::gpu::HeadlessGpuFixture,
    glyphs: &Glyphs,
    drawn: &Drawn,
    with_color: bool,
) -> Vec<[u8; 4]> {
    let (device, queue) = (&gpu.device, &gpu.queue);
    let [width, height] = drawn.size;
    let viewport = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("replay-emoji-viewport"),
        contents: bytemuck::bytes_of(&ViewportUniform {
            size: [width as f32, height as f32],
            reserved: [0.0, 1.0],
            text: [1.0, 0.0, 0.0, 0.0],
        }),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("replay-emoji-sampler"),
        ..Default::default()
    });
    let cell_layout = layout(device, wgpu::ShaderStages::VERTEX_FRAGMENT);
    let color_layout = layout(device, wgpu::ShaderStages::VERTEX);
    let atlas_texture = create_atlas_texture(device, queue, &glyphs.atlas);
    let color_texture = create_color_atlas_texture(device, queue, &glyphs.color_atlas);
    let cell_group =
        create_atlas_bind_group(device, &cell_layout, &viewport, &atlas_texture, &sampler);
    let color_group =
        create_color_atlas_bind_group(device, &color_layout, &viewport, &color_texture, &sampler);
    let cell_pipeline = create_cell_pipeline(device, TARGET, &cell_layout, SubpixelMode::Off);
    let color_pipeline = create_color_glyph_pipeline(device, TARGET, &color_layout);
    let cell_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("replay-emoji-cells"),
        contents: bytemuck::cast_slice(&drawn.cells),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let color_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("replay-emoji-color"),
        contents: bytemuck::cast_slice(&drawn.color),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("replay-emoji-target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("replay-emoji-encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("replay-emoji-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        for step in steps(drawn) {
            match step {
                OverlayDrawStep::Cells(start, end) => {
                    pass.set_pipeline(&cell_pipeline);
                    pass.set_bind_group(0, &cell_group, &[]);
                    pass.set_vertex_buffer(0, cell_buf.slice(..));
                    pass.draw(0..crate::grid::VERTS_PER_QUAD as u32, start..end);
                }
                OverlayDrawStep::Color(start, end) if with_color => {
                    pass.set_pipeline(&color_pipeline);
                    pass.set_bind_group(0, &color_group, &[]);
                    pass.set_vertex_buffer(0, color_buf.slice(..));
                    pass.draw(0..crate::grid::VERTS_PER_QUAD as u32, start..end);
                }
                OverlayDrawStep::Color(..) => {}
            }
        }
    }
    let bytes_per_row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("replay-emoji-readback"),
        size: u64::from(bytes_per_row) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));
    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        tx.send(result).ok();
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll");
    rx.recv().expect("map callback").expect("map readback");
    let mapped = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((width * height) as usize);
    for y in 0..height as usize {
        for x in 0..width as usize {
            let at = y * bytes_per_row as usize + x * 4;
            pixels.push([mapped[at], mapped[at + 1], mapped[at + 2], mapped[at + 3]]);
        }
    }
    drop(mapped);
    readback.unmap();
    pixels
}

fn decode(srgb: u8) -> f32 {
    let c = f32::from(srgb) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn encode(linear: f32) -> u8 {
    let c = linear.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

/// The pixel at the middle of the emoji's color quad, and the premultiplied
/// atlas texel the color pipeline samples there.
fn glyph_center(glyphs: &Glyphs, quad: &ColorGlyphVertex) -> ([usize; 2], [u8; 4]) {
    let x = ((quad.pos[0] + quad.end_pos[0]) / 2.0).floor();
    let y = ((quad.pos[1] + quad.end_pos[1]) / 2.0).floor();
    let fx = (x + 0.5 - quad.pos[0]) / (quad.end_pos[0] - quad.pos[0]);
    let fy = (y + 0.5 - quad.pos[1]) / (quad.end_pos[1] - quad.pos[1]);
    let u = quad.uv[0] + (quad.end_uv[0] - quad.uv[0]) * fx;
    let v = quad.uv[1] + (quad.end_uv[1] - quad.uv[1]) * fy;
    let atlas = &glyphs.color_atlas;
    let tx = ((u * atlas.width as f32) as usize).min(atlas.width as usize - 1);
    let ty = ((v * atlas.height as f32) as usize).min(atlas.height as usize - 1);
    let at = (ty * atlas.width as usize + tx) * 4;
    let texel = [
        atlas.data[at],
        atlas.data[at + 1],
        atlas.data[at + 2],
        atlas.data[at + 3],
    ];
    ([x as usize, y as usize], texel)
}

/// The premultiplied texel composited over the reference pixel, in the
/// linear space an sRGB target blends in.
fn expected_over(texel: [u8; 4], under: [u8; 4]) -> [u8; 3] {
    let alpha = f32::from(texel[3]) / 255.0;
    [0, 1, 2].map(|channel| {
        encode(f32::from(texel[channel]) / 255.0 + decode(under[channel]) * (1.0 - alpha))
    })
}

fn assert_close(actual: [u8; 4], expected: [u8; 3], what: &str) {
    for channel in 0..3 {
        assert!(
            actual[channel].abs_diff(expected[channel]) <= 3,
            "{what}: pixel {actual:?}, expected {expected:?}"
        );
    }
}

/// Draw `layers` and check that the replay layer's emoji is the fixture's
/// color glyph over its panel. Returns the drawn layers, the emoji pixel's
/// position, and the full readback.
fn assert_emoji_drawn_over_panel(
    gpu: &crate::native::gpu::HeadlessGpuFixture,
    glyphs: &mut Glyphs,
    layers: &[(Snapshot, [f32; 2])],
    what: &str,
) -> (Drawn, usize, Vec<[u8; 4]>) {
    let replay = layers
        .iter()
        .position(|(snapshot, _)| fire_cell(snapshot).is_some())
        .unwrap_or_else(|| panic!("{what}: a layer shows the recorded emoji"));
    let drawn = build(glyphs, layers);
    let first_color: u32 = drawn.segments[..replay]
        .iter()
        .map(|segment| segment.color)
        .sum();
    let pixels = render(gpu, glyphs, &drawn, true);
    let reference = render(gpu, glyphs, &drawn, false);
    let quad = drawn
        .color
        .get(first_color as usize)
        .unwrap_or_else(|| panic!("{what}: the replay layer builds no color glyph"));
    let ([x, y], texel) = glyph_center(glyphs, quad);
    assert!(
        texel[3] > 0 && texel[0] > texel[2],
        "{what}: the fixture texel is opaque red-yellow ink {texel:?}"
    );
    let index = y * drawn.size[0] as usize + x;
    assert_close(
        pixels[index],
        expected_over(texel, reference[index]),
        &format!("{what}: emoji over its panel"),
    );
    assert_ne!(
        pixels[index], reference[index],
        "{what}: the emoji changes the panel pixel"
    );
    let (row, column) = fire_cell(&layers[replay].0).expect("emoji cell");
    assert_eq!(
        drawn.runs[replay]
            .iter()
            .filter(|run| run.row == row && run.column == column)
            .count(),
        1,
        "{what}: exactly one color run owns the emoji cell"
    );
    assert_eq!(
        drawn.segments[replay].color, 1,
        "{what}: the replay layer draws exactly one color glyph"
    );
    (drawn, index, pixels)
}

/// Reordering on in a single pane: the replay preview is its own overlay
/// layer, and its recorded emoji draws as the color glyph.
#[test]
fn replay_emoji_draws_in_color_over_the_bidi_overlay_layer() {
    const TEST: &str = "replay_emoji_draws_in_color_over_the_bidi_overlay_layer";
    let Some(gpu) =
        crate::native::test_support::gpu_device::headless_device(TEST, "replay-emoji-test", false)
    else {
        return;
    };
    let mut glyphs = glyphs();
    let layers = single_pane_layers(&glyphs, false);
    assert_eq!(layers.len(), 1, "replay is the one window layer");
    assert_emoji_drawn_over_panel(&gpu, &mut glyphs, &layers, "bidi single pane");
}

/// Reordering off in a single pane keeps replay in the terminal snapshot,
/// whose own color-glyph runs draw the emoji; no window layer is handed over.
#[test]
fn replay_with_reordering_off_paints_into_the_terminal_snapshot() {
    let (mut app, _terminal) = headless_app_for_test();
    app.set_test_cell_for_test(CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    });
    open_replay_with_emoji(&mut app);
    app.redraw_single_pane_probe_for_test()
        .expect("single-pane frame");
    assert!(app.overlay_top_layers_for_test().is_empty());
}

/// A split tab hands replay over as the window layer whatever the reordering
/// setting; the emoji draws as the color glyph there too.
#[test]
fn replay_emoji_draws_in_color_over_the_split_overlay_layer() {
    const TEST: &str = "replay_emoji_draws_in_color_over_the_split_overlay_layer";
    let Some(gpu) = crate::native::test_support::gpu_device::headless_device(
        TEST,
        "replay-emoji-split-test",
        false,
    ) else {
        return;
    };
    let mut glyphs = glyphs();
    let cell = glyphs.atlas.cell;
    let (mut app, _terminal) = headless_app_for_test();
    let dims = Dimensions::new(30, 12);
    let pane = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(std::io::sink())));
    app.seed_headless_split_pane_for_test(true, pane, writer, dims);
    let focused = app.active_session_token_for_test();
    for token in app.active_tab_pane_tokens_for_test() {
        app.focus_session_token_for_test(token);
        app.set_test_cell_for_test(cell);
        app.set_test_surface_for_test(
            60 * cell.width,
            12 * cell.height,
            crate::native::WindowPadding::ZERO,
        );
    }
    app.focus_session_token_for_test(focused);
    app.reflow_active_panes_for_test();
    open_replay_with_emoji(&mut app);
    let _ = app.redraw_multipane_probe_for_test();
    let layers = app.overlay_top_layers_for_test().to_vec();
    assert_eq!(layers.len(), 1, "replay is the split frame's window layer");
    assert_emoji_drawn_over_panel(&gpu, &mut glyphs, &layers, "split");
}

/// With the rename band opened over replay, the frame draws the replay layer,
/// its emoji, then the band: the emoji's color draw sits between the two
/// layers, and an opaque layer drawn over the emoji covers it.
#[test]
fn a_later_overlay_layer_covers_the_replay_emoji() {
    const TEST: &str = "a_later_overlay_layer_covers_the_replay_emoji";
    let Some(gpu) = crate::native::test_support::gpu_device::headless_device(
        TEST,
        "replay-emoji-occlusion-test",
        false,
    ) else {
        return;
    };
    let mut glyphs = glyphs();
    let layers = single_pane_layers(&glyphs, true);
    assert_eq!(layers.len(), 2, "replay, then the rename band");
    let (drawn, _, _) =
        assert_emoji_drawn_over_panel(&gpu, &mut glyphs, &layers, "replay under the band");
    let cells0 = drawn.segments[0].cells;
    let total = drawn.cells.len() as u32;
    assert_eq!(
        steps(&drawn),
        vec![
            OverlayDrawStep::Cells(0, cells0),
            OverlayDrawStep::Color(0, 1),
            OverlayDrawStep::Cells(cells0, total),
        ],
        "each layer's color glyphs draw before the next layer"
    );

    // An opaque blank layer over the replay panel hides the emoji: the pixel
    // is the cover's own, with or without the color pass.
    let (replay, origin) = layers[0].clone();
    let mut cover = replay.clone();
    for cell in &mut cover.cells {
        cell.ch = ' ';
        cell.wide_continuation = false;
    }
    let covered = [(replay, origin), (cover, origin)];
    let drawn = build(&mut glyphs, &covered);
    assert_eq!(
        drawn.segments[1].color, 0,
        "the blank cover has no color glyph"
    );
    let with = render(&gpu, &glyphs, &drawn, true);
    let without = render(&gpu, &glyphs, &drawn, false);
    let ([x, y], _) = glyph_center(&glyphs, &drawn.color[0]);
    let index = y * drawn.size[0] as usize + x;
    assert_eq!(
        with[index], without[index],
        "a later opaque layer covers the earlier layer's emoji"
    );
}

/// The trailing segment order: each layer's cells, then its color glyphs,
/// then the floating rail strip that follows the layers in the main buffer.
#[test]
fn overlay_draw_steps_interleave_each_layer_with_its_color_glyphs() {
    let segments = [
        OverlayTopSegment { cells: 6, color: 2 },
        OverlayTopSegment { cells: 4, color: 0 },
        OverlayTopSegment { cells: 3, color: 1 },
    ];
    let mut steps = Vec::new();
    for_each_overlay_draw_step(10, 30, 5, 8, &segments, |step| steps.push(step));
    assert_eq!(
        steps,
        vec![
            OverlayDrawStep::Cells(10, 16),
            OverlayDrawStep::Color(5, 7),
            OverlayDrawStep::Cells(16, 20),
            OverlayDrawStep::Cells(20, 23),
            OverlayDrawStep::Color(7, 8),
            OverlayDrawStep::Cells(23, 30),
        ]
    );
    // No layers: the whole trailing segment is one cell draw, as before.
    let mut steps = Vec::new();
    for_each_overlay_draw_step(10, 14, 5, 5, &[], |step| steps.push(step));
    assert_eq!(steps, vec![OverlayDrawStep::Cells(10, 14)]);
    // Counts larger than the buffers are clamped, never drawn past the end.
    let mut steps = Vec::new();
    for_each_overlay_draw_step(10, 12, 5, 5, &segments, |step| steps.push(step));
    assert_eq!(steps, vec![OverlayDrawStep::Cells(10, 12)]);
}

/// A cursor-only or synchronized-output rebuild replaces the cursor's color
/// glyphs and keeps the retained overlay layers' color glyphs after them.
#[test]
fn cursor_rebuild_keeps_the_retained_overlay_color_glyphs() {
    let quad = |x: f32| ColorGlyphVertex {
        pos: [x, 0.0],
        end_pos: [x + 1.0, 1.0],
        uv: [0.0, 0.0],
        end_uv: [1.0, 1.0],
        alpha: 1.0,
        _pad: [0.0; 3],
    };
    let retained = vec![quad(50.0)];
    // Content 0, stale cursor 1, stale overlay 2 from the previous frame.
    let mut buffer = vec![quad(0.0), quad(1.0), quad(2.0)];
    // The cursor rebuild truncates to the content and appends a new cursor.
    buffer.truncate(1);
    buffer.push(quad(9.0));
    assert!(splice_overlay_top_color(&mut buffer, 2, &retained));
    assert_eq!(buffer, vec![quad(0.0), quad(9.0), quad(50.0)]);
    // Without layers nothing follows the cursor and nothing is uploaded.
    assert!(!splice_overlay_top_color(&mut buffer, 2, &[]));
    assert_eq!(buffer.len(), 2);
}

/// An emoji the color font cannot draw (the fixture maps only U+1F525) gets no
/// color run, so its layer is built exactly as before: the monochrome
/// fallback glyph, which is what a platform without color coverage for that
/// emoji shows, and no color glyph.
#[test]
fn an_emoji_without_color_coverage_keeps_the_monochrome_layer() {
    let mut glyphs = glyphs();
    let mut recorded = Terminal::new(12, 1);
    recorded.advance("a \u{1F600} b".as_bytes());
    let snapshot = recorded.snapshot();
    let runs = prepare_overlay_top_glyphs(
        &mut glyphs.atlas,
        &glyphs.fonts,
        &mut glyphs.rasterizer,
        &mut glyphs.color_atlas,
        &snapshot,
    );
    assert!(runs.is_empty(), "no color run without color coverage");
    let overlay = OverlayTop {
        snapshot: &snapshot,
        origin: [3.0, 5.0],
        treatment: crate::grid::BackgroundTreatmentParams::default(),
    };
    let (mut cells, mut color) = (Vec::new(), Vec::new());
    build_overlay_top_vertices_into(
        &mut cells,
        &mut color,
        &glyphs.atlas,
        &glyphs.color_atlas,
        &overlay,
        &runs,
        1.0,
    );
    assert!(color.is_empty());
    let mut text_only = Vec::new();
    crate::grid::build_cell_vertices_with_focus_dim_and_origin_into(
        &mut text_only,
        &snapshot,
        &glyphs.atlas,
        &[],
        0.0,
        overlay.origin,
        overlay.treatment,
        1.0,
        1.0,
        None,
        crate::grid::ChromePin::NONE,
    );
    assert_eq!(
        cells, text_only,
        "the layer is the pre-change text-only build"
    );
}
