#!/bin/bash
#
# Render GitHub release notes for a tag from CHANGELOG.md.
#
# CHANGELOG.md is the authoritative, curated release story (see
# .omc/RELEASE_RULE.md); the release body should reproduce it rather than a
# truncated `git log`. Prints the matching section to stdout.
#
# Usage: scripts/release-notes.sh <tag> [repository]
#   tag         e.g. v0.39.0 — matched against a "## v0.39.0 ..." heading
#   repository  e.g. owner/name — when given, appends a Full Changelog link
#
# Falls back to a commit list when the tag has no CHANGELOG section, so a
# release never ships with an empty body. Exits non-zero only on bad usage.

set -uo pipefail

TAG=${1:-}
REPO=${2:-}

if [ -z "$TAG" ]; then
    echo "usage: $0 <tag> [repository]" >&2
    exit 2
fi

CHANGELOG_FILE=${CHANGELOG_FILE:-CHANGELOG.md}

# Section for this tag: from its "## <tag>" heading up to the next "## " heading.
# The heading itself is dropped — the GitHub release is already titled.
section=""
if [ -f "$CHANGELOG_FILE" ]; then
    section=$(awk -v tag="$TAG" '
        # Heading for the requested tag: "## v0.39.0" or "## v0.39.0 — title".
        /^## / {
            if (found) { exit }
            heading = substr($0, 4)
            split(heading, parts, " ")
            if (parts[1] == tag) { found = 1; next }
        }
        found { print }
    ' "$CHANGELOG_FILE")
fi

# Strip leading and trailing blank lines.
section=$(printf '%s\n' "$section" | sed -e '/./,$!d' | awk '
    { lines[NR] = $0 }
    END {
        last = NR
        while (last > 0 && lines[last] ~ /^[[:space:]]*$/) { last-- }
        for (i = 1; i <= last; i++) { print lines[i] }
    }
')

if [ -n "$section" ]; then
    printf '%s\n' "$section"
else
    echo "::warning::No CHANGELOG.md section found for $TAG; falling back to commit list" >&2
    previous=$(git describe --tags --abbrev=0 "${TAG}^" 2>/dev/null || echo "")
    echo "## What's Changed"
    echo ""
    if [ -n "$previous" ]; then
        git log --pretty=format:"- %s" "${previous}..${TAG}"
    else
        git log --pretty=format:"- %s" "$TAG"
    fi
    echo ""
fi

if [ -n "$REPO" ]; then
    previous=${previous:-$(git describe --tags --abbrev=0 "${TAG}^" 2>/dev/null || echo "")}
    echo ""
    if [ -n "$previous" ]; then
        echo "**Full Changelog**: https://github.com/${REPO}/compare/${previous}...${TAG}"
    else
        echo "**Full Changelog**: https://github.com/${REPO}/commits/${TAG}"
    fi
fi
