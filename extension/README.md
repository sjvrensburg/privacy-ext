# PII Redactor (GLiNER2, on-device) — MV3 extension

Detects and redacts PII pasted into web forms, **fully on-device** using a GLiNER2
model run with onnxruntime-web. No text leaves the browser.

## Architecture

```
content.js  ──PF_CLASSIFY──▶  background.js (service worker)
 (paste hook)                      │ ensures offscreen doc exists
 redaction UI  ◀──entities──       ▼
                              offscreen.html/js  ──▶  pipeline.js
                              (hosts the model)        (GLiNER2: 7 onnx graphs
                                                        + tokenizer, onnxruntime-web)
```

- **content.js** — intercepts `paste` into editable fields, sends text to the worker, and shows an inline "Insert redacted / Insert original" prompt.
- **background.js** — MV3 service worker; can't run the model itself (no DOM, dies when idle), so it just creates/relays to the offscreen document.
- **offscreen.html/js** — long-lived page that hosts the model and runs inference.
- **pipeline.js** — the validated GLiNER2 pipeline (feed build → encoder → schema_gather → count → span_rep → scorer → decode).

## Models (not bundled — fetched + cached on first run)

The 7 fp16 ONNX graphs (~620 MB total, encoder dominates) are **downloaded at first
use** and stored via the Cache API (`pii-models-v1`). Default source is the
SemplificaAI HF repo; override via the popup's **Model base URL** (a directory
containing `encoder_fp16.onnx`, `schema_gather_fp16.onnx`, … ). Use **Preload model**
in the popup to warm the cache before pasting.

The tokenizer **is** bundled (`models/gliner2-tokenizer/`) — required for correctness.

## Load it (unpacked)

1. `chrome://extensions` → enable Developer mode → **Load unpacked** → select this `extension/` folder.
2. Click the toolbar icon → **Preload model** (first run downloads the model).
3. Paste text containing PII into any form field → choose redacted or original.

## Important notes / gotchas

- **transformers.js is pinned to 3.8.1** (`vendor/transformers.min.js`). Other
  versions tokenize differently (drop the leading sentencepiece metaspace), which
  silently breaks detection because `schema_positions` are baked. Do not bump
  without re-validating the feed.
- Runs **single-threaded WASM** (`ort.env.wasm.numThreads = 1`) so it needs **no
  cross-origin isolation** (no SharedArrayBuffer). For speed you can switch
  `executionProviders` to `["webgpu"]` in `pipeline.js` (jsep wasm is vendored) —
  test on target hardware first.
- Detection **threshold** defaults to 0.55 (configurable in the popup); true hits
  score ~0.72–0.73, neutral false-positives sit at ~0.5.
- `password` inputs are deliberately skipped.
- The model is fp16 / size-heavy. Quantizing the encoder is the obvious next size win.

## Status

Core pipeline (`pipeline.js` + vendored runtimes + tokenizer) is **validated working
in a real browser** (headless Chrome via onnxruntime-web). The MV3 wiring
(content/background/offscreen messaging) follows standard patterns and needs
testing in a live Chrome profile.
