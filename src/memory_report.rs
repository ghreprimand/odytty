// SPDX-License-Identifier: GPL-3.0-only
//! Env-gated memory-attribution diagnostic: byte totals for the subsystems
//! OdyTTY itself controls, reported alongside the process resident set.
//!
//! Memory claims about this terminal are only worth as much as the measurement
//! behind them. This module is the measurement: it names every allocation
//! OdyTTY decides the size of — glyph atlases, the background image, the
//! post-process targets, per-pane grid and scrollback, the graphics-protocol
//! image store, and the vertex buffers — and reports the arithmetic difference
//! between that total and the process resident set as an **explicitly labelled
//! remainder**. The remainder is never folded into a subsystem, because doing so
//! would turn an unexplained byte into an explained one without evidence.
//!
//! Two rules shape the shape of the record:
//!
//! * **GPU bytes sit alongside the resident set, never subtracted from it.**
//!   Where a texture's bytes physically live — device memory, a write-combined
//!   host mapping, or a driver-side shadow copy — is the driver's business and
//!   varies by adapter, backend, and allocator. Reporting them as a separate
//!   column keeps the resident figure the honest, comparable one.
//! * **The host-side remainder is signed.** Allocated-but-not-yet-faulted pages
//!   make it possible for the attributed total to exceed the resident set. A
//!   saturating subtraction would hide that; a signed remainder preserves the
//!   identity `accounted + remainder == resident` exactly, in both directions.
//!
//! This is a passive diagnostic, OFF by default and zero cost when off: a single
//! atomic load, no thread, no allocation, no file handle, and no added event-loop
//! wake unless [`sample_interval`] returns `Some`. When enabled it appends one
//! line per sample to `std::env::temp_dir()/odytty-memory-report.log`. A file
//! (not stderr) because the Windows build is a GUI-subsystem application with no
//! visible stderr; the log is retrieved from the temp dir afterward.
//!
//! # Privacy / public-repo safety
//! Every field is a byte total, a count, or a fixed identifier drawn from a
//! closed set defined in this file. No field can hold a string originating in a
//! session: there is no `String` in [`MemoryReport`], so cell contents, titles,
//! paths, and environment values have no way in even by mistake. The log
//! destination is the OS temp directory (no developer path is baked into the
//! source) and the env var name is generic.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Env gate. `1`/`true` enables sampling at [`DEFAULT_SAMPLE_SECS`]; a positive
/// integer enables it at that many seconds. Anything else (or absence) is off.
const GATE: &str = "ODYTTY_MEMORY_REPORT";

/// Sampling period used when the gate is set to `1`/`true` rather than to an
/// explicit second count. Long enough that the sampler's own wake is
/// insignificant against an idle terminal's cost, short enough that a
/// ten-minute idle capture yields sixty samples.
const DEFAULT_SAMPLE_SECS: u64 = 10;

/// Upper bound on the configured sampling period, so a mistyped value cannot
/// park the diagnostic effectively forever while still reporting as enabled.
const MAX_SAMPLE_SECS: u64 = 3600;

/// Parsed once from [`GATE`]; every later call is a single atomic load.
static INTERVAL: OnceLock<Option<Duration>> = OnceLock::new();
/// Monotonic per-process sample counter, included in every line so captures are
/// orderable without relying on the clock.
static SEQ: AtomicU64 = AtomicU64::new(0);
/// Set once after the header line is written, so repeated samples in one process
/// append data lines without re-emitting the legend.
static HEADER_WRITTEN: OnceLock<()> = OnceLock::new();

/// The sampling period, or `None` when the diagnostic is off. Reads the env var
/// exactly once; every subsequent call is a single atomic load.
pub fn sample_interval() -> Option<Duration> {
    *INTERVAL.get_or_init(|| parse_gate(std::env::var(GATE).ok().as_deref()))
}

/// Pure parse of the gate value, factored out so the accepted grammar is
/// testable without touching process environment.
pub fn parse_gate(value: Option<&str>) -> Option<Duration> {
    let raw = value?.trim().to_ascii_lowercase();
    if raw == "1" || raw == "true" {
        return Some(Duration::from_secs(DEFAULT_SAMPLE_SECS));
    }
    let secs: u64 = raw.parse().ok()?;
    if secs == 0 || secs > MAX_SAMPLE_SECS {
        return None;
    }
    Some(Duration::from_secs(secs))
}

/// Where the process-level resident figures came from. A closed set: these are
/// the only identifiers that can ever appear in the `rss_source` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResidentSource {
    /// Linux: `/proc/self/status` (`VmRSS` / `VmHWM`).
    ProcStatus,
    /// Windows: `GetProcessMemoryInfo` (`WorkingSetSize` / `PeakWorkingSetSize`).
    WindowsPsapi,
    /// macOS: `getrusage(RUSAGE_SELF)` — peak only; the current resident set is
    /// not exposed by that call and is reported `unmeasured`.
    Getrusage,
    /// The platform exposes no figure this build reads. Never approximated.
    Unmeasured,
}

impl ResidentSource {
    /// The fixed token written into a report line.
    pub fn token(self) -> &'static str {
        match self {
            Self::ProcStatus => "proc_status",
            Self::WindowsPsapi => "windows_psapi",
            Self::Getrusage => "getrusage",
            Self::Unmeasured => "unmeasured",
        }
    }
}

/// Process-level resident figures. `None` means the platform did not expose the
/// value — recorded as `unmeasured`, never inferred from another platform and
/// never estimated from the attributed subtotals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessMemory {
    pub resident_bytes: Option<u64>,
    pub peak_resident_bytes: Option<u64>,
    pub source: Option<ResidentSource>,
}

impl ProcessMemory {
    /// The source token, defaulting to `unmeasured` when nothing was read.
    pub fn source_token(&self) -> &'static str {
        self.source.unwrap_or(ResidentSource::Unmeasured).token()
    }
}

/// Byte breakdown of one scrollback store, or of several summed together.
///
/// Scrollback holds two structurally different things, and reporting them as
/// one sum makes it impossible to tell which one a change moved. The logical
/// ring is the content; the projection is a memoized *second physical copy* of
/// that content at the current width. They grow for different reasons and are
/// reclaimed by different means, so they are separate figures.
///
/// `ring_slack` is a **breakdown of `ring`, not an addition to it**: it is the
/// reserved-but-unused capacity already counted inside `ring`. Summing the
/// three would double-count, which is why [`HostBytes::accounted`] takes the
/// first two and deliberately excludes the third.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollbackBytes {
    /// Logical-line ring: its own slots plus each line's cell and button-span
    /// allocations, measured by capacity.
    pub ring: u64,
    /// Memoized physical projection of the ring at the current width.
    pub projection: u64,
    /// Portion of `ring` that is reserved-but-unused capacity (capacity minus
    /// length). A subset of `ring`, never added to it.
    pub ring_slack: u64,
}

impl ScrollbackBytes {
    /// Field-wise saturating sum, for accumulating across panes and buffers.
    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            ring: self.ring.saturating_add(other.ring),
            projection: self.projection.saturating_add(other.projection),
            ring_slack: self.ring_slack.saturating_add(other.ring_slack),
        }
    }
}

/// Host (resident-set-backed) bytes OdyTTY controls the size of.
///
/// Every field is an allocation this project decides the extent of. Anything
/// allocated by a dependency, the allocator's own overhead, the mapped binary,
/// and driver mappings are deliberately absent — they land in the remainder,
/// where they are visible as unexplained rather than silently attributed.
///
/// Most fields are additive and sum into [`HostBytes::accounted`]. One is not:
/// `scrollback_ring_slack` is a breakdown of a figure already counted. The
/// distinction is enforced by construction — see `accounted`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostBytes {
    /// Monochrome/subpixel glyph atlas CPU coverage bitmap.
    pub glyph_atlas_bitmap: u64,
    /// Colour glyph (emoji) atlas CPU bitmap.
    pub color_glyph_atlas_bitmap: u64,
    /// Decoded background-image RGBA retained on the CPU side after upload.
    pub background_image_buffer: u64,
    /// Visible grid cells across every live pane.
    pub grid_cells: u64,
    /// Logical-line scrollback ring across every live pane. Content only — the
    /// memoized physical view is `scrollback_projection`.
    pub scrollback_ring: u64,
    /// Memoized physical projection of scrollback across every live pane: a
    /// second copy of the ring's content wrapped to the current width.
    pub scrollback_projection: u64,
    /// Decoded bytes held by the terminal-graphics image store across panes.
    pub graphics_image_store: u64,
    /// CPU-side vertex staging vectors the renderer builds each frame and
    /// retains between frames.
    pub vertex_staging: u64,
    /// **Non-additive breakdown figure.** The reserved-but-unused portion of
    /// `scrollback_ring`, reported so reclaimable slack is visible without
    /// having to infer it. Already counted inside `scrollback_ring`; adding it
    /// to the total would double-count, so `accounted` excludes it explicitly.
    pub scrollback_ring_slack: u64,
}

impl HostBytes {
    /// Total attributed host bytes. Saturating, so no field combination can
    /// overflow the sum into a smaller number.
    ///
    /// The exhaustive destructuring is deliberate: a field added to
    /// [`HostBytes`] without deciding whether it is additive fails to compile
    /// here. That matters because this struct now carries one field that must
    /// *not* be summed, and a silently-included breakdown figure would inflate
    /// the attributed total and shrink the remainder — the exact failure mode
    /// the remainder exists to make visible.
    pub fn accounted(&self) -> u64 {
        let Self {
            glyph_atlas_bitmap,
            color_glyph_atlas_bitmap,
            background_image_buffer,
            grid_cells,
            scrollback_ring,
            scrollback_projection,
            graphics_image_store,
            vertex_staging,
            // Deliberately excluded: a subset of `scrollback_ring`.
            scrollback_ring_slack: _,
        } = *self;
        [
            glyph_atlas_bitmap,
            color_glyph_atlas_bitmap,
            background_image_buffer,
            grid_cells,
            scrollback_ring,
            scrollback_projection,
            graphics_image_store,
            vertex_staging,
        ]
        .into_iter()
        .fold(0u64, u64::saturating_add)
    }
}

/// GPU-object bytes OdyTTY controls the size of, reported **alongside** the
/// resident set and never subtracted from it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuBytes {
    pub glyph_atlas_texture: u64,
    pub color_glyph_atlas_texture: u64,
    pub background_image_texture: u64,
    /// Post-process offscreen + bright + ping render targets.
    pub post_process_textures: u64,
    /// Terminal-graphics placement textures held by the image layer.
    pub graphics_textures: u64,
    /// Cell, cursor, colour-glyph and image vertex buffers plus the viewport
    /// uniform.
    pub vertex_buffers: u64,
}

impl GpuBytes {
    /// Total attributed GPU-object bytes. Saturating, for the same reason as
    /// [`HostBytes::accounted`].
    pub fn accounted(&self) -> u64 {
        [
            self.glyph_atlas_texture,
            self.color_glyph_atlas_texture,
            self.background_image_texture,
            self.post_process_textures,
            self.graphics_textures,
            self.vertex_buffers,
        ]
        .into_iter()
        .fold(0u64, u64::saturating_add)
    }
}

/// One memory-attribution sample.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryReport {
    pub process: ProcessMemory,
    pub host: HostBytes,
    pub gpu: GpuBytes,
    /// Number of live panes the per-pane totals were summed over. A count, so a
    /// report is interpretable without knowing the session layout.
    pub panes: u64,
}

impl MemoryReport {
    /// The explicitly labelled host remainder: resident bytes this report does
    /// **not** attribute to a named subsystem.
    ///
    /// Signed and exact. `accounted + remainder == resident` holds by
    /// construction whenever the resident set was measured. A negative value is
    /// meaningful, not a bug: it means the attributed allocations exceed the
    /// resident set, which is what happens when a buffer is reserved but not yet
    /// faulted in, or when pages have been reclaimed. `None` when the platform
    /// did not expose a resident figure — the remainder is then `unmeasured`,
    /// never zero and never guessed.
    pub fn unaccounted_host_bytes(&self) -> Option<i64> {
        let resident = i64::try_from(self.process.resident_bytes?).ok()?;
        let accounted = i64::try_from(self.host.accounted()).ok()?;
        Some(resident - accounted)
    }
}

/// Column legend for the data lines, written once per process as a header.
const LEGEND: &str = concat!(
    "seq epoch_ms panes rss_source rss_bytes rss_peak_bytes ",
    "host_accounted_bytes host_unaccounted_bytes ",
    "host_glyph_atlas_bitmap host_color_glyph_atlas_bitmap host_background_image_buffer ",
    "host_grid_cells host_scrollback_ring host_scrollback_projection ",
    "host_scrollback_ring_slack host_graphics_image_store host_vertex_staging ",
    "gpu_accounted_bytes gpu_glyph_atlas_texture gpu_color_glyph_atlas_texture ",
    "gpu_background_image_texture gpu_post_process_textures gpu_graphics_textures ",
    "gpu_vertex_buffers"
);

/// Render an optional byte total: the number, or the fixed token `unmeasured`.
fn opt(value: Option<u64>) -> String {
    match value {
        Some(v) => v.to_string(),
        None => "unmeasured".to_string(),
    }
}

/// Format one report line. Pure — no environment, no I/O, no global state
/// beyond the supplied `seq` and `epoch_ms` — so the arithmetic and the field
/// set are testable without races.
pub fn format_report_line(seq: u64, epoch_ms: u128, report: &MemoryReport) -> String {
    let host = &report.host;
    let gpu = &report.gpu;
    format!(
        "seq={seq} epoch_ms={epoch_ms} panes={panes} rss_source={src} rss_bytes={rss} \
rss_peak_bytes={peak} host_accounted_bytes={acc} host_unaccounted_bytes={unacc} \
host_glyph_atlas_bitmap={h1} host_color_glyph_atlas_bitmap={h2} \
host_background_image_buffer={h3} host_grid_cells={h4} host_scrollback_ring={h5} \
host_scrollback_projection={h5b} host_scrollback_ring_slack={h5c} \
host_graphics_image_store={h6} host_vertex_staging={h7} gpu_accounted_bytes={gacc} \
gpu_glyph_atlas_texture={g1} gpu_color_glyph_atlas_texture={g2} \
gpu_background_image_texture={g3} gpu_post_process_textures={g4} \
gpu_graphics_textures={g5} gpu_vertex_buffers={g6}",
        panes = report.panes,
        src = report.process.source_token(),
        rss = opt(report.process.resident_bytes),
        peak = opt(report.process.peak_resident_bytes),
        acc = host.accounted(),
        unacc = match report.unaccounted_host_bytes() {
            Some(v) => v.to_string(),
            None => "unmeasured".to_string(),
        },
        h1 = host.glyph_atlas_bitmap,
        h2 = host.color_glyph_atlas_bitmap,
        h3 = host.background_image_buffer,
        h4 = host.grid_cells,
        h5 = host.scrollback_ring,
        h5b = host.scrollback_projection,
        h5c = host.scrollback_ring_slack,
        h6 = host.graphics_image_store,
        h7 = host.vertex_staging,
        gacc = gpu.accounted(),
        g1 = gpu.glyph_atlas_texture,
        g2 = gpu.color_glyph_atlas_texture,
        g3 = gpu.background_image_texture,
        g4 = gpu.post_process_textures,
        g5 = gpu.graphics_textures,
        g6 = gpu.vertex_buffers,
    )
}

/// Append one report line to the temp-dir log. Callers only reach this when
/// [`sample_interval`] is `Some`, so the off path costs one atomic load. Any I/O
/// error is silently ignored — a diagnostic must never perturb the terminal.
pub fn append_report(report: &MemoryReport) {
    use std::io::Write;

    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let epoch_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = std::env::temp_dir().join("odytty-memory-report.log");
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };

    if HEADER_WRITTEN.set(()).is_ok() {
        let _ = writeln!(
            file,
            "# odytty-memory-report v{} start_epoch_ms={epoch_ms}\n# {LEGEND}",
            env!("CARGO_PKG_VERSION"),
        );
    }

    let _ = writeln!(file, "{}", format_report_line(seq, epoch_ms, report));
}

/// Parse one `Name:   <n> kB` line of a Linux `/proc/<pid>/status` file into
/// bytes. A field that is absent or unparseable yields `None`, which surfaces
/// as `unmeasured` — never as zero and never as an approximation.
///
/// Declared unconditionally (rather than inside the Linux platform module) so
/// the parse is unit-testable on every CI leg, including the Windows and macOS
/// legs where the interface it parses does not exist.
pub fn parse_status_field_kib(status: &str, key: &str) -> Option<u64> {
    status
        .lines()
        .find_map(|line| line.strip_prefix(key))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse::<u64>().ok())
        .and_then(|kib| kib.checked_mul(1024))
}

/// Read the process resident set and its high-water mark from the platform.
///
/// Each platform reads its own native source; a figure one platform does not
/// expose is reported `None` (`unmeasured`) rather than substituted from
/// another platform's interface or derived from the attributed subtotals.
pub fn read_process_memory() -> ProcessMemory {
    platform::read_process_memory()
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{ProcessMemory, ResidentSource, parse_status_field_kib};

    /// Linux exposes both the current resident set (`VmRSS`) and its high-water
    /// mark (`VmHWM`) in `/proc/self/status`, in kibibytes.
    pub(super) fn read_process_memory() -> ProcessMemory {
        let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
            return ProcessMemory {
                resident_bytes: None,
                peak_resident_bytes: None,
                source: Some(ResidentSource::Unmeasured),
            };
        };
        ProcessMemory {
            resident_bytes: parse_status_field_kib(&status, "VmRSS:"),
            peak_resident_bytes: parse_status_field_kib(&status, "VmHWM:"),
            source: Some(ResidentSource::ProcStatus),
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{ProcessMemory, ResidentSource};

    /// macOS: `getrusage` reports the resident high-water mark in bytes. The
    /// *current* resident set needs a Mach task-info call this build does not
    /// link, so it is reported `unmeasured` rather than approximated by the peak
    /// (which would overstate a settled process) or by the attributed subtotals
    /// (which would make the remainder meaningless).
    pub(super) fn read_process_memory() -> ProcessMemory {
        // SAFETY: `getrusage` writes into a fully initialized local `rusage`
        // and returns a status; no pointer escapes and no invariant is assumed.
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
        if rc != 0 {
            return ProcessMemory {
                resident_bytes: None,
                peak_resident_bytes: None,
                source: Some(ResidentSource::Unmeasured),
            };
        }
        ProcessMemory {
            resident_bytes: None,
            peak_resident_bytes: u64::try_from(usage.ru_maxrss).ok(),
            source: Some(ResidentSource::Getrusage),
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::{ProcessMemory, ResidentSource};
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;

    /// Windows: `GetProcessMemoryInfo` exposes both the current working set and
    /// its peak, in bytes. This is the platform's own interface — no `/proc`
    /// figure is inferred for Windows, and no Linux value is carried across.
    pub(super) fn read_process_memory() -> ProcessMemory {
        let size = u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>()).unwrap_or(0);
        let mut counters = PROCESS_MEMORY_COUNTERS {
            cb: size,
            ..Default::default()
        };
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no
        // close, and the counters struct is fully initialized with its own size
        // in `cb` and passed again as the `cb` argument, which is the documented
        // contract for this call.
        let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) };
        if ok.is_err() {
            return ProcessMemory {
                resident_bytes: None,
                peak_resident_bytes: None,
                source: Some(ResidentSource::Unmeasured),
            };
        }
        ProcessMemory {
            resident_bytes: u64::try_from(counters.WorkingSetSize).ok(),
            peak_resident_bytes: u64::try_from(counters.PeakWorkingSetSize).ok(),
            source: Some(ResidentSource::WindowsPsapi),
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod platform {
    use super::{ProcessMemory, ResidentSource};

    /// Any other target: nothing is read and nothing is guessed.
    pub(super) fn read_process_memory() -> ProcessMemory {
        ProcessMemory {
            resident_bytes: None,
            peak_resident_bytes: None,
            source: Some(ResidentSource::Unmeasured),
        }
    }
}
