# External security review closure (2026-09)

A public, scanner-assisted security review of the repository reported 14
findings. This ledger records the disposition of each one against the code as
it stood at the review baseline and the change (if any) landed for v0.14.0.
Scanner severity was not accepted as exploitability evidence: every entry
carries a code citation, and no entry is marked fixed without regression
coverage. Classifications:

- **True positive (TP):** a real weakness in shipped code or shipped
  distribution tooling; fixed with tests.
- **Defense in depth (DiD):** the primary gate already existed upstream; a
  second check was added at the sink so a future caller cannot bypass it.
- **Non-shipping (NS):** development, measurement, or CI tooling that is never
  part of the installed product; hardened where cheap, otherwise classified.
- **False positive (FP):** the reported weakness is not present; regression
  evidence is retained instead of a rewrite.
- **Informational (INFO):** an advisory that does not describe a
  vulnerability.

None of the 14 items was a release blocker, and none describes a remotely
reachable weakness in the terminal itself. Two shipped-surface items were true
positives and are fixed below.

| # | Area | Disposition | Change and evidence |
|---|------|-------------|---------------------|
| 1 | Theme Builder save path (`src/native/theme_builder.rs`, `save_theme_to_dir`) | DiD | `ThemeBuilder::save_request` already rejected names outside `[A-Za-z0-9_-]`. The write sink now re-validates the name at the filesystem boundary and refuses traversal, separators, controls, empty names, and any extension other than the generated `.theme`. Tests: `src/native/theme_builder_tests.rs`, `tests/security_configuration_sinks.rs`. |
| 2 | `hosts.conf` field quoting (`src/connection_hosts.rs`) | DiD | Form input already rejected control characters before save. Every append, edit, and remove serialization path now rejects control characters (CR, LF, CRLF, tab, NUL, C1) in the target and sibling fields and leaves the original file byte-identical on rejection. Tests: `mutation_rejects_control_characters_without_changing_existing_bytes` and `tests/security_configuration_sinks.rs`. |
| 3 | Remote image-paste temp name and remote create (`src/ssh_connect.rs`, `src/native/app/image_paste.rs`) | TP | The name was drawn from hasher randomization rather than a documented CSPRNG, and the remote `cat >` create was not exclusive, so a predicted name on a shared remote `/tmp` could have been pre-planted. The token now comes from `getrandom` (128 bits, hex only, fixed length) and upload fails closed with a visible notice when entropy is unavailable; the remote command runs `umask 077; set -C; cat > '<path>'` so an existing path or symlink fails instead of being followed; cleanup only ever names OdyTTY-minted `/tmp/odytty-paste-<hex>.png` paths. Linux/macOS use the ControlMaster socket; the Windows client uses `ssh.exe` without a ControlPath and no detached Windows host surface is claimed. Tests: `remote_upload_target_fails_closed_when_entropy_is_unavailable`, `remote_cleanup_only_names_minted_paths`, `tests/security_configuration_sinks.rs`. |
| 4 | `ttf-parser` RUSTSEC-2026-0192 | INFO | An "unmaintained" advisory, not a disclosed vulnerability or unsoundness. `.github/scripts/rustsec-audit.sh` carries a dated exception that fails the gate on or after 2026-10-15; a replacement parser lands only with font-corpus, shaping, MSRV, performance, and all-platform parity evidence before that date. The date is not extended. |
| 5 | `scripts/vttest-runner.py` archive extraction | NS | Already enforced member-count and size caps, absolute and parent-path refusal, and non-regular member refusal. No change. |
| 6 | `scripts/vttest-runner.py` regex use | FP | Fixed, anchored patterns over harness-controlled text; no user-supplied pattern. No change. |
| 7 | `scripts/vttest-runner.py` HTTP fetch | NS | Already HTTPS-only with a bounded streaming read and timeout. Internal state filenames are now additionally constrained at the path join. Self-test: `python3 scripts/vttest-runner.py selftest`. |
| 8 | Benchmark-protocol fixture integrity (`scripts/bench-protocol/driver.py`) | FP | Fixtures are content-hashed with SHA-256 while streamed and the preregistration digest comparison is active; the driver self-test asserts byte count and digest. Evidence retained, code unchanged. |
| 9 | Benchmark-protocol readiness spin (`driver.py`) | NS | An `exists()` readiness barrier on harness-owned temporary paths; no trust boundary is crossed. Classified, no change. |
| 10 | Benchmark-protocol HTTP fetch (`scripts/bench-protocol/w6_runner.py`) | NS | Public read-only GitHub fetch over HTTPS. Response bodies are now capped at 1 MiB with a declared-length check before the read. This does not alter measurement semantics; the protocol version is unchanged and existing results are unaffected. |
| 11 | Benchmark-protocol unused helper | NS | Measurement harness only; no release impact. Classified. |
| 12 | `scripts/mutation-summary.py` selection regex | FP | Patterns come from the tracked `scripts/mutation-batches.tsv`, not untrusted input. They are now precompiled with a length bound and an explicit invalid-pattern failure. Self-test: `python3 scripts/mutation-summary.py --self-test`. |
| 13 | Graphics-fuzz support module marked dead | FP | `src/core/graphics_fuzz_tests.rs` is compiled under `cfg(test)` and its smoke cases run in the library suite; the nightly-only `fuzz/` harness carries an intentional `allow(dead_code)` on its sink. Evidence retained, code unchanged. |
| 14 | Installer trust chain (`dist/install.sh`, README, `docs/install.md`) | TP (distribution) | `SHA256SUMS` was already signed with the published release key, but the promoted path fetched the installer from mutable `master` and never verified the signature. The installer is now published as a version-pinned release asset covered by `SHA256SUMS` and `SHA256SUMS.minisig`; it authenticates the manifest with the pinned public key before trusting any checksum and fails closed without `minisign` (an explicit `--insecure-skip-signature` opt-out states its tradeoff). Documentation puts signature verification first and labels any convenience command as untrusted. Checks: `bash -n`, `--dry-run`, CI shell job. |

Related documents: [threat model](threat-model.md), [release process](release.md),
[installation](install.md).
