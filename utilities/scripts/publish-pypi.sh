#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"

export PATH="$HOME/.local/bin:$PATH"

usage() {
    cat <<EOF
Usage: utilities/scripts/publish-pypi.sh [options]

Builds and publishes pure Python client (wardrobe-client) and native embedded extension (wardrobe-embedded)
packages to PyPI.

Publish sequence:
  1. Build & publish bindings/python/wardrobe-client (hatchling / build / twine)
  2. Build & publish bindings/python/wardrobe-embedded (maturin / PyO3)

Options:
  --dry-run               Build packages and run verification without uploading to PyPI
  --repository <repo>     PyPI repository target (e.g., pypi [default], testpypi)
  --token <token>         Pass API token (__token__) for upload
  --username <username>   PyPI username
  --password <password>   PyPI password
  --skip-build            Skip build phase (upload existing dist/wheels)
  --help, -h              Display this help message
EOF
}

DRY_RUN=false
REPOSITORY="pypi"
TOKEN=""
USERNAME=""
PASSWORD=""
SKIP_BUILD=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run)
            DRY_RUN=true
            shift
            ;;
        --repository)
            if [[ $# -lt 2 ]]; then
                printf 'Error: --repository requires an argument (e.g., testpypi)\n' >&2
                exit 1
            fi
            REPOSITORY="$2"
            shift 2
            ;;
        --token)
            if [[ $# -lt 2 ]]; then
                printf 'Error: --token requires a token argument\n' >&2
                exit 1
            fi
            TOKEN="$2"
            shift 2
            ;;
        --username)
            if [[ $# -lt 2 ]]; then
                printf 'Error: --username requires an argument\n' >&2
                exit 1
            fi
            USERNAME="$2"
            shift 2
            ;;
        --password)
            if [[ $# -lt 2 ]]; then
                printf 'Error: --password requires an argument\n' >&2
                exit 1
            fi
            PASSWORD="$2"
            shift 2
            ;;
        --skip-build)
            SKIP_BUILD=true
            shift
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

if ! command -v python3 >/dev/null 2>&1; then
    printf 'Error: python3 is required to publish PyPI packages\n' >&2
    exit 1
fi

log_step() {
    printf '\n==> %s\n' "$1"
}

CLIENT_DIR="$REPO_ROOT/bindings/python/wardrobe-client"
EMBEDDED_DIR="$REPO_ROOT/bindings/python/wardrobe-embedded"

# Step 1: wardrobe-client
log_step "Step 1: Building and publishing wardrobe-client (pure Python client package)"
cd "$CLIENT_DIR"

if [ "$SKIP_BUILD" = false ]; then
    log_step "Building wardrobe-client wheel..."
    rm -rf dist/
    if python3 -c "import build" >/dev/null 2>&1 && python3 -m build --help >/dev/null 2>&1; then
        python3 -m build
    elif command -v hatch >/dev/null 2>&1; then
        hatch build
    else
        python3 -m pip wheel . --no-deps -w dist/
    fi
fi

if command -v twine >/dev/null 2>&1; then
    log_step "Verifying wardrobe-client artifacts with twine check..."
    twine check dist/*
else
    printf 'Warning: twine is not installed; skipping package verification.\n' >&2
fi

if [ "$DRY_RUN" = true ]; then
    log_step "[Dry-Run] Skipping PyPI upload for wardrobe-client."
else
    log_step "Uploading wardrobe-client to PyPI ($REPOSITORY)..."
    TWINE_CMD=""
    if command -v twine >/dev/null 2>&1; then
        TWINE_CMD="twine"
    elif python3 -m twine --version >/dev/null 2>&1; then
        TWINE_CMD="python3 -m twine"
    else
        printf 'Error: twine is required to upload wardrobe-client to PyPI. Install it with: pip install twine\n' >&2
        exit 1
    fi

    TWINE_ARGS=("upload" "--skip-existing")
    if [ "$REPOSITORY" != "pypi" ]; then
        TWINE_ARGS+=("--repository" "$REPOSITORY")
    fi
    if [ -n "$TOKEN" ]; then
        TWINE_ARGS+=("-u" "__token__" "-p" "$TOKEN")
    elif [ -n "$USERNAME" ] && [ -n "$PASSWORD" ]; then
        TWINE_ARGS+=("-u" "$USERNAME" "-p" "$PASSWORD")
    fi
    $TWINE_CMD "${TWINE_ARGS[@]}" dist/*
fi

# Step 2: wardrobe-embedded
log_step "Step 2: Building and publishing wardrobe-embedded (native Rust PyO3 extension)"
cd "$EMBEDDED_DIR"

if [ "$SKIP_BUILD" = false ]; then
    if ! command -v maturin >/dev/null 2>&1; then
        printf 'Error: maturin is required to build wardrobe-embedded native wheels. Install with: pip install maturin\n' >&2
        exit 1
    fi
    log_step "Building wardrobe-embedded wheel with maturin build --release..."
    maturin build --release
fi

if [ "$DRY_RUN" = true ]; then
    log_step "[Dry-Run] Skipping PyPI upload for wardrobe-embedded."
else
    log_step "Uploading wardrobe-embedded to PyPI ($REPOSITORY)..."
    MATURIN_FLAGS=("--skip-existing")
    if [ "$REPOSITORY" != "pypi" ]; then
        MATURIN_FLAGS+=("--repository" "$REPOSITORY")
    fi
    if [ -n "$TOKEN" ]; then
        export MATURIN_PYPI_TOKEN="$TOKEN"
    elif [ -n "$USERNAME" ] && [ -n "$PASSWORD" ]; then
        MATURIN_FLAGS+=("-u" "$USERNAME" "-p" "$PASSWORD")
    fi
    maturin publish "${MATURIN_FLAGS[@]}"
fi

log_step "PyPI publish sequence finished successfully!"
