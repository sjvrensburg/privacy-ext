# CLAUDE.md

Guidance for working in this repo. User-facing setup lives in `README.md`.

## What this is

On-device PII redactor for web forms. **Server-client architecture**: a local
Rust daemon runs the GLiNER2 model; a thin Chrome (MV3) extension intercepts
pastes, asks the daemon, and offers redacted vs. original text. No text leaves
the machine.

```
content.js (paste hook) ─▶ background.js ─fetch─▶ 127.0.0.1:8731  (clipcloak-server, Rust)
       ▲                                                  │ gliner2-rs (8 ONNX fragments)
       └──────────── redacted text / spans ◀──────────────┘
```

## Layout

- `server/` — the Rust daemon (`clipcloak-server`). `tiny_http` (sync; the model call
  blocks and is serialized, so no async runtime) + `gliner2_inference`
  (vendored `gliner2-rs` at `server/vendor-gliner2-rs/`) + `ort` (load-dynamic).
  `src/lib.rs` holds the whole server (routing, windowing, `ort`/engine load,
  `ServerConfig`/`LiveState`, CORS); `src/main.rs` is a thin headless front-end
  that reads env vars and calls the lib. `run.sh` launches it. The desktop app
  drives the *same* lib with a config it owns.
- `desktop/` — the Tauri 2 tray GUI (`clipcloak-desktop`) that wraps the daemon
  for non-developers. `src-tauri/src/lib.rs` starts the server (as a library),
  persists port+token, and installs a Chrome Native Messaging host
  (`ai.semplifica.clipcloak`, built as the `clipcloak-native-host` sidecar bin)
  so the extension pairs with **zero manual token entry**. `src/` is the tray
  webview UI. This is the supported end-user path; the headless `server/` has
  nothing to pair with on its own.
- `extension-client/` — the MV3 thin client (manifest, background fetch,
  content-script paste hook + redaction UI, popup settings). No model code.
- `extension-firefox/` — the Gecko MV3 port. Shares every file with
  `extension-client/` except `manifest.json`; `scripts/sync-firefox.sh` mirrors
  the rest.
  `background.js` is cross-browser (it guards the worker-only `importScripts`).
- `assets/icon/` — icon masters (`master-color.png` / `master-light.png`, made
  with Nano Banana 2). `scripts/render-icons.sh` derives every shipped PNG/ICO
  (extension toolbar on/off, desktop tray, light-mode tile) from them.
- `semplifica/` — local copy of the 8 fp16 ONNX fragments + `tokenizer.json`
  (gitignored; used via `PII_MODELS_DIR` to skip the HF download).

## Build & run

```sh
# Headless daemon:
cd server && cargo build --release
PII_TOKEN=<secret> PII_MODELS_DIR=../semplifica ./run.sh   # or ./run.sh to auto-download

# Desktop tray app (bundles the daemon + native-messaging host):
cd desktop && ./src-tauri/scripts/prepare-sidecar.sh       # build+stage the sidecar first
npx tauri dev                                              # or `npx tauri build`
```
Then load `extension-client/` unpacked. With the tray app running, pairing is
automatic (native messaging); with only the headless daemon, set the matching
token in the popup manually.

After editing any shared extension file, run `scripts/sync-firefox.sh` to mirror
`extension-client/` → `extension-firefox/` (everything but `manifest.json`).

## Model

GLiNER2 (205M params, schema-driven). The default repo is now the SA-names
fine-tune `stefanj0/gliner2-sa-names-lora` (`onnx_int8` subfolder), auto-fetched
from HF on first run unless `PII_MODELS_DIR` points at local fragments; override
with `PII_MODEL_REPO`/`PII_SUBFOLDER`. Local model dirs: `semplifica/` (the
active fragments), `semplifica_baseline_backup/` and `semplifica_finetuned/`
(comparison sets). See `nguni-name-detection-gap` / `gliner2-finetune-pipeline`
memories for why the fine-tune exists. The model **cannot** be a single ONNX
graph — it's 8 fragments (encoder + schema_gather + count_* + token_gather +
span_rep + scorer + classifier), orchestrated by gliner2-rs. fp16 ≈ 620 MB.

## Gotchas (learned the hard way)

- **`ort` version pinning:** pin BOTH `ort` and `ort-sys` to `=2.0.0-rc.9` in
  `server/Cargo.toml` and `server/vendor-gliner2-rs/Cargo.toml`. A looser req
  pulls rc.12 (vitis.rs compile error) or a mismatched ort-sys (OrtApi field
  errors).
- **`ort` `ndarray` feature** must be enabled in the vendored Cargo.toml, or the
  lib fails with `IntoValueTensor` / `try_extract_tensor` errors.
- **`ORT_DYLIB_PATH`** must point at a `libonnxruntime.so` (ort uses
  load-dynamic). `run.sh` auto-finds one.
- The V2 engine needs **`tokenizer.json` inside `PII_MODELS_DIR`**.
- Tune detection with the popup **threshold** (~0.55). gliner2-rs returns
  ~0.999 confidence and a ready-made `redacted` string (`mask_pii_text`).
- **Long text is windowed:** model inference is ~O(n²) (5 k chars ≈ 37 s in one
  pass), so `/classify` scans text longer than `WINDOW_BYTES` (1500) in
  overlapping windows (`OVERLAP_BYTES` 300, > any single entity so nothing
  straddles a boundary), remaps offsets, pools, and masks once. The response
  carries `parts` (window count); the extension caps pastes at 20 k chars and
  the server rejects > `MAX_TEXT_BYTES` (40 k) with 413.

A pure in-browser WASM build (onnxruntime-web + transformers.js, no daemon) was
prototyped and validated, then dropped in favour of server-client. It lives in
git history and project memory if a serverless route is ever revived.

## Security

Daemon binds `127.0.0.1`; set `PII_TOKEN` (bearer) so other local processes /
web pages can't use it. CORS is locked to the pinned extension origin: the
extension's ID is fixed by the `key` in `extension-client/manifest.json`
(`ihjamhkkcgbifajnbikldcjfamggnbaj`), and the server only echoes
`Access-Control-Allow-Origin` for that origin (`DEFAULT_EXTENSION_ORIGIN` in
`server/src/main.rs`). Override with `PII_ALLOWED_ORIGINS` (comma-separated) for
a differently-keyed build. The signing key for the pinned ID lives in
`extension-client/.keys/` (gitignored) — needed only to re-pack/publish; the
process for reserving that fixed ID is in `docs/reserve-chrome-id.md`.

Firefox's extension origin (`moz-extension://<uuid>`) is randomised per install
and can't be pinned, so the Firefox build authorises native messaging by
extension id (`allowed_extensions` in the host manifest) and, if a CORS error
appears, is run against a daemon started with `PII_ALLOWED_ORIGINS=moz-extension://*`
(`resolve_origin` in `server/src/lib.rs` supports a trailing-`*` wildcard). The
bearer token stays the real access control.
