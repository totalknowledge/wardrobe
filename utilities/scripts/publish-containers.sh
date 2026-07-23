#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
DOCKERFILE="$REPO_ROOT/server/packaging/docker/Dockerfile"
IMAGE_REPOSITORY="${DOCKERHUB_REPOSITORY:?Set DOCKERHUB_REPOSITORY to the Docker Hub repository, for example account/wardrobe-server}"
IMAGE_TAG="${IMAGE_REPOSITORY}:latest"

if ! command -v docker >/dev/null; then
  printf 'docker is required to build and publish the Wardrobe server image\n' >&2
  exit 1
fi

docker build --pull --file "$DOCKERFILE" --tag "$IMAGE_TAG" "$REPO_ROOT"
docker push "$IMAGE_TAG"