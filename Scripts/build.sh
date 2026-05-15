#!/usr/bin/env bash

set -euo pipefail

# Build script for the Rust Grok proxy.
# This script ensures credentials are set up and then builds the proxy binary.

echo "Building Grok proxy..."

# First run the setup script to ensure dependencies and credentials
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SETUP_SCRIPT="$SCRIPT_DIR/setup_proxy.sh"

if [[ -f "$SETUP_SCRIPT" ]]; then
    echo "Running setup script..."
    "$SETUP_SCRIPT" || {
        echo "Setup failed. Please fix the issues and try again."
        exit 1
    }
else
    echo "Warning: Setup script not found at $SETUP_SCRIPT"

    # Check if credentials.json exists in the project root directory
    CREDENTIALS_FILE="$PROJECT_ROOT/credentials.json"
    if [[ ! -f "$CREDENTIALS_FILE" ]]; then
        echo "Warning: credentials.json not found."
        echo "The proxy will start with mock credentials which will likely fail with real requests."
        echo "Run 'Scripts/setup_proxy.sh' to create credentials.json for the proxy."
    fi
fi

# Build the proxy
echo "Building Grok proxy with Cargo..."
cd "$PROJECT_ROOT" || {
    echo "Failed to navigate to project root directory."
    exit 1
}

cargo build --package grok-proxy --bin proxy || {
    echo "Build failed. Please fix the issues and try again."
    exit 1
}

echo "Build successful! You can now run the proxy with 'cargo run -p grok-proxy --bin proxy -- serve'"
