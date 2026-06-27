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
from the failure mode in <https://farnoy.dev/posts/linux-latency> (a client
presenting a frame every vblank and starving the focused window's frame budget):
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
