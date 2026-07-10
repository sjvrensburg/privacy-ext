// Headless daemon front-end: reads configuration from environment variables and
// runs the server defined in `lib.rs`. Behaviour is unchanged from the original
// single-file daemon; the GUI front-end (Tauri desktop app) uses the same lib
// with a `ServerConfig`/`LiveState` it drives from the tray instead.
//
// Run:  ORT_DYLIB_PATH=/path/to/libonnxruntime.so cargo run --release
// First run downloads the model (~620 MB) into the HF cache, unless
// PII_MODELS_DIR points at a local directory of the 8 ONNX fragments + tokenizer.

use clipcloak_server::{
    compile_rules, new_live_state, CompiledRule, LiveSettings, ModelSource, Server, ServerConfig,
    DEFAULT_EXTENSION_ORIGIN, DEFAULT_LABELS, DEFAULT_PORT, DEFAULT_THRESHOLD,
};

/// Parse `PII_RULES` (a JSON array of `{"name","pattern"}`) into compiled regex
/// rules. Bad JSON is ignored with a warning; malformed or uncompilable rules are
/// skipped — but each skip is logged, since a headless operator has no UI that
/// would otherwise flag a mistyped rule silently vanishing.
fn parse_rules(json: &str) -> Vec<CompiledRule> {
    let arr: Vec<serde_json::Value> = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("PII_RULES ignored (not a JSON array): {e}");
            return Vec::new();
        }
    };
    let mut specs: Vec<(String, String)> = Vec::new();
    for (i, v) in arr.iter().enumerate() {
        let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        let pattern = v.get("pattern").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if name.is_empty() || pattern.is_empty() {
            eprintln!("PII_RULES: rule #{i} skipped — needs a non-empty \"name\" and \"pattern\"");
            continue;
        }
        specs.push((name, pattern));
    }
    let (compiled, errors) = compile_rules(specs.iter().map(|(n, p)| (n.as_str(), p.as_str(), true)));
    for e in errors {
        eprintln!("PII_RULES: skipping rule {:?}: {}", e.name, e.error);
    }
    compiled
}

fn main() -> anyhow::Result<()> {
    if std::env::var("ORT_DYLIB_PATH").is_err() {
        eprintln!("WARNING: ORT_DYLIB_PATH not set; ort may fail to find libonnxruntime.so");
    }
    ort::init().with_name("clipcloak-server").commit()?;

    let model = if let Ok(dir) = std::env::var("PII_MODELS_DIR") {
        ModelSource::LocalDir(dir)
    } else {
        let repo = std::env::var("PII_MODEL_REPO")
            .unwrap_or_else(|_| "stefanj0/gliner2-sa-names-lora".to_string());
        let subfolder = std::env::var("PII_SUBFOLDER").unwrap_or_else(|_| "onnx_int8".to_string());
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

    let rules = std::env::var("PII_RULES")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| parse_rules(&s))
        .unwrap_or_default();

    if token.is_none() {
        eprintln!("WARNING: no PII_TOKEN set — the endpoint is unauthenticated (dev mode).");
    }
    eprintln!("Labels: {labels:?}");
    if !rules.is_empty() {
        eprintln!("Custom regex rules: {}", rules.len());
    }
    eprintln!("CORS allowed origins: {allowed_origins:?}");

    let config = ServerConfig { port, model, allowed_origins };
    let state = new_live_state(LiveSettings { token, labels, threshold, rules });
    let server = Server::new(config, state);
    server.run()
}
