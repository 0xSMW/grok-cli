# Fix Next Plan

This repository is now the Rust port of the Grok CLI. The previous contents tracked implementation-specific review findings imported from the old codebase; those findings were removed because they no longer describe the current Rust crates.

## Current Focus

1. Preserve source-project behavior parity through Rust tests and manual checks.
2. Keep active implementation plans grounded in `crates/grok-client`, `crates/grok-cli`, and `crates/grok-proxy`.
3. Retire imported Swift execution notes instead of carrying them forward as Rust-port guidance.

## Next Validation Targets

- Expand Rust parity coverage for command behavior, JSON/scriptable output, streaming, tasks, files, auth flows, and proxy behavior as each surface lands.
- Check future release notes and plans for Rust crate paths, Cargo commands, and current parity expectations.
- Keep historical behavior evidence in dedicated parity references, not in active implementation plans.
