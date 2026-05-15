# Test Commands

Run commands from the repository root unless noted. Cargo is the canonical validation surface for this Rust-only project; SwiftPM commands are retired and are not part of current validation.

## Cargo Validation

Prerequisites:

- Rust toolchain with Cargo, rustfmt, and Clippy. Check with `cargo --version`, `cargo fmt --version`, and `cargo clippy --version`.
- No Grok account, browser session, or saved credentials are required for automated Cargo tests. Networked behavior is covered with local mock servers or in-process Axum routes.

Canonical commands:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Cargo Scope Map

Use the workspace command for full validation, then narrow to these commands while iterating:

| Scope | Command | Coverage |
| --- | --- | --- |
| Full Rust workspace | `cargo test --workspace` | All `grok-client`, `grok-cli`, and `grok-proxy` unit and integration tests. |
| Client library | `cargo test -p grok-client` | Model and response decoding, endpoint normalization, request payloads, options, JSON lookup, account/conversation/resource/task/sharing APIs, HTTP helpers, and stream parsing. |
| CLI library and binary | `cargo test -p grok-cli` | CLI routing, option parsing, config, shell splitting, interactive parsing, terminal/table/HUD rendering, JSON and human output formatting, task formatting, typeahead, and binary-level behavior tests. |
| CLI binary behavior harness | `cargo test -p grok-cli --test cli_binary_parity` | The `grok` executable against local mock Grok servers, including top-level commands, disabled command handling, JSON/NDJSON contracts, auth paths, message/audio/file/workspace/task flows, formatting, and error handling. |
| Proxy crate | `cargo test -p grok-proxy` | In-process Axum routes for `/hello`, `/v1/models`, `/models`, chat completion validation, streaming server-sent events, audio transcription validation, CORS, credential loading, and body-size configuration. |

Examples of focused test filters:

```bash
cargo test -p grok-client mode
cargo test -p grok-cli --test cli_binary_parity message_json
cargo test -p grok-proxy chat_completion
```

## Python Cookie Extractor Tests

Prerequisites:

- Python 3.
- No browser login is required.

Command:

```bash
python3 Tests/test_cookie_extractor.py
```

Coverage:

- Imports `Scripts/cookie_extractor.py` directly.
- Tests SQLite cookie database reading, including WAL sidecars and copy fallback behavior.
- Tests handling of invalid encrypted cookie text as bytes.
- Tests browser/profile inference from explicit cookie database paths.
- Tests macOS keychain service probing through mocked subprocess calls.

## Proxy Smoke Scripts

These scripts are manual smoke checks, not Cargo suites. They require a running proxy and may call Grok through real credentials.

Prerequisites:

- `curl`.
- A running proxy on `http://127.0.0.1:8080` for the chat and model scripts. Start one with:

```bash
cargo run -p grok-proxy --bin proxy -- serve
```

- Valid proxy credentials, either by setting `GROK_COOKIES` to a JSON object of cookie key/value strings or by placing `credentials.json` in the proxy process working directory. `Scripts/setup_proxy.sh` can generate `credentials.json` from browser cookies when Python 3 and a logged-in browser session are available.
- `jq` for `./Scripts/test_proxy_models.sh`.
- A readable local audio file for `./Scripts/test_proxy_transcription.sh`.

Commands:

```bash
./Scripts/test_proxy_request.sh
./Scripts/test_proxy_streaming.sh
./Scripts/test_proxy_models.sh
./Scripts/test_proxy_transcription.sh <audio-file> [model]
```

Transcription-specific environment variables:

- `GROK_PROXY_URL`: overrides the proxy base URL for `test_proxy_transcription.sh`. Defaults to `http://127.0.0.1:8080`.
- `GROK_TRANSCRIPTION_MODEL`: overrides the transcription model when the command does not pass `[model]`. Defaults to `whisper-1`.

Coverage:

- `test_proxy_request.sh`: sends a non-streaming OpenAI-style chat completion request to `/v1/chat/completions`.
- `test_proxy_streaming.sh`: sends two streaming chat completion requests, prints the raw server-sent events, and fails if the stream does not include a terminal `finish_reason: "stop"` chunk followed by `data: [DONE]`.
- `test_proxy_models.sh`: calls `/v1/models` and `/models`, then formats both JSON responses with `jq`.
- `test_proxy_transcription.sh`: sends a multipart audio upload to `/v1/audio/transcriptions` and prints the transcription response.
