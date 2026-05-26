#!/usr/bin/env bash
# Roll back a published rekody version everywhere it lives.
#
# A rekody release fans out to 4 places (see CLAUDE.md "Release & rollback surface"):
#   1. Workspace Cargo.toml version
#   2. Git tag on rekody/rekody
#   3. GitHub Release at rekody/rekody
#   4. Homebrew tap formula at rekody/homebrew-rekody
#
# This script undoes 2, 3, and 4. Step 1 (reverting the version-bump commit on
# main) is intentionally NOT automated — that's a code change you should review
# and revert by hand with `git revert`.
#
# Usage:  scripts/rollback.sh <bad-version>
# Example: scripts/rollback.sh 0.5.5
#
# Idempotent: each step checks state and skips if already done.

set -euo pipefail

VERSION="${1:-}"
if [[ -z "$VERSION" ]]; then
  echo "usage: $0 <version>   (e.g. $0 0.5.5)" >&2
  exit 2
fi
VERSION="${VERSION#v}"  # accept "0.5.5" or "v0.5.5"
TAG="v$VERSION"

REPO="rekody/rekody"
TAP_REPO="rekody/homebrew-rekody"

# --- preflight ---------------------------------------------------------------

command -v gh   >/dev/null || { echo "gh not found"   >&2; exit 1; }
command -v git  >/dev/null || { echo "git not found"  >&2; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "gh not authenticated — run 'gh auth login'" >&2; exit 1; }

echo "About to roll back $TAG everywhere it lives:"
echo "  - GitHub Release at $REPO"
echo "  - Git tag $TAG (local + remote)"
echo "  - Homebrew tap formula commit at $TAP_REPO"
echo
echo "NOT touched by this script:"
echo "  - Workspace Cargo.toml (revert the version-bump commit by hand)"
echo "  - Your local brew install (run 'brew update && brew upgrade rekody' after)"
echo
read -r -p "Proceed? [y/N] " ans
[[ "$ans" =~ ^[Yy]$ ]] || { echo "aborted"; exit 1; }

# --- 1. GitHub Release -------------------------------------------------------

echo
echo "==> Deleting GitHub Release $TAG…"
if gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
  gh release delete "$TAG" --repo "$REPO" --yes
  echo "    deleted."
else
  echo "    not present, skipping."
fi

# --- 2. Tags (remote first, then local) --------------------------------------

echo
echo "==> Deleting remote tag $TAG…"
if git ls-remote --tags origin "refs/tags/$TAG" | grep -q .; then
  git push origin ":refs/tags/$TAG"
  echo "    deleted."
else
  echo "    not present on origin, skipping."
fi

echo
echo "==> Deleting local tag $TAG…"
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  git tag -d "$TAG"
  echo "    deleted."
else
  echo "    not present locally, skipping."
fi

# --- 3. Homebrew tap formula -------------------------------------------------

echo
echo "==> Reverting Homebrew tap commit for $TAG…"

TAP_DIR="$(mktemp -d)"
trap 'rm -rf "$TAP_DIR"' EXIT
git clone --quiet "https://github.com/$TAP_REPO.git" "$TAP_DIR/tap"
pushd "$TAP_DIR/tap" >/dev/null

CURRENT_TAP_VERSION="$(grep -E '^\s*version\s+"' Formula/rekody.rb | sed -E 's/.*"([^"]+)".*/\1/')"

if [[ "$CURRENT_TAP_VERSION" != "$VERSION" ]]; then
  echo "    formula is on $CURRENT_TAP_VERSION (not $VERSION), skipping tap revert."
else
  # Find the most recent commit that bumped the formula to this version.
  BAD_SHA="$(git log --format='%H %s' -- Formula/rekody.rb \
    | awk -v v="$VERSION" '$0 ~ ("update rekody to v" v) {print $1; exit}')"

  if [[ -z "$BAD_SHA" ]]; then
    echo "    couldn't find a 'chore: update rekody to v$VERSION' commit in tap history." >&2
    echo "    inspect manually: https://github.com/$TAP_REPO/commits/main" >&2
    popd >/dev/null
    exit 1
  fi

  echo "    reverting tap commit $BAD_SHA…"
  git revert --no-edit "$BAD_SHA" >/dev/null
  git push origin main
  echo "    pushed."
fi

popd >/dev/null

# --- 4. Verify ---------------------------------------------------------------

echo
echo "==> Verifying all four surfaces…"

OK=1

if gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
  echo "    [FAIL] GitHub Release $TAG still exists"; OK=0
else
  echo "    [ ok ] GitHub Release $TAG gone"
fi

if git ls-remote --tags origin "refs/tags/$TAG" | grep -q .; then
  echo "    [FAIL] remote tag $TAG still exists"; OK=0
else
  echo "    [ ok ] remote tag $TAG gone"
fi

TAP_NOW="$(gh api "repos/$TAP_REPO/contents/Formula/rekody.rb" --jq .content \
  | base64 -d | grep -E '^\s*version\s+"' | sed -E 's/.*"([^"]+)".*/\1/')"
if [[ "$TAP_NOW" == "$VERSION" ]]; then
  echo "    [FAIL] tap formula still on $VERSION"; OK=0
else
  echo "    [ ok ] tap formula on $TAP_NOW (not $VERSION)"
fi

echo
if [[ $OK -eq 1 ]]; then
  echo "Rollback complete. Next:"
  echo "  1. Revert the version-bump commit on main with 'git revert <sha>' if you haven't."
  echo "  2. Run 'brew update && brew upgrade rekody' to refresh your local install."
  echo "  3. Confirm with 'rekody --version'."
else
  echo "Rollback INCOMPLETE — see [FAIL] lines above." >&2
  exit 1
fi
