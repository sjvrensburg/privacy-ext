#!/usr/bin/env bash
# Builds the clipcloak-native-host sidecar and stages it where Tauri's `externalBin`
# expects it: src-tauri/binaries/<name>-<target-triple>[.exe]. Tauri strips the
# triple when it copies the sidecar into the bundle next to the main binary,
# which is where desktop/src-tauri/src/lib.rs::install_native_messaging_host
# looks for it at runtime.
set -euo pipefail
cd "$(dirname "$0")/.."

triple="$(rustc -vV | sed -n 's/^host: //p')"
ext=""
[[ "$triple" == *windows* ]] && ext=".exe"
staged="binaries/clipcloak-native-host-${triple}${ext}"

# tauri-build's build.rs checks that every externalBin path already exists
# before it lets ANYTHING in this package compile (including clipcloak-native-host
# itself) — so stage a placeholder first to break the chicken-and-egg, then
# overwrite it with the real binary once built.
mkdir -p binaries
[ -f "$staged" ] || : > "$staged"

cargo build --release --bin clipcloak-native-host

cp "target/release/clipcloak-native-host${ext}" "$staged"
echo "Staged ${staged}"
