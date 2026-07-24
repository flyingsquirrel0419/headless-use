## Summary

<!-- Brief description of what this PR changes and why. -->

## Linked issue

<!-- Closes #123, or "N/A" if none. -->

## Verification

- [ ] `cargo fmt --check` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes
- [ ] `cargo test --workspace -- --test-threads=2` passes
- [ ] Added/updated tests for new behavior
- [ ] Updated docs (README, docs/, CHANGELOG) if user-facing

## User-facing impact

<!-- What changes for users? Breaking changes? Migration needed? -->

## Key invariants check

- [ ] Input uses real `Input.*` CDP events (not JS `element.click()`)
- [ ] No raw CDP JSON leaked past the `cdp` module
- [ ] Secrets masked in traces
- [ ] CDP remains bound to `127.0.0.1`
