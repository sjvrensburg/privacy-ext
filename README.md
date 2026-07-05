# privacy-ext — on-device PII redactor (server-client)

Detects and redacts PII pasted into web forms. A **local Rust daemon** runs the
GLiNER2 model (via `gliner2-rs`); a **thin Chrome extension** intercepts pastes,
asks the daemon, and offers redacted vs original text. Nothing leaves the machine.

```
content.js (paste hook) ──▶ background.js ──fetch──▶ 127.0.0.1:8731 (pii-server, Rust)
       ▲                                                      │ gliner2-rs (8 ONNX fragments)
       └────────────── redacted text / spans ◀────────────────┘
```

## 1. Run the daemon (`server/`)

Built with `tiny_http` (sync) + `gliner2_inference` (gliner2-rs), ort load-dynamic.

```sh
cd server
cargo build --release                 # already built: target/release/pii-server
# needs a libonnxruntime.so (ort load-dynamic); run.sh auto-finds one:
PII_TOKEN=<your-secret> ./run.sh
```

Env vars:
- `ORT_DYLIB_PATH` — path to `libonnxruntime.so` (run.sh auto-detects).
- `PII_MODELS_DIR` — local dir of the 8 ONNX fragments + `tokenizer.json` (skips the HF download). Otherwise the model is fetched from `stefanj0/gliner2-sa-names-lora` (`onnx_int8`) into the HF cache on first run.
- `PII_TOKEN` — bearer token the extension must send (recommended; otherwise the endpoint is open on loopback).
- `PII_PORT` (8731), `PII_LABELS` (comma list), `PII_MODEL_REPO`, `PII_SUBFOLDER`.

API: `GET /health` · `POST /classify {text, threshold?}` → `{entities:[{label,text,start,end,score}], redacted}`.

## 2. Load the extension (`extension-client/`)

`chrome://extensions` → Developer mode → **Load unpacked** → `extension-client/`.

Pairing is zero-config when the **desktop tray app** (`desktop/`) is running: it
persists its port + token and registers a Chrome Native Messaging host
(`ai.semplifica.privacy_redactor`); the extension calls that host on demand and
never needs a manually-typed URL or token. Open the popup to see the live
connection chip (green = paired and reachable) and a **Re-pair** button for
when the tray app's token has rotated. Then paste a sentence with PII into any
form field and choose **Insert redacted** or **Insert original**.

Running the headless daemon (`server/`) without the desktop app means there's
nothing to pair with — the desktop app is the supported way to configure the
extension.

## Security
- Daemon binds `127.0.0.1` only.
- Set `PII_TOKEN` so random local processes / web pages can't use it (the
  desktop app generates one automatically; see its `AppConfig`).
- CORS only echoes `Access-Control-Allow-Origin` for the pinned extension
  origin (`DEFAULT_EXTENSION_ORIGIN` in `server/src/main.rs`).

## Notes
- `gliner2-rs` gives ~0.999 confidence and returns ready-made `redacted` text.
- A pure in-browser WASM approach (no daemon) was prototyped, then dropped in
  favour of server-client; it remains in git history if ever needed.
