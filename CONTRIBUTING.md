# Contributing to OdyTTY

OdyTTY is developed in the open in a public repository. These are the working
conventions for changes and commits. See `DEVLOG.md` for current state, `TODO.md`
for the milestone checklist, and `SPEC.md` for durable product/architecture
decisions.

## Scope discipline

- Keep changes small and reviewable, tied to a milestone in `TODO.md`.
- Preserve the separation between terminal correctness (the owned core) and the
  Odyssey visual/experience layer. Visual experiments must not destabilize core
  behavior.
- Prefer adding deterministic tests for new terminal behavior over manual checks.

## Pre-commit gate

Before every commit, run through this gate and stop if anything is unclear:

1. **Inspect the staged diff.** Review exactly what is staged
   (`git diff --cached`); stage only the files the change intends.
2. **Run the relevant tests.** For core or harness changes, run `cargo test`.
   The default suite is deterministic and host-independent; the live-PTY smoke
   test stays `#[ignore]`d (`cargo test -- --ignored` to run it explicitly).
3. **Check formatting:** `cargo fmt --check`.
4. **Check whitespace:** `git diff --cached --check` (no trailing whitespace or
   conflict markers).
5. **Scan staged content for secrets.** No credentials, API keys, tokens,
   private hostnames/URLs, personal data, or local-only configuration.
6. **Keep local-only files out.** Machine-local config, generated credentials,
   private notes, `.env*`, and editor/agent scratch files stay untracked.

## Public repository safety

This is a hard publishing boundary. Never commit, push, paste, or summarize
secrets, credentials, private hostnames/URLs, personal data, or local-only
configuration. If anything looks ambiguous, stop and confirm before committing.

## Commit, push, and devlog cadence

- Commit at noteworthy milestones: a passing work packet, a docs/process
  checkpoint, or a prototype slice. Avoid noisy partial commits, but do not let
  finished work sit uncommitted.
- Update `DEVLOG.md` as part of each work packet (what landed, verified
  `cargo test` / `cargo fmt --check` status, remaining gaps) so the running
  record stays in lockstep with the code.
- Write clear commit messages describing what changed and why.
- Push after each completed packet, once the tree is clean, `cargo test` and
  `cargo fmt --check` pass, public docs and `DEVLOG.md` match the state of the
  project, and tracked content has been scanned for secrets or local-only data.
  Frequent pushed commits are preferred so the public history is a living record
  of development; the public-repo safety boundary is the gate, not deliberate
  infrequency.
