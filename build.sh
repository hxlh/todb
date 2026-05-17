#!/usr/bin/env bash
# build.sh — project task runner
# Usage: ./build.sh [command]
#
# Commands:
#   fmt       Format all code with rustfmt
#   lint      Run clippy with strict settings
#   check     Type-check without producing binaries
#   test      Run all tests
#   build     Build all targets (debug)
#   release   Build all targets (release)
#   ci        Full CI pipeline: fmt-check → lint → test → build
#   help      Show this message
#
# Default (no argument): runs ci

set -euo pipefail

BOLD='\033[1m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
RED='\033[0;31m'
RESET='\033[0m'

step() { echo -e "\n${BOLD}${YELLOW}▶ $*${RESET}"; }
ok()   { echo -e "${GREEN}✓ $*${RESET}"; }
die()  { echo -e "${RED}✗ $*${RESET}" >&2; exit 1; }

cmd_fmt() {
    step "Formatting (cargo fmt)"
    cargo fmt --all
    ok "Format complete"
}

cmd_fmt_check() {
    step "Format check (cargo fmt --check)"
    cargo fmt --all -- --check
    ok "Format check passed"
}

cmd_lint() {
    step "Lint (cargo clippy)"
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    ok "Lint passed"
}

cmd_check() {
    step "Type check (cargo check)"
    cargo check --workspace --all-targets --all-features
    ok "Check passed"
}

cmd_test() {
    step "Tests (cargo test)"
    cargo test --workspace --all-features
    ok "All tests passed"
}

cmd_build() {
    step "Build debug (cargo build)"
    cargo build --workspace --all-targets
    ok "Debug build complete"
}

cmd_release() {
    step "Build release (cargo build --release)"
    cargo build --workspace --all-targets --release
    ok "Release build complete"
}

cmd_ci() {
    step "CI pipeline"
    cmd_fmt_check
    cmd_lint
    cmd_test
    cmd_build
    ok "CI pipeline passed"
}

cmd_help() {
    sed -n '/^# Usage/,/^[^#]/p' "$0" | grep '^#' | sed 's/^# \?//'
}

case "${1:-ci}" in
    fmt)        cmd_fmt ;;
    fmt-check)  cmd_fmt_check ;;
    lint)       cmd_lint ;;
    check)      cmd_check ;;
    test)       cmd_test ;;
    build)      cmd_build ;;
    release)    cmd_release ;;
    ci)         cmd_ci ;;
    help|--help|-h) cmd_help ;;
    *) die "Unknown command: ${1}. Run './build.sh help' for usage." ;;
esac
