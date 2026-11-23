#!/usr/bin/env bash
#
# Notarize the macOS .dmg of a drafted release, staple the ticket to it, put it back,
# and rebuild the checksums over what is actually published.
#
# For the manual notarization route: CI signs the bundle but has no notary credentials,
# so the .dmg in the draft release is signed and un-notarized. This finishes the job
# from a Mac that holds the credentials in a keychain profile.
#
#   xcrun notarytool store-credentials "NGINTIP" \
#     --apple-id <apple-id> --team-id <team-id> --password <app-specific-password>
#
# Usage:
#   scripts/notarize-release.sh v0.4.0 [keychain-profile]
#
# The order below is the whole point. Stapling REWRITES the .dmg, so a checksum taken
# before it is a checksum of a file nobody will ever download. SHA256SUMS.txt is
# therefore rebuilt from the stapled file, last.

set -euo pipefail

TAG="${1:?usage: notarize-release.sh <tag> [keychain-profile]}"
PROFILE="${2:-NGINTIP}"

command -v gh >/dev/null || { echo "need the gh CLI" >&2; exit 1; }
command -v xcrun >/dev/null || { echo "need Xcode command line tools" >&2; exit 1; }
[ "$(uname)" = "Darwin" ] || { echo "notarization only runs on macOS" >&2; exit 1; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/ngintip-notarize.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"

echo "==> Downloading the assets of $TAG"
# Draft releases are reachable by tag to anyone who can see the repository.
gh release download "$TAG" --pattern '*.dmg' --dir . --clobber
DMG="$(ls -1 ./*.dmg 2>/dev/null | head -1)"
[ -n "${DMG:-}" ] || { echo "no .dmg in release $TAG" >&2; exit 1; }
echo "    $(basename "$DMG")"

echo "==> Checking it is signed before asking Apple to vouch for it"
# Notarization of an unsigned bundle fails deep inside Apple's service with a report
# that has to be fetched separately. Catching it here costs a second.
codesign -dv --verbose=2 "$DMG" 2>&1 | grep -E 'Authority|TeamIdentifier' || {
  echo "that .dmg is not signed; notarizing it would fail" >&2
  exit 1
}

echo "==> Submitting to Apple (this waits, and can take several minutes)"
if ! xcrun notarytool submit "$DMG" --keychain-profile "$PROFILE" --wait; then
  echo >&2
  echo "Rejected. The reason is in the log, not in the line above:" >&2
  echo "  xcrun notarytool history --keychain-profile \"$PROFILE\"" >&2
  echo "  xcrun notarytool log <submission-id> --keychain-profile \"$PROFILE\"" >&2
  exit 1
fi

echo "==> Stapling the ticket (this rewrites the .dmg)"
xcrun stapler staple "$DMG"
xcrun stapler validate "$DMG"

echo "==> What Gatekeeper will say about it"
MOUNT="$(mktemp -d "$WORK/mnt.XXXXXX")"
hdiutil attach "$DMG" -nobrowse -quiet -mountpoint "$MOUNT"
APP="$(ls -1d "$MOUNT"/*.app | head -1)"
# "source=Notarized Developer ID" is the line that means a user sees no warning at all.
spctl -a -vvv -t install "$APP" 2>&1 || true
hdiutil detach "$MOUNT" -quiet

echo "==> Putting the stapled .dmg back and rebuilding the checksums"
gh release upload "$TAG" "$DMG" --clobber

# Every other asset is downloaded only so the checksum file covers the whole release,
# not just the file that changed.
gh release download "$TAG" --dir . --clobber --pattern '*' || true
rm -f SHA256SUMS.txt
shasum -a 256 -- *.dmg *.msi *.exe *.deb *.rpm *.AppImage 2>/dev/null \
  | sed 's|\./||' > SHA256SUMS.txt
cat SHA256SUMS.txt
gh release upload "$TAG" SHA256SUMS.txt --clobber

echo
echo "Done. $TAG is notarized, stapled and its checksums match what is published."
echo "It is still a DRAFT: publishing is what tells every running copy there is an update."
