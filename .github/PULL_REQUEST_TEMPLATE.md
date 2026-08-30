## Summary

Describe the problem and the completed change. Keep the pull request focused on
one accepted issue or proposal.

## Related issue or proposal

Link the issue this implements (for example, `Closes #123`). Substantial new
work should have scope agreement before implementation.

## Verification

List the exact checks run and their outcomes. Record skipped, ignored,
unsupported, or unavailable-platform checks distinctly.

## Platform and behavior impact

- Linux:
- macOS:
- Windows:
- Deliberate user-visible behavior changes, or `none`:
- Documentation or screenshots updated, or `not applicable`:

## Checklist

- [ ] I understand and can explain the submitted changes and their interaction
      with the surrounding code.
- [ ] The change is focused, preserves terminal correctness, and includes
      regression coverage where practical.
- [ ] `cargo fmt --check`,
      `cargo clippy --all-targets --locked -- -D warnings`, and
      `cargo test --locked` pass, or every unavailable check is explained above.
- [ ] Public documentation, the current monthly devlog, and its index match the
      changed behavior.
- [ ] Every commit carries a `Signed-off-by:` line matching its author identity
      (`git commit -s`) as required by the DCO.
- [ ] The diff contains no secrets, private hosts or URLs, personal data,
      identifying local paths, machine-local configuration, or generated
      credentials.
