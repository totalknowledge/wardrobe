#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"

usage() {
    cat <<EOF
Usage: utilities/scripts/publish-crates.sh [options]

Publishes the foundational engine (wardrobe-embedded) and network client (wardrobe-client)
to crates.io using cargo publish in the exact sequence required.

Publish sequence:
  1. cargo publish -p wardrobe-embedded
  2. cargo publish -p wardrobe-client

Options:
  --dry-run        Run cargo publish with --dry-run
  --allow-dirty    Pass --allow-dirty to cargo publish
  --no-verify      Pass --no-verify to cargo publish
  --token <token>  Pass authentication token to cargo publish
  --help, -h       Display this help message
EOF
}

DRY_RUN=false
ALLOW_DIRTY=false
NO_VERIFY=false
CARGO_TOKEN=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run)
            DRY_RUN=true
            shift
            ;;
        --allow-dirty)
            ALLOW_DIRTY=true
            shift
            ;;
        --no-verify)
            NO_VERIFY=true
            shift
            ;;
        --token)
            if [[ $# -lt 2 ]]; then
                printf 'Error: --token requires an API token argument\n' >&2
                exit 1
            fi
            CARGO_TOKEN="$2"
            shift 2
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            printf 'Error: Unknown argument: %s\n' "$1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

if ! command -v cargo >/dev/null 2>&1; then
    printf 'Error: cargo is required to publish crates to crates.io\n' >&2
    exit 1
fi

log_step() {
    printf '\n==> %s\n' "$1"
}

CARGO_FLAGS=()
if [ "$DRY_RUN" = true ]; then
    CARGO_FLAGS+=("--dry-run")
fi
if [ "$ALLOW_DIRTY" = true ]; then
    CARGO_FLAGS+=("--allow-dirty")
fi
if [ "$NO_VERIFY" = true ]; then
    CARGO_FLAGS+=("--no-verify")
fi
if [ -n "$CARGO_TOKEN" ]; then
    CARGO_FLAGS+=("--token" "$CARGO_TOKEN")
fi

log_step "Step 1: Publishing foundational engine crate (wardrobe-embedded) to crates.io"
(cd "$REPO_ROOT" && cargo publish -p wardrobe-embedded "${CARGO_FLAGS[@]}")

log_step "Step 2: Publishing network client crate (wardrobe-client) to crates.io"
(cd "$REPO_ROOT" && cargo publish -p wardrobe-client "${CARGO_FLAGS[@]}")

log_step "Publish sequence finished successfully!"
