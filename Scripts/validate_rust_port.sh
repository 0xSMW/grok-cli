#!/usr/bin/env bash
set -Eeuo pipefail
IFS=$'\n\t'

current_step=''

trap 'status=$?; if [[ -n "${current_step:-}" ]]; then printf >&2 "\nValidation failed during: %s\n" "$current_step"; else printf >&2 "\nValidation failed with exit code %d at line %d.\n" "$status" "$LINENO"; fi; exit "$status"' ERR

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd -- "$script_dir/.." && pwd -P)"

cd "$repo_root"

if [[ ! -f Cargo.toml || ! -d Scripts ]]; then
  printf >&2 'error: expected repository root at %s\n' "$repo_root"
  exit 1
fi

export CARGO_NET_OFFLINE=true

format_command() {
  local rendered=''
  local quoted=''

  for arg in "$@"; do
    printf -v quoted '%q' "$arg"
    rendered+="${rendered:+ }$quoted"
  done

  printf '%s' "$rendered"
}

run() {
  current_step="$(format_command "$@")"
  printf '\n==> %s\n' "$current_step"
  "$@"
  current_step=''
}

run_quiet() {
  current_step="$(format_command "$@")"
  printf '\n==> %s\n' "$current_step"
  "$@" >/dev/null
  current_step=''
}

require_command() {
  local command_name="$1"

  if ! command -v "$command_name" >/dev/null 2>&1; then
    printf >&2 'error: required command not found: %s\n' "$command_name"
    exit 1
  fi
}

shell_scripts=()
while IFS= read -r -d '' file_path; do
  first_line=''
  IFS= read -r first_line <"$file_path" || true

  if [[ "$file_path" == *.sh || "$first_line" == '#!'*'/bash'* || "$first_line" == '#!'*'env bash'* ]]; then
    shell_scripts+=("$file_path")
  fi
done < <(find "$repo_root/Scripts" -type f ! -path '*/__pycache__/*' -print0 | sort -z)

require_command bash
require_command cargo

if [[ ${#shell_scripts[@]} -eq 0 ]]; then
  printf '\n==> no shell scripts found under Scripts/\n'
else
  for shell_script in "${shell_scripts[@]}"; do
    run bash -n "$shell_script"
  done
fi

run_quiet cargo metadata --format-version 1 --locked --offline --no-deps
run cargo fmt --all --check
run cargo clippy --workspace --all-targets --locked --offline -- -D warnings
run cargo test --workspace --locked --offline

if [[ -f Tests/test_cookie_extractor.py ]]; then
  require_command python3
  run python3 Tests/test_cookie_extractor.py
else
  printf '\n==> skipping missing Tests/test_cookie_extractor.py\n'
fi

printf '\nRust port validation passed.\n'
