#!/usr/bin/env bash
# Builds the rocket-mem workspace.
# Usage: ./scripts/build.sh [--release]

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'

usage() {
    echo "Usage: $0 [--release]"
    echo ""
    echo "  (no flag)   debug build (target/debug)"
    echo "  --release   optimized build (target/release)"
    exit 1
}

profile="debug"
build_args=(--workspace)

case "${1:-}" in
    "") ;;
    --release)
        profile="release"
        build_args+=(--release)
        ;;
    -h|--help) usage ;;
    *)
        echo -e "${RED}Unknown argument: ${1}${NC}"
        usage
        ;;
esac

echo -e "${BLUE}Building rocket-mem workspace (${profile})...${NC}"
if ! cargo build "${build_args[@]}"; then
    echo -e "${RED}Build failed.${NC}"
    exit 1
fi

echo -e "${GREEN}Build succeeded.${NC}"
echo "Binary: target/${profile}/rocket-mem"
