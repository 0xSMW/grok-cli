#!/usr/bin/env bash

set -euo pipefail

# Setup script for the Rust Grok proxy.
# This script ensures Python browser-cookie dependencies and credentials are set up.

echo "Setting up Grok proxy..."

# Check if Python3 is installed
if ! command -v python3 >/dev/null 2>&1; then
    echo "Error: Python3 is required but not installed. Please install Python3 and try again."
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Check if credentials.json exists in the project root directory.
# The proxy reads credentials.json from its current working directory.
CREDENTIALS_FILE="$PROJECT_ROOT/credentials.json"
if [[ ! -f "$CREDENTIALS_FILE" ]]; then
    echo "credentials.json not found. Attempting to generate it..."

    # Check if the cookie_extractor.py script exists
    COOKIE_EXTRACTOR="$SCRIPT_DIR/cookie_extractor.py"
    if [[ ! -f "$COOKIE_EXTRACTOR" ]]; then
        echo "Error: cookie_extractor.py not found at $COOKIE_EXTRACTOR"
        exit 1
    fi

    # Run the cookie_extractor.py script to generate credentials.json
    if ! python3 "$COOKIE_EXTRACTOR" --format json --required --output "$CREDENTIALS_FILE"; then
        echo "Error: Failed to generate credentials.json."
        echo "Please ensure you are logged into Grok in your browser and try again."
        exit 1
    fi

    echo "Successfully generated credentials.json at $CREDENTIALS_FILE"
else
    echo "credentials.json already exists at $CREDENTIALS_FILE"
fi

echo "Setup complete. You can now build and run the Grok proxy."
