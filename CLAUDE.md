# CLAUDE.md

Guidance for working in this repo. User-facing setup lives in `README.md`.

## What this is

On-device PII redactor for web forms. **Server-client architecture**: a local
Rust daemon runs the GLiNER2 model; a thin Chrome (MV3) extension intercepts
pastes, asks the daemon, and offers redacted vs. original text. No text leaves
the machine.

```
content.js (paste hook) ─▶ background.js ─fetch─▶ 127.0.0.1:8731  (pii-server, Rust)
       ▲                                                  │ gliner2-rs (8 ONNX fragments)
       └──────────── redacted text / spans ◀──────────────┘
```

## Layout

- `server/` — the Rust daemon (`pii-server`). `tiny_http` (sync; the model call
  blocks and is serialized, so no async runtime) + `gliner2_inference`
  (vendored `gliner2-rs` at `server/vendor-gliner2-rs/`) + `ort` (load-dynamic).
  `src/main.rs` is the whole server; `run.sh` launches it.
- `extension-client/` — the MV3 thin client (manifest, background fetch,
  content-script paste hook + redaction UI, popup settings). No model code.
- `semplifica/` — local copy of the 8 fp16 ONNX fragments + `tokenizer.json`
  (gitignored; used via `PII_MODELS_DIR` to skip the HF download).

## Build & run

```sh
cd server && cargo build --release
PII_TOKEN=<secret> PII_MODELS_DIR=../semplifica ./run.sh   # or ./run.sh to auto-download
```
Then load `extension-client/` unpacked and set the matching token in the popup.

## Model

GLiNER2 (`SemplificaAI/gliner2-privacy-filter-PII-multi`, 205M params,
schema-driven). It **cannot** be a single ONNX graph — it's 8 fragments
(encoder + schema_gather + count_* + token_gather + span_rep + scorer +
classifier), orchestrated by gliner2-rs. fp16 ≈ 620 MB total.

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
`extension-client/.keys/` (gitignored) — needed only to re-pack/publish.
