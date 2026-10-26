#!/usr/bin/env bash
# Cut a release from this machine: local CI, a tag, and a GitHub release
# carrying the artifacts CI built. Refuses a dirty tree, a tag that exists, or
# a version that does not match firmware/Cargo.toml.
#
#   scripts/release.sh v0.1.0 notes.md
set -euo pipefail
cd "$(dirname "$0")/.."
TAG=${1:?usage: scripts/release.sh vX.Y.Z notes.md}
NOTES=${2:?release notes file}

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' firmware/Cargo.toml | head -1)
[ "$TAG" = "v$VERSION" ] || { echo "tag $TAG does not match firmware version $VERSION"; exit 1; }
[ -z "$(git status --porcelain)" ] || { echo "working tree is dirty; commit first"; exit 1; }
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null && { echo "tag $TAG exists"; exit 1; }

scripts/ci.sh
if ls dist/*-dirty-* >/dev/null 2>&1; then echo "CI built a dirty image"; exit 1; fi

git tag -a "$TAG" -m "cairn-trail $TAG"
git push origin "$TAG"
gh release create "$TAG" dist/*.bin dist/*.elf dist/SHA256SUMS \
  --title "cairn-trail $TAG" --notes-file "$NOTES"
echo "released $TAG"
