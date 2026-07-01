# Privacy Redactor — desktop tray app

A [Tauri 2](https://tauri.app) wrapper around the `pii-server` daemon. It runs the
GLiNER2 model in the background, lives in the system tray, and offers a settings
window (port, access token, detection threshold, per-label redaction toggles,
launch-at-login). The Chrome extension talks to it exactly as before.

```
src/              static settings UI (index.html / styles.css / main.js)
src-tauri/        the Rust/Tauri shell
  src/lib.rs      tray + server thread + IPC commands + JSON config persistence
  tauri.conf.json window, bundle targets (deb / appimage / nsis), icons
```

The server logic itself lives in `../server` (the `pii_server` library); this app
spawns `pii_server::Server` on a thread and mutates its live state from the GUI.

## Run in dev

Needs the ONNX runtime and the model, same as the bare daemon:

```sh
cd src-tauri
ORT_DYLIB_PATH=/path/to/libonnxruntime.so \
PII_MODELS_DIR=../../semplifica \
cargo run
```

The window starts hidden; click the tray icon to open settings. Closing the
window hides it back to the tray (use tray → Quit to exit).

Live settings (token, threshold, labels) apply immediately. Changing the port is
saved but only takes effect on the next launch (the model is too expensive to
reload to rebind).

## Build installers

`.deb` / `.AppImage` (Linux) and NSIS `.exe` (Windows). Requires the Tauri CLI:

```sh
npm exec --yes @tauri-apps/cli@2 build    # or: cargo tauri build
```

For a self-contained installer, the model (`semplifica/`) and a `libonnxruntime`
must be present as bundle resources before building — CI fetches the model from
HuggingFace (`HF_TOKEN` secret) and wires both into `tauri.conf.json`'s
`bundle.resources`. See the release workflow.
