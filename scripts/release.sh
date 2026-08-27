#!/usr/bin/env bash
# Release this fork: build the XCFramework, point Package.swift at the GitHub
# release that is about to exist, commit, tag, push, publish.
#
#   ./scripts/release.sh <version>       e.g. ./scripts/release.sh 0.3.0-kidgloves.1
#
# Bare semver, no `v` prefix — SwiftPM resolves tags as versions and a `v`
# breaks `exact:` pins. Adapted from zshannon/yswift's scripts/release.sh.
set -euo pipefail

VERSION="${1:-}"
if [ -z "$VERSION" ]; then
    echo "usage: ./scripts/release.sh <version>" >&2
    exit 1
fi

cd "$(dirname "$0")/.."

REPO_URL=$(git remote get-url origin)
case "$REPO_URL" in
    git@github.com:*)     REPO=${REPO_URL#git@github.com:} ;;
    https://github.com/*) REPO=${REPO_URL#https://github.com/} ;;
    *) echo "error: origin is not a GitHub remote: $REPO_URL" >&2; exit 1 ;;
esac
REPO=${REPO%.git}
BRANCH=$(git rev-parse --abbrev-ref HEAD)

if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "error: uncommitted changes; commit or stash first" >&2
    exit 1
fi
if git rev-parse -q --verify "refs/tags/$VERSION" >/dev/null; then
    echo "error: tag $VERSION already exists" >&2
    exit 1
fi
command -v gh >/dev/null || { echo "error: gh CLI not installed" >&2; exit 1; }

echo "▸ Releasing $REPO $VERSION from $BRANCH"

# Regenerates lib/swift/scaffold and prints the checksum at the end.
YSWIFT_LOCAL=true ./scripts/build-xcframework.sh

CHECKSUM=$(openssl dgst -sha256 lib/yniffiFFI.xcframework.zip | awk '{print $2}')
DOWNLOAD_URL="https://github.com/$REPO/releases/download/$VERSION/yniffiFFI.xcframework.zip"

sed -i '' "s|url: \"https://github.com/.*/releases/download/.*/yniffiFFI.xcframework.zip\"|url: \"$DOWNLOAD_URL\"|" Package.swift
sed -i '' "s|checksum: \"[a-f0-9]*\"|checksum: \"$CHECKSUM\"|" Package.swift
grep -q "$CHECKSUM" Package.swift || { echo "error: Package.swift binary target not rewritten" >&2; exit 1; }

echo "▸ Package.swift → $DOWNLOAD_URL ($CHECKSUM)"

git add Package.swift lib/swift/scaffold/
git commit -m "Release $VERSION"
git tag "$VERSION"
git push origin "$BRANCH" "refs/tags/$VERSION"   # this tag only — never --tags

gh release create "$VERSION" --repo "$REPO" --title "$VERSION" \
    --notes "yswift $VERSION — see devnotes/DevLog.md for what this fork changes." \
    lib/yniffiFFI.xcframework.zip

echo "▸ Released https://github.com/$REPO/releases/tag/$VERSION"
