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
- `PII_MODELS_DIR` — local dir of the 8 fp16 ONNX fragments + `tokenizer.json` (skips the ~620 MB HF download). Otherwise the model is fetched from `SemplificaAI/gliner2-privacy-filter-PII-multi` (`fp16_v2`) into the HF cache on first run.
- `PII_TOKEN` — bearer token the extension must send (recommended; otherwise the endpoint is open on loopback).
- `PII_PORT` (8731), `PII_LABELS` (comma list), `PII_MODEL_REPO`, `PII_SUBFOLDER`.

API: `GET /health` · `POST /classify {text, threshold?}` → `{entities:[{label,text,start,end,score}], redacted}`.

## 2. Load the extension (`extension-client/`)

`chrome://extensions` → Developer mode → **Load unpacked** → `extension-client/`.
Open the popup → set **Daemon URL** (default `http://127.0.0.1:8731`) and **Token**
(must match `PII_TOKEN`) → **Save** → **Test connection**. Then paste a sentence
with PII into any form field and choose **Insert redacted** or **Insert original**.

## Security
- Daemon binds `127.0.0.1` only.
- Set `PII_TOKEN` so random local processes / web pages can't use it.
- CORS is `*` for dev — tighten `Access-Control-Allow-Origin` in `server/src/main.rs`
  to your `chrome-extension://<id>` origin for production.

## Notes
- `gliner2-rs` gives ~0.999 confidence and returns ready-made `redacted` text.
- A pure in-browser WASM approach (no daemon) was prototyped, then dropped in
  favour of server-client; it remains in git history if ever needed.
