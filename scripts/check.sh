#!/usr/bin/env bash
set -euo pipefail
SCRIPT_PATH="${BASH_SOURCE[0]}"
if [ "${SCRIPT_PATH#*/}" = "$SCRIPT_PATH" ]; then
    SCRIPT_PATH="./$SCRIPT_PATH"
fi
SCRIPT_DIR="${SCRIPT_PATH%/*}"
ROOT="$SCRIPT_DIR/.."
APP="$ROOT/apps/harness-app"
require_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        printf 'error: required command %s was not found on PATH\n' "$1" >&2
        exit 1
    fi
}
step() {
    local label="$1"
    shift
    printf '\n==> %s\n' "$label"
    "$@"
}
require_command cargo
require_command pnpm
if [ ! -d "$APP/node_modules" ]; then
    printf 'error: frontend dependencies are missing; run: pnpm --dir apps/harness-app install\n' >&2
    exit 1
fi
cd "$ROOT"
step 'file size' node scripts/size-check.mjs
step 'cargo fmt --check' cargo fmt --all -- --check
step 'cargo clippy' cargo clippy --workspace --all-targets -- -D warnings
step 'cargo test' cargo test --workspace
step 'frontend typecheck' pnpm --dir "$APP" typecheck
printf '\nAll checks passed.\n'
