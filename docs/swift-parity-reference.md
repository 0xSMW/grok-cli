# Swift Parity Reference

SwiftPM is retired for this repository. The supported implementation is the
Rust-only Cargo workspace with `grok-client`, `grok-cli`, and `grok-proxy`.

This file is now a historical behavior checklist for the Rust port, not a
current build or test guide. Use Cargo for validation and keep user-facing docs
focused on the Rust binaries.

Current validation lives in [Tests/README.md](../Tests/README.md):

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`

When checking coverage against the historical Swift implementation, verify the
Rust tests and docs cover these supported behavior areas:

- client request construction, endpoint normalization, response decoding, model
  aliasing, streaming, account/conversation/resource/task/sharing APIs, and
  speech-to-text helpers
- CLI routing, global option handling, JSON and NDJSON output contracts,
  interactive commands, auth import/generation, file attachments, audio input,
  workspace/task/skill/agent commands, formatting, and error mapping
- proxy endpoints for `/hello`, `/v1/models`, `/models`,
  `/v1/chat/completions`, and `/v1/audio/transcriptions`, including streaming
  server-sent events, multipart and JSON audio transcription requests, CORS,
  credential loading, and body-size configuration
- installation and operations through Cargo, `Scripts/install_cli.sh`, proxy
  helper scripts, Docker, and Docker Compose

If a historical behavior is intentionally dropped, document the Rust-only
behavior directly. For example, `grok code` remains disabled because Grok tool
calls run server-side, so docs should not describe a working local code harness.
