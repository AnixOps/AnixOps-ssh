#!/usr/bin/env bash
# Builds and signs the update feed of each desktop platform (RELEASING.md,
# "Update feed"): anixops-ssh-<windows|macos|linux>.appcast.json, in the
# anixops-appcast-v1 format of anix-kit. One item per platform: its installer,
# signed with the same key. A platform without an installer in ASSET_DIR is
# skipped with a warning.
#
#   scripts/build-update-feeds.sh TAG ASSET_DIR OUT_DIR
#
# Environment:
#   KEY_FILE    PKCS#8 PEM private key (anixsign keygen); required
#   REPO        owner/name of the release (default AnixOps/EasySSH)
#   SEQUENCE    feed sequence (default: now, as unix time)
#   KIT         anix-kit commit to run the tools from (default: the pinned one)
#   ANIXUPDATE, ANIXSIGN   commands to use instead of `go run`, for tests
set -euo pipefail

tag=${1:?tag}
assets=${2:?asset dir}
out=${3:?output dir}
: "${KEY_FILE:?KEY_FILE}"
repo=${REPO:-AnixOps/EasySSH}
sequence=${SEQUENCE:-$(date +%s)}
kit=${KIT:-73de268a2e0909b71ba1e1b80fd3470ae5a54aa9}
anixupdate=${ANIXUPDATE:-go run github.com/AnixOps/anix-kit/cmd/anixupdate@$kit}
anixsign=${ANIXSIGN:-go run github.com/AnixOps/anix-kit/cmd/anixsign@$kit}

version=${tag#v}
# The build number: major*1000000 + minor*1000 + patch, the same number the
# Android versionCode and iOS CFBundleVersion use, and what the app compares.
IFS=. read -r major minor patch <<< "${version%%[-+]*}"
build=$(( major * 1000000 + minor * 1000 + patch ))

root=$(cd "$(dirname "$0")/.." && pwd)
macos_min=$(jq -r '.bundle.macOS.minimumSystemVersion // "10.13"' "$root/src-tauri/tauri.conf.json")
notes=""
[ -f "$root/docs/releases/$tag.md" ] && notes=$(head -c 4000 "$root/docs/releases/$tag.md")

public=$($anixsign pub "$KEY_FILE")
mkdir -p "$out"
built=0

feed() { # platform, glob, minimum system version
  local platform=$1 pattern=$2 minimum=$3 product="anixops-ssh-$1" file
  file=$(find "$assets" -maxdepth 1 -type f -name "$pattern" | sort | head -1)
  if [ -z "$file" ]; then
    echo "::warning::no $pattern installer for $platform; its feed is not built"
    return 0
  fi
  local url="https://github.com/$repo/releases/download/$tag/$(basename "$file")"
  $anixupdate item "$KEY_FILE" "$product" "$version" "$build" "$minimum" "$url" "$file" \
    | jq --arg notes "$notes" '[. + {notes: $notes}]' > "$out/$product.items.json"
  $anixupdate feed "$KEY_FILE" "$product" stable "$sequence" "$out/$product.items.json" > "$out/$product.appcast.json"
  rm "$out/$product.items.json"
  # What users will run: the feed and the archive verify against the public key.
  $anixupdate verify "$public" "$product" "$out/$product.appcast.json" "$file" "$build"
  built=$(( built + 1 ))
}

feed windows '*-setup.exe' 0
feed macos '*.dmg' "$macos_min"
feed linux '*.AppImage' 0

echo "built $built feed(s) in $out (sequence $sequence, build $build)"
