#!/usr/bin/env bash
set -euo pipefail

cargo_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
npm_version="$(node -p "require('./npm/package.json').version")"
tag="${1:-${GITHUB_REF_NAME:-}}"

if [[ -z "$cargo_version" || -z "$npm_version" ]]; then
  echo "error: could not read Cargo or npm package version" >&2
  exit 1
fi

if [[ "$cargo_version" != "$npm_version" ]]; then
  echo "error: Cargo version $cargo_version != npm version $npm_version" >&2
  exit 1
fi

if [[ -n "$tag" && "$tag" != "v$cargo_version" ]]; then
  echo "error: release tag $tag != v$cargo_version" >&2
  exit 1
fi

echo "$cargo_version"
