// Local PII detection daemon: wraps the gliner2-rs engine in a tiny localhost
// HTTP API (tiny_http, synchronous — the model call is blocking and serialized
// anyway, so no async runtime is needed). The Chrome extension is a thin client
// that POSTs text and gets back PII spans + redacted text.
//
// Security: binds 127.0.0.1 only; optional bearer token (PII_TOKEN); CORS is
// locked to the pinned extension origin (see DEFAULT_EXTENSION_ORIGIN), override
// with PII_ALLOWED_ORIGINS.
//
// Run:  ORT_DYLIB_PATH=/path/to/libonnxruntime.so cargo run --release
// First run downloads the model (~620 MB) into the HF cache, unless
// PII_MODELS_DIR points at a local directory of the 8 ONNX fragments + tokenizer.

use gliner2_inference::{
    mask_pii_text, Gliner2Config, Gliner2Engine, InferenceParams, ModelType, SchemaTask,
};
use serde::{Deserialize, Serialize};
use std::io::Read;
use tiny_http::{Header, Method, Response, Server};

#[derive(Deserialize)]
struct ClassifyReq {
    text: String,
    threshold: Option<f32>,
}

#[derive(Serialize)]
struct Ent {
    label: String,
    text: String,
    start: usize,
    end: usize,
    score: f32,
}

#[derive(Serialize)]
struct ClassifyResp {
    entities: Vec<Ent>,
    redacted: String,
}

// Label phrasing matters to GLiNER2: the bare "address" label scores street
// addresses like "5 Elm Street" at ~0.44-0.51, under the 0.55 threshold, so they
// slip through. "street address" scores the same spans at ~0.84-0.98 — reliably
// caught without lowering the threshold (which would add false positives across
// every label). See the probe in vendor-gliner2-rs/examples/probe_address.rs.
const DEFAULT_LABELS: &[&str] = &[
    "name", "street address", "email", "phone_num", "id_num", "url", "username",
];

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

// Pinned identity of extension-client/ (derived from its manifest "key"). The
// extension's background fetch sends `Origin: chrome-extension://<id>`; we echo
// it back only when it matches, so other local processes / web origins can't use
// the daemon even though it's reachable on localhost.
const DEFAULT_EXTENSION_ORIGIN: &str = "chrome-extension://ihjamhkkcgbifajnbikldcjfamggnbaj";

// CORS headers for a request whose Origin resolved to `allow_origin` (None = the
// Origin wasn't in the allowlist, so we send no Allow-Origin and the browser
// blocks the response).
fn cors_headers(allow_origin: Option<&str>) -> Vec<Header> {
    let mut headers = vec![
        header("Access-Control-Allow-Methods", "POST, OPTIONS"),
        header("Access-Control-Allow-Headers", "authorization, content-type"),
        header("Vary", "Origin"),
    ];
    if let Some(origin) = allow_origin {
        headers.push(header("Access-Control-Allow-Origin", origin));
    }
    headers
}

// Echo the request Origin back only if it's in the allowlist.
fn resolve_origin<'a>(origin: Option<&'a str>, allowed: &[String]) -> Option<&'a str> {
    origin.filter(|o| allowed.iter().any(|a| a == o))
}

fn json_response(
    status: u16,
    body: &str,
    allow_origin: Option<&str>,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut resp = Response::from_string(body).with_status_code(status);
    resp.add_header(header("Content-Type", "application/json"));
    for h in cors_headers(allow_origin) {
        resp.add_header(h);
    }
    resp
}

fn main() -> anyhow::Result<()> {
    if std::env::var("ORT_DYLIB_PATH").is_err() {
        eprintln!("WARNING: ORT_DYLIB_PATH not set; ort may fail to find libonnxruntime.so");
    }
    ort::init().with_name("pii-server").commit()?;

    let repo = std::env::var("PII_MODEL_REPO")
        .unwrap_or_else(|_| "SemplificaAI/gliner2-privacy-filter-PII-multi".to_string());
    let subfolder = std::env::var("PII_SUBFOLDER").unwrap_or_else(|_| "fp16_v2".to_string());
    let labels: Vec<String> = std::env::var("PII_LABELS")
        .ok()
        .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_else(|| DEFAULT_LABELS.iter().map(|s| s.to_string()).collect());
    let token = std::env::var("PII_TOKEN").ok().filter(|s| !s.is_empty());
    // Origins allowed via CORS. Defaults to the pinned extension origin; override
    // with PII_ALLOWED_ORIGINS (comma-separated, e.g. to add a second build).
    let allowed_origins: Vec<String> = std::env::var("PII_ALLOWED_ORIGINS")
        .ok()
        .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_else(|| vec![DEFAULT_EXTENSION_ORIGIN.to_string()]);
    let port: u16 = std::env::var("PII_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(8731);

    let engine = if let Ok(dir) = std::env::var("PII_MODELS_DIR") {
        eprintln!("Loading GLiNER2 engine from local dir {dir}…");
        Gliner2Engine::new(Gliner2Config { models_dir: dir, max_width: 8, model_type: ModelType::HuggingFace })?
    } else {
        eprintln!("Loading GLiNER2 engine from {repo}/{subfolder} (first run downloads ~620 MB)…");
        Gliner2Engine::from_pretrained(&repo, Some(&subfolder), ModelType::HuggingFace)?
    };
    eprintln!("Model ready. Labels: {labels:?}");
    if token.is_none() {
        eprintln!("WARNING: no PII_TOKEN set — the endpoint is unauthenticated (dev mode).");
    }
    eprintln!("CORS allowed origins: {allowed_origins:?}");

    let server = Server::http(("127.0.0.1", port)).map_err(|e| anyhow::anyhow!("bind: {e}"))?;
    eprintln!("Listening on http://127.0.0.1:{port}");

    for mut request in server.incoming_requests() {
        let method = request.method().clone();
        let url = request.url().to_string();

        let origin = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Origin"))
            .map(|h| h.value.as_str().to_string());
        let allow_origin = resolve_origin(origin.as_deref(), &allowed_origins);

        // CORS preflight
        if method == Method::Options {
            let mut resp = Response::empty(204);
            for h in cors_headers(allow_origin) {
                resp.add_header(h);
            }
            let _ = request.respond(resp);
            continue;
        }

        if method == Method::Get && url == "/health" {
            let _ = request.respond(json_response(200, "{\"ok\":true}", allow_origin));
            continue;
        }

        if !(method == Method::Post && url == "/classify") {
            let _ = request.respond(json_response(404, "{\"error\":\"not found\"}", allow_origin));
            continue;
        }

        // optional bearer token
        if let Some(expected) = &token {
            let ok = request
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.as_str())
                .and_then(|s| s.strip_prefix("Bearer "))
                == Some(expected.as_str());
            if !ok {
                let _ = request.respond(json_response(401, "{\"error\":\"invalid token\"}", allow_origin));
                continue;
            }
        }

        let mut body = String::new();
        if request.as_reader().read_to_string(&mut body).is_err() {
            let _ = request.respond(json_response(400, "{\"error\":\"bad body\"}", allow_origin));
            continue;
        }
        let req: ClassifyReq = match serde_json::from_str(&body) {
            Ok(r) => r,
            Err(e) => {
                let _ = request.respond(json_response(400, &format!("{{\"error\":\"bad json: {e}\"}}"), allow_origin));
                continue;
            }
        };

        let threshold = req.threshold.unwrap_or(0.55);
        let tasks = vec![SchemaTask::Entities(labels.clone())];
        let params = InferenceParams { threshold, flat_ner: true };
        let resp_body = match engine.extract(&req.text, &tasks, Some(params)) {
            Ok((ents, _rel, _cls)) => {
                let redacted = mask_pii_text(&req.text, &ents);
                let entities = ents
                    .into_iter()
                    .map(|e| Ent { label: e.label, text: e.text, start: e.start_char, end: e.end_char, score: e.score })
                    .collect();
                serde_json::to_string(&ClassifyResp { entities, redacted }).unwrap()
            }
            Err(e) => {
                let _ = request.respond(json_response(500, &format!("{{\"error\":\"infer: {e}\"}}"), allow_origin));
                continue;
            }
        };
        let _ = request.respond(json_response(200, &resp_body, allow_origin));
    }

    Ok(())
}
