# Parser and graphics fuzz workspace

This standalone `cargo-fuzz` workspace exercises four external-input seams
without adding fuzz-only dependencies to the release workspace:

- `parser_dispatch`: compares whole-buffer, byte-at-a-time, and deterministic
  chunk dispatch through `OdyParser`, then checks parser recovery with a
  sentinel.
- `terminal_stream`: compares observable `Terminal` state across the same feed
  schedules, with bounded scrollback and graphics storage, then checks RIS
  reset and sentinel output.
- `kitty_graphics`: routes one bounded Kitty APC through `Terminal`, keeps
  named transports disabled, and checks image-store, placement, raw-command,
  and response bounds.
- `sixel_decode`: calls the public Sixel decoder with bounded input and checks
  dimensions, checked pixel arithmetic, and RGBA length.

The workspace owns its Cargo workspace boundary and lockfile. It pins
`libfuzzer-sys` to `0.4.13`, `cargo-fuzz` commands to `0.13.2`, and Rust to
`nightly-2026-07-15`. The dated nightly resolved on Linux x86_64 to
`rustc 1.99.0-nightly (da80ed070 2026-07-14)`. The normal project MSRV remains
Rust 1.96.

## Tool setup and structural checks

Install the command at the documented version:

```text
cargo install cargo-fuzz --version 0.13.2 --locked
cargo fuzz --version
```

Run the structural checks from this directory:

```text
cargo +nightly-2026-07-15 fuzz check
cargo +nightly-2026-07-15 fuzz build parser_dispatch
cargo +nightly-2026-07-15 fuzz build terminal_stream
cargo +nightly-2026-07-15 fuzz build kitty_graphics
cargo +nightly-2026-07-15 fuzz build sixel_decode
```

## Bounded local runs

Create the ignored artifact directories before a run:

```text
mkdir -p artifacts/parser_dispatch artifacts/terminal_stream
mkdir -p artifacts/kitty_graphics artifacts/sixel_decode
```

Run one target and one worker at a time. Each command has both a shell deadline
and libFuzzer time, memory, per-input, and input-length limits. The surrounding
job must also enforce the repository's CPU and memory limits; do not run these
commands on a host where those limits cannot be established.

```text
CARGO_BUILD_JOBS=4 RUST_TEST_THREADS=1 timeout --kill-after=15s 45s cargo +nightly-2026-07-15 fuzz run parser_dispatch corpus/parser_dispatch -- -max_total_time=30 -timeout=5 -rss_limit_mb=8192 -workers=1 -jobs=1 -max_len=65536 -dict=dictionaries/parser_dispatch.dict -artifact_prefix=artifacts/parser_dispatch/
CARGO_BUILD_JOBS=4 RUST_TEST_THREADS=1 timeout --kill-after=15s 45s cargo +nightly-2026-07-15 fuzz run terminal_stream corpus/terminal_stream -- -max_total_time=30 -timeout=5 -rss_limit_mb=8192 -workers=1 -jobs=1 -max_len=65536 -dict=dictionaries/terminal_stream.dict -artifact_prefix=artifacts/terminal_stream/
CARGO_BUILD_JOBS=4 RUST_TEST_THREADS=1 timeout --kill-after=15s 45s cargo +nightly-2026-07-15 fuzz run kitty_graphics corpus/kitty_graphics -- -max_total_time=30 -timeout=8 -rss_limit_mb=8192 -workers=1 -jobs=1 -max_len=2097152 -dict=dictionaries/kitty_graphics.dict -artifact_prefix=artifacts/kitty_graphics/
CARGO_BUILD_JOBS=4 RUST_TEST_THREADS=1 timeout --kill-after=15s 45s cargo +nightly-2026-07-15 fuzz run sixel_decode corpus/sixel_decode -- -max_total_time=30 -timeout=8 -rss_limit_mb=8192 -workers=1 -jobs=1 -max_len=1048576 -dict=dictionaries/sixel_decode.dict -artifact_prefix=artifacts/sixel_decode/
```

An input is also clipped inside each target. The command-line limits are not
the sole resource boundary.

The scheduled Linux x86_64 smoke uses
`.github/scripts/run-coverage-fuzz.sh`, which establishes the repository's
transient cgroup limits before running the same four targets sequentially. The
workflow in `.github/workflows/coverage-fuzz.yml` retains logs, the evolved
corpora, and crash artifacts for 14 days. An unavailable cgroup is a failed
run, not an unbounded fallback.

## Corpus and crash handling

`corpus.toml` records the origin and digest of every retained seed. All seeds
are repository literals or synthetic protocol examples. Private transcripts,
images, settings, paths, and host-derived inputs are prohibited. Newly
generated hash-named corpus entries and all artifacts remain ignored until
manual inspection confirms that they are public-safe.

Preserve a failure artifact and reproduce it with the exact target and pinned
toolchain. Minimize it before any closure:

```text
cargo +nightly-2026-07-15 fuzz tmin TARGET artifacts/TARGET/ARTIFACT
```

After minimization:

1. Inspect the bytes for private or host-derived content.
2. Reproduce the failure with the minimized artifact.
3. Add a focused ordinary test in
   `tests/fuzz_regressions_parser_graphics.rs` and a synthetic fixture in
   `tests/fixtures/fuzz/parser_graphics/`.
4. Confirm the ordinary test fails before the correction and passes after it.
5. Add a sanitized minimized seed to `corpus/TARGET/` and record its digest and
   origin in `corpus.toml`.

No crash is closed solely because a corpus entry stops reproducing. Corpus
minimization may be run only after retained seeds and artifacts are backed up:

```text
cargo +nightly-2026-07-15 fuzz cmin TARGET corpus/TARGET
```

## Platform scope

These `cargo-fuzz` commands are supported on Linux and macOS hosts with the
pinned nightly. They are unsupported on Windows. Windows execution must not be
inferred from Linux or macOS results; `windows-latest` remains a separate
project gate. The stable regression fixture for named Kitty transports includes
a synthetic Windows-style path and executes through the ordinary cross-platform
test suite without accessing that path.
