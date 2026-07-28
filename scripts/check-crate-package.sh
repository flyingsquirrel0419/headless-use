#!/usr/bin/env bash
set -euo pipefail

files="$(cargo package --locked --list --allow-dirty)"
allowed='^(\.cargo_vcs_info\.json|Cargo\.lock|Cargo\.toml|Cargo\.toml\.orig|LICENSE|README\.md|build\.rs|src/.+)$'
unexpected="$(printf '%s\n' "$files" | grep -Ev "$allowed" || true)"

if [[ -n "$unexpected" ]]; then
  echo "error: crates.io package contains unexpected files:" >&2
  printf '%s\n' "$unexpected" >&2
  exit 1
fi

for required in Cargo.toml LICENSE README.md build.rs src/lib.rs src/main.rs; do
  if ! grep -Fxq "$required" <<<"$files"; then
    echo "error: crates.io package is missing required file: $required" >&2
    exit 1
  fi
done

printf '%s\n' "$files"
