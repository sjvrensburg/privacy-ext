// Headless daemon front-end: reads configuration from environment variables and
// runs the server defined in `lib.rs`. Behaviour is unchanged from the original
// single-file daemon; the GUI front-end (Tauri desktop app) uses the same lib
// with a `ServerConfig`/`LiveState` it drives from the tray instead.
//
// Run:  ORT_DYLIB_PATH=/path/to/libonnxruntime.so cargo run --release
// First run downloads the model (~620 MB) into the HF cache, unless
// PII_MODELS_DIR points at a local directory of the 8 ONNX fragments + tokenizer.

use pii_server::{
    new_live_state, LiveSettings, ModelSource, Server, ServerConfig, DEFAULT_EXTENSION_ORIGIN,
    DEFAULT_LABELS, DEFAULT_PORT, DEFAULT_THRESHOLD,
};

fn main() -> anyhow::Result<()> {
    if std::env::var("ORT_DYLIB_PATH").is_err() {
        eprintln!("WARNING: ORT_DYLIB_PATH not set; ort may fail to find libonnxruntime.so");
    }
    ort::init().with_name("pii-server").commit()?;

    let model = if let Ok(dir) = std::env::var("PII_MODELS_DIR") {
        ModelSource::LocalDir(dir)
    } else {
        let repo = std::env::var("PII_MODEL_REPO")
            .unwrap_or_else(|_| "SemplificaAI/gliner2-privacy-filter-PII-multi".to_string());
        let subfolder = std::env::var("PII_SUBFOLDER").unwrap_or_else(|_| "fp16_v2".to_string());
        ModelSource::HuggingFace { repo, subfolder }
    };

    let labels: Vec<String> = std::env::var("PII_LABELS")
        .ok()
        .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_else(|| DEFAULT_LABELS.iter().map(|s| s.to_string()).collect());
    let token = std::env::var("PII_TOKEN").ok().filter(|s| !s.is_empty());
    let allowed_origins: Vec<String> = std::env::var("PII_ALLOWED_ORIGINS")
        .ok()
        .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_else(|| vec![DEFAULT_EXTENSION_ORIGIN.to_string()]);
    let port: u16 = std::env::var("PII_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_PORT);
    let threshold: f32 = std::env::var("PII_THRESHOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_THRESHOLD);

    if token.is_none() {
        eprintln!("WARNING: no PII_TOKEN set — the endpoint is unauthenticated (dev mode).");
    }
    eprintln!("Labels: {labels:?}");
    eprintln!("CORS allowed origins: {allowed_origins:?}");

    let config = ServerConfig { port, model, allowed_origins };
    let state = new_live_state(LiveSettings { token, labels, threshold });
    let server = Server::new(config, state);
    server.run()
}
