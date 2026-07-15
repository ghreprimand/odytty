# Idle Wakeups — wgpu/Vulkan Driver Threads (Investigation)

Status: **open / tracked**. Not a committed fix. Recorded so the public claim
"I'll look into whether those can be quieted further" is on record rather than
hand-waved.

## Context

While verifying idle cost for v0.5.5 (config live-reload poll gated on focus),
a thread-by-thread measurement of a backgrounded, idle OdyTTY window surfaced
background wakeups that are **not** OdyTTY's own event loop.

Measured on Linux + NVIDIA (RTX-class GPU), Vulkan backend via `wgpu`, window
idle and unfocused, monitored from a separate terminal:

| thread (`/proc/$pid/task/*/comm`) | wakeups (voluntary ctxt switches) |
|-----------------------------------|-----------------------------------|
| main `odytty` event loop          | ~0.7/sec (post-fix; was ~1.0/sec) |
| `odytty` worker (futex-waiting)   | ~100/sec                          |
| `[vkrt] Analysis`                 | ~10/sec                           |
| `[vkps] Update`                   | ~4/sec                            |

Whole-process CPU over the same window: **~0.1% of one core**. Per-process GPU
engine utilization (`nvidia-smi pmon`, `sm` column): **flat `-` (zero)** the
entire time — no frames presented.

## What these threads are

- `[vkrt]` / `[vkps]` are **NVIDIA Vulkan driver** threads, spawned inside the
  process by the driver when a Vulkan device exists. They are not created by
  OdyTTY code.
- The futex-waiting `odytty` worker is most likely a `wgpu` internal
  device-poll / maintenance thread.

They run on the **CPU**, not the GPU, and dispatch no GPU work. This is distinct
from the failure mode described in Jakub Okoński's "Linux latency measurements
and compositor tuning" (<https://farnoy.dev/posts/linux-latency/>) — a client
presenting a frame every vblank and starving the focused window's frame budget:
OdyTTY presents nothing while idle, so the compositor-frame-budget mechanism does
not apply.

## Why this is not trivially fixable

- The driver threads live and die with the Vulkan **device**. A terminal cannot
  destroy its render device while it owns a visible window, so they cannot be
  removed from app code short of tearing down and recreating the GPU surface.
- A device teardown/recreate on unfocus/idle is possible in principle but risky:
  recreation flicker, focus-regain latency, and added complexity for a ~0.1%
  CPU saving. Not obviously worth it.

## Candidate directions (unverified)

1. Investigate whether the `wgpu` maintenance/poll thread (the ~100/sec futex
   worker) can be made to poll less aggressively or only when there is pending
   GPU work, without harming present latency.
2. Confirm the same wakeups occur under the Vulkan backend on other vendors
   (Intel/AMD/Mesa) to establish how much is NVIDIA-specific.
3. Evaluate (low priority) a release-the-device-when-idle path and measure
   focus-regain cost vs. the wakeups saved.

## Acceptance bar before claiming any reduction

A reproducible before/after thread-level measurement (same protocol as above),
showing reduced wakeups with **no** regression in present latency or
focus-regain responsiveness, plus a regression test where feasible.

---

## Decision: config live-reload uses mtime polling, not inotify

This records *why* the config-file live-reload watcher polls (and why v0.5.5
gated that poll on focus) rather than switching to OS change notifications, so
the tradeoff does not have to be re-derived next time someone asks.

### What the poll actually does

`ConfigFileFingerprint::read` (`src/settings/reload.rs`) calls `fs::metadata()`
and compares **mtime + size** only. It does **not** read or parse the file. So
each poll is a single `stat()` syscall on the resolved config path. On a path
the kernel already has in its dentry/inode cache that is sub-microsecond and
does no disk I/O — which is why the measured cost was ~0.03s of CPU over a full
minute of 1 Hz polling. The poller is deliberately dependency-free and
time-injected so the native event loop folds it into existing sleeps instead of
spawning a watcher thread (see the type's doc comment).

### Why not event-driven (inotify / the `notify` crate)?

"Reload on actual change" means the OS notifies us instead of us asking. On
Linux that is inotify. Real reasons polling was the defensible default:

- **No watcher thread, no extra dependency.** inotify means either a blocking
  thread or wiring an fd into the winit event loop — more moving parts and more
  failure modes for a feature whose whole job is "notice an occasional edit."
- **inotify watches inodes, not paths.** Many editors save atomically by writing
  a temp file and `rename()`-ing it over the original, which swaps the inode and
  silently breaks a naive watch on the file after the first save (vim is the
  classic offender). Doing it correctly means watching the *parent directory*
  and filtering events — fiddly, and easy to get subtly wrong. mtime+size
  polling sidesteps all of it and handles missing / deleted / recreated config
  files uniformly with one `stat`.

Event-driven's upside is real (truly zero idle cost, instant pickup instead of
up-to-1s latency), but it only removes the *focused* 1 Hz poll — which happens
while the user is actively using the window anyway, the cheapest time to do it.

### Old-hardware note

One `stat()/sec` is the same syscall regardless of CPU speed; even a decade-old
CPU does millions of cached `stat`s per second, so the poll work itself is noise
on any machine that can run a Vulkan/wgpu renderer at all. The cost that *does*
matter on old / battery hardware is **waking the CPU out of a deep sleep state**
once a second, which hurts idle power even when the work is trivial. That is
precisely what the v0.5.5 focus-gate removes: backgrounded, OdyTTY schedules no
config-poll timer wake of its own. The driver threads measured above still wake
periodically, so the process as a whole does not remain in a deep C-state.

### Standing recommendation

Keep mtime polling. The implementation is the cheap (stat-only) variant, its
own idle/backgrounded wake is already gated out, and inotify
would trade a tiny, well-understood cost for atomic-save/rename edge-case
complexity. If a future change *does* move to a `notify`-crate watcher, watch
the **parent directory** (not the file) and land a regression test covering the
temp-file-then-rename atomic-save path before shipping.
