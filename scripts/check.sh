#!/usr/bin/env bash
# The full local gate: runs what the CI `test` job runs on this OS.
#   format check, clippy with -D warnings, cargo test --workspace,
#   and on Linux the coverage floor from .github/workflows/ci.yml.
# Usage: scripts/check.sh   (from anywhere in the repo)
# On Windows use scripts/check.ps1 instead.
set -euo pipefail

cd "$(dirname "$0")/.."

# Test scratch (tempfile directories) stays in the worktree, not /tmp.
mkdir -p .verify
scratch=$(mktemp -d "$PWD/.verify/tmp.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
export TMPDIR="$scratch"

step() { printf '\n==> %s\n' "$*"; }

step "cargo fmt --all --check"
cargo fmt --all --check

step "cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

step "cargo test --workspace"
cargo test --workspace

if [ "$(uname -s)" = "Linux" ]; then
  floor=$(sed -n 's/^ *COVERAGE_FLOOR: *\([0-9.]*\).*/\1/p' .github/workflows/ci.yml)
  if [ -z "$floor" ]; then
    echo "error: COVERAGE_FLOOR not found in .github/workflows/ci.yml" >&2
    exit 1
  fi
  if ! cargo llvm-cov --version > /dev/null 2>&1; then
    echo "error: cargo-llvm-cov is missing. Install it with:" >&2
    echo "  rustup component add llvm-tools-preview && cargo install cargo-llvm-cov --locked --target-dir .verify/cargo-install" >&2
    exit 1
  fi
  step "cargo llvm-cov --workspace (floor ${floor}% lines)"
  cargo llvm-cov --workspace --summary-only --fail-under-lines "$floor"
else
  step "coverage skipped: CI measures it on Linux only"
fi

step "all checks passed"
