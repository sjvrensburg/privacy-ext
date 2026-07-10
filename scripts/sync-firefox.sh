#!/usr/bin/env bash
# The Firefox build (extension-firefox/) shares all of its logic, styling and
# icons with the Chrome client (extension-client/). Only manifest.json differs
# (Gecko id + event-page background instead of a `key` + service worker). This
# script mirrors the shared files across so the two never drift. Re-run after
# editing anything in extension-client/ (except its manifest).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
src="$root/extension-client"
dst="$root/extension-firefox"

# Shared source + assets. background.js is cross-browser (it guards the
# worker-only importScripts); manifest.json is intentionally NOT copied.
for f in background.js content.js content.css popup.html popup.css popup.js ai-sites.js redact.html redact.css redact.js; do
  cp "$src/$f" "$dst/$f"
done

mkdir -p "$dst/icons"
cp "$src/icons/"*.png "$dst/icons/"

echo "Synced shared files from extension-client into extension-firefox."
