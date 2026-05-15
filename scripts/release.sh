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
RESUME=0
for arg in "$@"; do
    case "$arg" in
        --dry-run)      DRY_RUN=1 ;;
        --skip-publish) SKIP_PUBLISH=1 ;;
        --skip-github)  SKIP_GITHUB=1 ;;
        # Skip the build / test / clippy preflight. Use this when
        # re-running after a crates.io 429 to pick up where the
        # previous run was rate-limited; per-crate publish is
        # already idempotent via the `already_published` check.
        --resume)       RESUME=1 ;;
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
    in_deps && /^truce-rack-/ && /version[[:space:]]*=[[:space:]]*"/ {
        # Portable POSIX-awk capture: strip everything up through
        # the opening quote of the version string, then everything
        # from the closing quote on. BSD awk on macOS lacks the
        # gawk-only 3-arg form of match().
        v = $0
        sub(/.*version[[:space:]]*=[[:space:]]*"/, "", v)
        sub(/".*/, "", v)
        if (v != want) print $1, "=", v
    }
' Cargo.toml)
if [[ -n "$PIN_MISMATCH" ]]; then
    echo "workspace.dependencies pins disagree with workspace.package.version=$VERSION:" >&2
    echo "$PIN_MISMATCH" >&2
    echo "run scripts/bump.sh to resync." >&2
    exit 1
fi

# Tracked-file changes block the release; untracked files (?? lines)
# are fine — local notes, build artefacts, license files not yet
# checked in, etc. don't affect what's about to be published.
TRACKED_DIRTY=$(git status --porcelain | grep -v '^??' || true)
if [[ -n "$TRACKED_DIRTY" ]]; then
    echo "tracked-file changes — commit or stash first." >&2
    echo "$TRACKED_DIRTY" >&2
    exit 1
fi

BRANCH=$(git rev-parse --abbrev-ref HEAD)
echo "    branch: $BRANCH"
echo "    HEAD:   $(git rev-parse --short HEAD)"

if (( ! DRY_RUN )) && git rev-parse "$TAG" >/dev/null 2>&1; then
    echo "tag $TAG already exists; bump version or delete the tag." >&2
    exit 1
fi

if (( RESUME )); then
    echo "==> --resume set, skipping build/test/clippy preflight"
else
    echo "==> cargo build --workspace --release (sanity check)"
    cargo build --workspace --release

    echo "==> cargo test --workspace"
    cargo test --workspace --quiet

    echo "==> cargo clippy --workspace --all-targets -- -D warnings"
    cargo clippy --workspace --all-targets -- -D warnings
fi

# ----------------------------------------------------------------- crates.io
# `true` iff `<crate>@<version>` is already on crates.io. Lets us
# resume after a 429 (new-crate rate limit: ~1/10min for low-trust
# publishers) without re-uploading anything that landed.
already_published() {
    local crate="$1"
    local version="$2"
    local code
    code=$(curl -fsS -o /dev/null -w '%{http_code}' \
        -A "truce-rack-release/$VERSION" \
        "https://crates.io/api/v1/crates/$crate/$version" 2>/dev/null || echo "000")
    [[ "$code" == "200" ]]
}

# Sleep until the timestamp in a crates.io 429 message ("try
# again after Fri, 15 May 2026 08:03:00 GMT"), plus a 30s buffer
# for clock skew. Returns non-zero if the date can't be parsed.
wait_until_after() {
    local retry_at="$1"
    local now wait_until sleep_secs
    now=$(date -u +%s)
    if [[ "$(uname)" == "Darwin" ]]; then
        wait_until=$(date -j -u -f '%a, %d %b %Y %H:%M:%S GMT' "$retry_at" +%s 2>/dev/null) || return 1
    else
        wait_until=$(date -u -d "$retry_at" +%s 2>/dev/null) || return 1
    fi
    sleep_secs=$(( wait_until - now + 30 ))
    (( sleep_secs > 0 )) || sleep_secs=30
    echo "==> rate-limited until $retry_at; sleeping ${sleep_secs}s"
    sleep "$sleep_secs"
}

publish_crate() {
    local crate="$1"
    if (( DRY_RUN )); then
        echo "==> cargo publish -p $crate --dry-run"
        cargo publish -p "$crate" --dry-run
        return
    fi
    if already_published "$crate" "$VERSION"; then
        echo "==> $crate $VERSION already on crates.io; skipping"
        return
    fi

    # Try up to 4 times: any 429 sleeps until the retry-after
    # timestamp + 30s, then loops. Other errors propagate
    # immediately. `--no-verify` is intentionally NOT passed —
    # we want each crate to compile in isolation against
    # already-published deps.
    local attempt out rc retry_at
    for attempt in 1 2 3 4; do
        out=$(mktemp)
        echo "==> cargo publish -p $crate (attempt $attempt)"
        cargo publish -p "$crate" 2>&1 | tee "$out"
        rc=${PIPESTATUS[0]}
        if (( rc == 0 )); then
            rm -f "$out"
            # Index-propagation insurance — see banner above.
            sleep 10
            return 0
        fi
        retry_at=$(grep -oE 'try again after [A-Za-z]+, [0-9]+ [A-Za-z]+ [0-9]+ [0-9:]+ GMT' "$out" \
            | sed 's/^try again after //' | head -n1)
        rm -f "$out"
        if [[ -z "$retry_at" ]]; then
            echo "==> $crate publish failed (rc=$rc); not a rate-limit error" >&2
            return $rc
        fi
        wait_until_after "$retry_at" || {
            echo "==> failed to parse retry-after date '$retry_at'" >&2
            return 1
        }
    done
    echo "==> $crate publish gave up after 4 attempts" >&2
    return 1
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
