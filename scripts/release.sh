#!/usr/bin/env bash
# Release truce-rack to crates.io + cut a GitHub release.
#
# Usage:
#   scripts/release.sh                 # publish workspace.package.version
#   scripts/release.sh --dry-run       # cargo publish --dry-run for every crate, no tag, no GH release
#   scripts/release.sh --skip-publish  # skip crates.io, only tag + GH release
#   scripts/release.sh --skip-github   # publish to crates.io, skip tag + GH release
#
# Requires:
#   - clean git working tree, on a published branch
#   - `cargo login` already done (or CARGO_REGISTRY_TOKEN exported)
#   - `gh` CLI authenticated for the GitHub release step
#
# Crates publish in topological order; we sleep between each so
# crates.io's index has time to register the new version before
# the next crate that depends on it is uploaded.

set -euo pipefail

cd "$(dirname "$0")/.."

DRY_RUN=0
SKIP_PUBLISH=0
SKIP_GITHUB=0
for arg in "$@"; do
    case "$arg" in
        --dry-run)      DRY_RUN=1 ;;
        --skip-publish) SKIP_PUBLISH=1 ;;
        --skip-github)  SKIP_GITHUB=1 ;;
        -h|--help)
            sed -n '2,15p' "$0" | sed 's/^# //;s/^#//'
            exit 0
            ;;
        *)
            echo "unknown arg: $arg" >&2
            exit 2
            ;;
    esac
done

# Topological dependency order. core has no internal deps; au3
# depends on au; standalone depends on every wrapper.
CRATES=(
    truce-rack-core
    truce-rack-clap
    truce-rack-vst3
    truce-rack-au
    truce-rack-lv2
    truce-rack-test
    truce-rack-au3
    truce-rack-standalone
)

VERSION=$(awk '
    /^\[workspace\.package\]/ { in_pkg = 1; next }
    /^\[/                     { in_pkg = 0 }
    in_pkg && /^version[[:space:]]*=/ {
        gsub(/[ "]/, "", $0); sub(/^version=/, ""); print; exit
    }
' Cargo.toml)
if [[ -z "$VERSION" ]]; then
    echo "could not read workspace.package.version from Cargo.toml" >&2
    exit 1
fi
TAG="v$VERSION"

# ---------------------------------------------------------------- preflight
echo "==> truce-rack release $TAG"

# Verify that every truce-rack-* pin in [workspace.dependencies]
# matches workspace.package.version. The two MUST stay in lockstep
# or cargo publish writes a stale dep version into the published
# crate's manifest. Use scripts/bump.sh to keep them in sync.
PIN_MISMATCH=$(awk -v want="$VERSION" '
    /^\[workspace\.dependencies\]/ { in_deps = 1; next }
    /^\[/                          { in_deps = 0 }
    in_deps && /^truce-rack-/ {
        if (match($0, /version = "([^"]+)"/, m) && m[1] != want) {
            print $1, "= " m[1]
        }
    }
' Cargo.toml)
if [[ -n "$PIN_MISMATCH" ]]; then
    echo "workspace.dependencies pins disagree with workspace.package.version=$VERSION:" >&2
    echo "$PIN_MISMATCH" >&2
    echo "run scripts/bump.sh to resync." >&2
    exit 1
fi

if [[ -n "$(git status --porcelain)" ]]; then
    echo "working tree is dirty — commit or stash first." >&2
    git status --short >&2
    exit 1
fi

BRANCH=$(git rev-parse --abbrev-ref HEAD)
echo "    branch: $BRANCH"
echo "    HEAD:   $(git rev-parse --short HEAD)"

if (( ! DRY_RUN )) && git rev-parse "$TAG" >/dev/null 2>&1; then
    echo "tag $TAG already exists; bump version or delete the tag." >&2
    exit 1
fi

echo "==> cargo build --workspace --release (sanity check)"
cargo build --workspace --release

echo "==> cargo test --workspace"
cargo test --workspace --quiet

echo "==> cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

# ----------------------------------------------------------------- crates.io
publish_crate() {
    local crate="$1"
    local extra=""
    if (( DRY_RUN )); then
        extra="--dry-run"
    fi
    echo "==> cargo publish -p $crate $extra"
    # `--no-verify` is intentionally NOT passed — we want each
    # crate to compile in isolation against published deps.
    cargo publish -p "$crate" $extra
    if (( ! DRY_RUN )); then
        # Give crates.io's index a moment to propagate so the next
        # crate's dep resolution sees this version. 10s is overkill
        # for the index but cheap insurance against a transient
        # 'no matching package' error on the next publish.
        sleep 10
    fi
}

if (( SKIP_PUBLISH )); then
    echo "==> skipping crates.io publish (--skip-publish)"
else
    for crate in "${CRATES[@]}"; do
        publish_crate "$crate"
    done
fi

# Stop here for dry-run so we don't accidentally tag/push.
if (( DRY_RUN )); then
    echo "==> dry-run complete; no tag, no GitHub release"
    exit 0
fi

# ----------------------------------------------------------------- git tag + GitHub release
if (( SKIP_GITHUB )); then
    echo "==> skipping GitHub release (--skip-github)"
    exit 0
fi

echo "==> tagging $TAG and pushing"
git tag -a "$TAG" -m "truce-rack $TAG"
git push origin "$TAG"

# Build release notes from the commits since the previous tag.
PREV_TAG=$(git tag --sort=-v:refname | grep -v "^$TAG\$" | head -n1 || true)
NOTES_FILE=$(mktemp)
{
    echo "## truce-rack $TAG"
    echo
    if [[ -n "$PREV_TAG" ]]; then
        echo "### Changes since $PREV_TAG"
        echo
        git log --pretty=format:'- %s (%h)' "$PREV_TAG..$TAG"
    else
        echo "Initial release."
    fi
    echo
    echo
    echo "### Crates"
    for crate in "${CRATES[@]}"; do
        echo "- [$crate $VERSION](https://crates.io/crates/$crate/$VERSION)"
    done
} > "$NOTES_FILE"

echo "==> gh release create $TAG"
gh release create "$TAG" \
    --title "$TAG" \
    --notes-file "$NOTES_FILE"
rm -f "$NOTES_FILE"

echo "==> done. https://github.com/truce-audio/truce-rack/releases/tag/$TAG"
