#!/usr/bin/env bash
# Prints `true` when a PR changes a file that the Rust jobs build, lint, test or scan, and `false`
# when it changes only docs, scripts or other workflows. CI and CodeQL skip those jobs on `false`.
# Usage: code-changed.sh <base sha> <head sha>
set -euo pipefail

files=$(git diff --name-only "$1...$2")
printf '%s\n' "$files" >&2

if grep -qE '^(crates|benches|examples|\.cargo)/|^(Cargo\.toml|Cargo\.lock|rust-toolchain\.toml|rustfmt\.toml|clippy\.toml|deny\.toml)$|^\.github/(workflows/(ci|codeql)\.yml|scripts/code-changed\.sh)$' <<<"$files"; then
  echo true
else
  echo false
fi
