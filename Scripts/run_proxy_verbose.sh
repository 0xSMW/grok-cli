#!/usr/bin/env bash

set -euo pipefail

# Run the Grok proxy server with verbose logging enabled.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

cd "$PROJECT_ROOT"
cargo run --package grok-proxy --bin proxy -- serve --verbose "$@"
