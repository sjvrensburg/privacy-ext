#!/usr/bin/env bash
# Convenience launcher: finds a libonnxruntime.so and starts the daemon.
# Override any of these via the environment before running.
set -euo pipefail
cd "$(dirname "$0")"

# Locate an onnxruntime shared library if ORT_DYLIB_PATH isn't already set.
if [[ -z "${ORT_DYLIB_PATH:-}" ]]; then
  ORT_DYLIB_PATH="$(find "$HOME" -name 'libonnxruntime.so*' 2>/dev/null | head -1 || true)"
  if [[ -z "$ORT_DYLIB_PATH" ]]; then
    echo "ERROR: no libonnxruntime.so found. Set ORT_DYLIB_PATH to one." >&2
    exit 1
  fi
fi
export ORT_DYLIB_PATH
echo "Using ORT_DYLIB_PATH=$ORT_DYLIB_PATH"

# PII_MODELS_DIR : local dir of the 8 ONNX fragments + tokenizer.json (skips HF download)
# PII_TOKEN      : bearer token the extension must send (recommended)
# PII_PORT       : default 8731
# PII_LABELS     : comma-separated entity labels
exec ./target/release/pii-server
