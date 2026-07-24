#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
DOCKERFILE="$REPO_ROOT/server/packaging/docker/Dockerfile"

# Ensure DOCKER_API_VERSION satisfies Docker daemon minimum API requirement (>= 1.44)
export DOCKER_API_VERSION="${DOCKER_API_VERSION:-1.45}"

usage() {
    cat <<EOF
Usage: utilities/scripts/publish-containers.sh [options]

Builds and publishes Docker container images for wardrobe-server.

Options:
  --repository <repo>   Container image repository (default: totalknowledge/wardrobe-server or \$DOCKERHUB_REPOSITORY)
  --version <version>   Version tag (default: version from server/Cargo.toml or \$WARDROBE_VERSION)
  --dry-run             Build image without pushing to registry
  --help, -h            Display this help message
EOF
}

DEFAULT_VERSION="$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "$REPO_ROOT/server/Cargo.toml" | head -n 1)"
IMAGE_REPOSITORY="${DOCKERHUB_REPOSITORY:-totalknowledge/wardrobe-server}"
IMAGE_VERSION="${WARDROBE_VERSION:-$DEFAULT_VERSION}"
DRY_RUN=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --repository|--repo)
            if [[ $# -lt 2 ]]; then
                printf 'Error: --repository requires an argument\n' >&2
                exit 1
            fi
            IMAGE_REPOSITORY="$2"
            shift 2
            ;;
        --version|--tag)
            if [[ $# -lt 2 ]]; then
                printf 'Error: --version requires an argument\n' >&2
                exit 1
            fi
            IMAGE_VERSION="$2"
            shift 2
            ;;
        --dry-run)
            DRY_RUN=true
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

VERSION_TAG="${IMAGE_REPOSITORY}:${IMAGE_VERSION}"
LATEST_TAG="${IMAGE_REPOSITORY}:latest"

if ! command -v docker >/dev/null 2>&1; then
    printf 'Error: docker is required to build and publish the Wardrobe server image\n' >&2
    exit 1
fi

printf '==> Building Docker image %s and %s...\n' "$VERSION_TAG" "$LATEST_TAG"
docker build \
    --pull \
    --file "$DOCKERFILE" \
    --tag "$VERSION_TAG" \
    --tag "$LATEST_TAG" \
    "$REPO_ROOT"

if [ "$DRY_RUN" = true ]; then
    printf '\n==> [Dry-Run] Skipping docker push.\n'
else
    printf '\n==> Pushing container tags to registry...\n'
    docker push "$VERSION_TAG"
    docker push "$LATEST_TAG"
fi

printf '\n==> Container publish sequence finished successfully!\n'
