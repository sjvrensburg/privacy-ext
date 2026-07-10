// Local PII detection daemon as a library. The Chrome extension is a thin client
// that POSTs text and gets back PII spans + redacted text; this crate runs the
// gliner2-rs engine behind a tiny localhost HTTP API (tiny_http, synchronous —
// the model call is blocking and serialized anyway, so no async runtime).
//
// Two front-ends consume this lib:
//   * `main.rs`  — the headless env-var-configured daemon (unchanged behaviour).
//   * the Tauri desktop app — spawns `Server::run` on a background thread and
//     mutates `LiveState` from the tray/settings GUI without a restart.
//
// Split of config:
//   * `ServerConfig` (port, models_dir, allowed_origins) is fixed for the life of
//     a running server — changing the port means stopping and starting again.
//   * `LiveState` (token, labels, threshold) is behind an RwLock so the GUI can
//     retune detection on the fly; every request reads the current values.

use gliner2_inference::{
    mask_pii_text, ExtractedEntity, Gliner2Config, Gliner2Engine, InferenceParams, ModelType,
    SchemaTask,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tiny_http::{Header, Method, Response, Server as HttpServer};

// Label phrasing matters to GLiNER2: the bare "address" label scores street
// addresses like "5 Elm Street" at ~0.44-0.51, under the 0.55 threshold, so they
// slip through. "street address" scores the same spans at ~0.84-0.98 — reliably
// caught without lowering the threshold (which would add false positives across
// every label). See the probe in vendor-gliner2-rs/examples/probe_address.rs.
pub const DEFAULT_LABELS: &[&str] = &[
    "name", "street address", "email", "phone_num", "id_num", "url", "username",
];

// Pinned identity of extension-client/ (derived from its manifest "key"). The
// extension's background fetch sends `Origin: chrome-extension://<id>`; we echo
// it back only when it matches, so other local processes / web origins can't use
// the daemon even though it's reachable on localhost.
pub const DEFAULT_EXTENSION_ID: &str = "ihjamhkkcgbifajnbikldcjfamggnbaj";
pub const DEFAULT_EXTENSION_ORIGIN: &str = "chrome-extension://ihjamhkkcgbifajnbikldcjfamggnbaj";

/// Build a `chrome-extension://<id>` origin from a raw extension id. Used by the
/// desktop app to derive both the daemon's CORS allow-list and the native-host
/// `allowed_origins` from a single configurable id list — so if the Chrome Web
/// Store assigns an id that differs from the pinned one, adding it is a config
/// edit, not a recompile. See `chrome_extension_ids` in the desktop AppConfig.
pub fn chrome_extension_origin(id: &str) -> String {
    format!("chrome-extension://{id}")
}

pub const DEFAULT_PORT: u16 = 8731;
pub const DEFAULT_THRESHOLD: f32 = 0.55;

// Score assigned to a user regex match. Higher than any model confidence (which
// tops out near 1.0), so in `dedup_entities` / `mask_pii_text` a user rule always
// wins an overlap against a model span — "rules run before the model".
pub const RULE_SCORE: f32 = 2.0;

// Defense-in-depth ceilings for user-supplied patterns. Rust's regex engine is
// already linear-time (no catastrophic backtracking / ReDoS), but a pathological
// pattern can still compile to a large automaton; bound it.
const REGEX_SIZE_LIMIT: usize = 1 << 20; // 1 MiB compiled program
const REGEX_DFA_SIZE_LIMIT: usize = 1 << 20;
// Cap matches PER RULE so a `.`-style rule on a big paste can't produce an
// unbounded entity list. Applied per rule (not to the running total) so a broad
// early rule can't exhaust the budget and starve later, more specific rules.
const MAX_MATCHES_PER_RULE: usize = 512;

/// A user-defined regex redaction rule, compiled and ready to run. `label` is the
/// human name of the rule; it doubles as the entity label and (normalised) the
/// mask tag, e.g. a rule named "Account number" masks to `[ACCOUNT_NUMBER]`.
#[derive(Clone, Debug)]
pub struct CompiledRule {
    pub label: String,
    pub re: regex::Regex,
}

impl CompiledRule {
    /// Compile a pattern with the standard size limits. Returns the regex error
    /// message (not the opaque Debug form) so the GUI can show it to the user.
    pub fn new(label: impl Into<String>, pattern: &str) -> Result<Self, String> {
        let re = regex::RegexBuilder::new(pattern)
            .size_limit(REGEX_SIZE_LIMIT)
            .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(CompiledRule { label: label.into(), re })
    }
}

/// One matched rule, for the desktop "test filters" panel. The panel only needs
/// the rule name (it shows a per-rule count and a redacted preview), so that is
/// all this carries — no unused span/offset fields.
#[derive(Clone, Debug, Serialize)]
pub struct RuleHit {
    /// The rule (label) that matched.
    pub rule: String,
}

/// A rule that failed to compile, in a shape the desktop GUI can surface.
#[derive(Clone, Debug, Serialize)]
pub struct RuleCompileError {
    pub name: String,
    pub error: String,
}

/// Compile a set of `(name, pattern, enabled)` rules into ready-to-run
/// `CompiledRule`s, splitting out per-rule compile errors. Disabled rules and
/// those with a blank (trimmed) name or empty pattern are skipped silently.
/// Single source of truth for "which rules are live" — the headless daemon, the
/// desktop config, and the test-filters preview all go through here so they can
/// never drift.
pub fn compile_rules<'a, I>(rules: I) -> (Vec<CompiledRule>, Vec<RuleCompileError>)
where
    I: IntoIterator<Item = (&'a str, &'a str, bool)>,
{
    let mut compiled = Vec::new();
    let mut errors = Vec::new();
    for (name, pattern, enabled) in rules {
        let name = name.trim();
        if !enabled || name.is_empty() || pattern.is_empty() {
            continue;
        }
        match CompiledRule::new(name, pattern) {
            Ok(c) => compiled.push(c),
            Err(error) => errors.push(RuleCompileError { name: name.to_string(), error }),
        }
    }
    (compiled, errors)
}

/// Regex-only redaction preview: run `rules` over `text` and return the matched
/// rule names plus the masked string, using the exact same matcher and masking
/// the live daemon uses. Lets the settings UI test rules without the model.
pub fn test_rules_preview(text: &str, rules: &[CompiledRule]) -> (Vec<RuleHit>, String) {
    let ents = regex_entities(text, rules);
    let masked = mask_pii_text(text, &ents);
    let hits = ents.iter().map(|e| RuleHit { rule: e.label.clone() }).collect();
    (hits, masked)
}

/// Run every rule over `text`, returning matches as entities with `RULE_SCORE`.
/// Match offsets are byte offsets (what `mask_pii_text` and the windowing code
/// use), so these pool directly with model spans. Empty matches are skipped, and
/// each rule contributes at most `MAX_MATCHES_PER_RULE` matches (a per-rule cap,
/// so one broad rule can't starve the others).
pub fn regex_entities(text: &str, rules: &[CompiledRule]) -> Vec<ExtractedEntity> {
    let mut out = Vec::new();
    for rule in rules {
        let mut n = 0;
        for m in rule.re.find_iter(text) {
            if m.start() == m.end() {
                continue;
            }
            out.push(ExtractedEntity {
                text: m.as_str().to_string(),
                label: rule.label.clone(),
                score: RULE_SCORE,
                start_tok: 0,
                end_tok: 0,
                start_char: m.start(),
                end_char: m.end(),
            });
            n += 1;
            if n >= MAX_MATCHES_PER_RULE {
                break;
            }
        }
    }
    out
}

/// Where the model lives. Bundled builds point at a local dir; otherwise the
/// engine downloads from HuggingFace on first run.
#[derive(Clone, Debug)]
pub enum ModelSource {
    LocalDir(String),
    HuggingFace { repo: String, subfolder: String },
}

/// Fixed-for-the-run configuration. Changing any of these requires a restart.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub port: u16,
    pub model: ModelSource,
    pub allowed_origins: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            port: DEFAULT_PORT,
            model: ModelSource::HuggingFace {
                repo: "stefanj0/gliner2-sa-names-lora".to_string(),
                subfolder: "onnx_int8".to_string(),
            },
            allowed_origins: vec![DEFAULT_EXTENSION_ORIGIN.to_string()],
        }
    }
}

/// Hot-swappable detection settings. The GUI mutates these live; each request
/// reads a snapshot under the lock.
#[derive(Clone, Debug)]
pub struct LiveSettings {
    /// Bearer token the extension must present. `None` = unauthenticated (dev).
    pub token: Option<String>,
    /// Entity labels to detect (this is the "what to redact" toggle set).
    pub labels: Vec<String>,
    pub threshold: f32,
    /// User-defined regex rules, applied alongside the model with precedence over
    /// it (see `regex_entities` / `RULE_SCORE`). Empty by default.
    pub rules: Vec<CompiledRule>,
}

impl Default for LiveSettings {
    fn default() -> Self {
        LiveSettings {
            token: None,
            labels: DEFAULT_LABELS.iter().map(|s| s.to_string()).collect(),
            threshold: DEFAULT_THRESHOLD,
            rules: Vec::new(),
        }
    }
}

/// Shared, mutable detection settings handed to the GUI and read per-request.
pub type LiveState = Arc<RwLock<LiveSettings>>;

pub fn new_live_state(settings: LiveSettings) -> LiveState {
    Arc::new(RwLock::new(settings))
}

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
    // How many overlapping windows the text was scanned in (1 for short text).
    // Lets the client tell the user a large paste was chunked.
    parts: usize,
}

// Model inference is ~O(n²) in input length, so long text is scanned in
// overlapping windows rather than one slow pass. The overlap must exceed the
// longest single entity so nothing detected straddles a boundary and is lost.
const WINDOW_BYTES: usize = 1500;
const OVERLAP_BYTES: usize = 300;
// Hard ceiling: refuse absurdly large bodies (the extension caps well below
// this) so a single request can't monopolise the serialized daemon for
// minutes. ~26 windows at the step size above.
const MAX_TEXT_BYTES: usize = 40_000;

// Byte ranges of overlapping windows covering `text`, each starting on a char
// boundary. Returns a single (0, text) window when the text fits in one pass.
fn windows(text: &str) -> Vec<(usize, &str)> {
    let n = text.len();
    if n <= WINDOW_BYTES {
        return vec![(0, text)];
    }
    let step = WINDOW_BYTES - OVERLAP_BYTES;
    let bump = |mut i: usize| {
        while i < n && !text.is_char_boundary(i) {
            i += 1;
        }
        i
    };
    let mut out = Vec::new();
    let mut start = 0;
    loop {
        let end = bump((start + WINDOW_BYTES).min(n));
        out.push((start, &text[start..end]));
        if end >= n {
            break;
        }
        start = bump(start + step);
    }
    out
}

// Collapse entities pooled from overlapping windows into a non-overlapping set,
// mirroring mask_pii_text's own selection (highest score, then longest span,
// then earliest) so the reported list and the redacted string agree exactly.
fn dedup_entities(mut ents: Vec<ExtractedEntity>) -> Vec<ExtractedEntity> {
    ents.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then((b.end_char - b.start_char).cmp(&(a.end_char - a.start_char)))
            .then(a.start_char.cmp(&b.start_char))
    });
    let mut selected: Vec<ExtractedEntity> = Vec::new();
    for e in ents {
        let overlaps = selected
            .iter()
            .any(|s| !(e.end_char <= s.start_char || e.start_char >= s.end_char));
        if !overlaps {
            selected.push(e);
        }
    }
    selected.sort_by_key(|e| e.start_char);
    selected
}

// Trim leading/trailing ASCII whitespace bytes off a [start,end) byte range so a
// residual model sub-span doesn't swallow the separator next to a rule mask
// (e.g. "John Smith" with a rule on "John" leaves "Smith", not " Smith"). ASCII
// whitespace bytes are char boundaries, so this stays boundary-safe.
fn trim_range(text: &str, mut start: usize, mut end: usize) -> (usize, usize) {
    let b = text.as_bytes();
    while start < end && b[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && b[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    (start, end)
}

// Combine model spans with user rule spans, giving rules precedence *only over
// the bytes they actually cover*. A rule that partially overlaps a longer model
// span carves out its own range and the model span's uncovered remainder is kept
// (re-masked under the model label) rather than dropped wholesale — otherwise a
// rule on "John" would turn a "John Smith" name span into "[JOHN] Smith" and leak
// the surname. Returns a non-overlapping set sorted by position.
fn resolve_entities(
    text: &str,
    model: Vec<ExtractedEntity>,
    rules: Vec<ExtractedEntity>,
) -> Vec<ExtractedEntity> {
    let model = dedup_entities(model);
    let rules = dedup_entities(rules); // rules are non-overlapping among themselves
    let mut out: Vec<ExtractedEntity> = Vec::new();

    for m in &model {
        // Clip each overlapping rule to this model span, in order.
        let mut cuts: Vec<(usize, usize)> = rules
            .iter()
            .filter(|r| r.start_char < m.end_char && r.end_char > m.start_char)
            .map(|r| (r.start_char.max(m.start_char), r.end_char.min(m.end_char)))
            .collect();
        cuts.sort_by_key(|c| c.0);

        // Emit the gaps between the cuts as residual model sub-spans.
        let mut cursor = m.start_char;
        for (s, e) in cuts {
            if s > cursor {
                push_subspan(&mut out, m, text, cursor, s);
            }
            cursor = cursor.max(e);
        }
        if cursor < m.end_char {
            push_subspan(&mut out, m, text, cursor, m.end_char);
        }
    }

    out.extend(rules);
    out.sort_by_key(|e| e.start_char);
    out
}

fn push_subspan(
    out: &mut Vec<ExtractedEntity>,
    base: &ExtractedEntity,
    text: &str,
    start: usize,
    end: usize,
) {
    let (start, end) = trim_range(text, start, end);
    if start >= end {
        return;
    }
    out.push(ExtractedEntity {
        text: text.get(start..end).unwrap_or_default().to_string(),
        label: base.label.clone(),
        score: base.score,
        start_tok: 0,
        end_tok: 0,
        start_char: start,
        end_char: end,
    });
}

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

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

// An allowed entry ending in `*` is a prefix wildcard. This exists mainly for
// Firefox: its extension origin is `moz-extension://<uuid>` where the uuid is
// randomised per install and so can't be pinned at build time the way the
// Chrome id is. A user running the Firefox build can set
// `PII_ALLOWED_ORIGINS=moz-extension://*` (the bearer token remains the real
// access control). Chrome's exact origin still matches via the `a == o` arm.
fn resolve_origin<'a>(origin: Option<&'a str>, allowed: &[String]) -> Option<&'a str> {
    origin.filter(|o| {
        allowed.iter().any(|a| match a.strip_suffix('*') {
            Some(prefix) => o.starts_with(prefix),
            None => a == o,
        })
    })
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

/// Initialise the ONNX Runtime (load-dynamic). Call once before loading a model.
/// `ORT_DYLIB_PATH` must point at a `libonnxruntime` shared lib.
pub fn init_ort() -> anyhow::Result<()> {
    ort::init().with_name("clipcloak-server").commit()?;
    Ok(())
}

/// Build the GLiNER2 engine for the given model source. Expensive (loads ~620 MB
/// of ONNX); call once per server lifetime.
pub fn load_engine(model: &ModelSource) -> anyhow::Result<Gliner2Engine> {
    match model {
        ModelSource::LocalDir(dir) => {
            eprintln!("Loading GLiNER2 engine from local dir {dir}…");
            Ok(Gliner2Engine::new(Gliner2Config {
                models_dir: dir.clone(),
                max_width: 8,
                model_type: ModelType::HuggingFace,
            })?)
        }
        ModelSource::HuggingFace { repo, subfolder } => {
            eprintln!("Loading GLiNER2 engine from {repo}/{subfolder} (first run downloads ~620 MB)…");
            Ok(Gliner2Engine::from_pretrained(repo, Some(subfolder), ModelType::HuggingFace)?)
        }
    }
}

/// A running (or runnable) server. `run` blocks serving requests until `stop` is
/// called from another thread (the tray menu) — it polls so the loop can notice
/// the shutdown flag even when idle.
pub struct Server {
    config: ServerConfig,
    state: LiveState,
    shutdown: Arc<AtomicBool>,
}

impl Server {
    pub fn new(config: ServerConfig, state: LiveState) -> Self {
        Server { config, state, shutdown: Arc::new(AtomicBool::new(false)) }
    }

    /// A handle that, when set, makes a blocked `run` return at the next poll.
    pub fn shutdown_handle(&self) -> Arc<AtomicBool> {
        self.shutdown.clone()
    }

    /// Load the model and serve until shutdown. Blocks the calling thread.
    pub fn run(&self) -> anyhow::Result<()> {
        let engine = load_engine(&self.config.model)?;
        eprintln!("Model ready.");
        self.serve(engine)
    }

    /// Serve with an already-loaded engine (lets the GUI show "model ready"
    /// before binding, and reuse the engine across port restarts).
    pub fn serve(&self, engine: Gliner2Engine) -> anyhow::Result<()> {
        let server = HttpServer::http(("127.0.0.1", self.config.port))
            .map_err(|e| anyhow::anyhow!("bind 127.0.0.1:{}: {e}", self.config.port))?;
        eprintln!("Listening on http://127.0.0.1:{}", self.config.port);

        loop {
            if self.shutdown.load(Ordering::Relaxed) {
                eprintln!("Shutdown requested; stopping server.");
                return Ok(());
            }
            // Poll so an idle server still notices the shutdown flag.
            let request = match server.recv_timeout(Duration::from_millis(250)) {
                Ok(Some(req)) => req,
                Ok(None) => continue,
                Err(e) => {
                    eprintln!("recv error: {e}");
                    continue;
                }
            };
            self.handle(request, &engine);
        }
    }

    fn handle(&self, mut request: tiny_http::Request, engine: &Gliner2Engine) {
        let method = request.method().clone();
        let url = request.url().to_string();

        let origin = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Origin"))
            .map(|h| h.value.as_str().to_string());
        let allow_origin = resolve_origin(origin.as_deref(), &self.config.allowed_origins);

        if method == Method::Options {
            let mut resp = Response::empty(204);
            for h in cors_headers(allow_origin) {
                resp.add_header(h);
            }
            let _ = request.respond(resp);
            return;
        }

        if method == Method::Get && url == "/health" {
            let _ = request.respond(json_response(200, "{\"ok\":true}", allow_origin));
            return;
        }

        if !(method == Method::Post && url == "/classify") {
            let _ = request.respond(json_response(404, "{\"error\":\"not found\"}", allow_origin));
            return;
        }

        // Snapshot live settings once for this request.
        let (token, labels, default_threshold, rules) = {
            let s = self.state.read().unwrap();
            (s.token.clone(), s.labels.clone(), s.threshold, s.rules.clone())
        };

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
                return;
            }
        }

        let mut body = String::new();
        if request.as_reader().read_to_string(&mut body).is_err() {
            let _ = request.respond(json_response(400, "{\"error\":\"bad body\"}", allow_origin));
            return;
        }
        let req: ClassifyReq = match serde_json::from_str(&body) {
            Ok(r) => r,
            Err(e) => {
                let _ = request.respond(json_response(400, &format!("{{\"error\":\"bad json: {e}\"}}"), allow_origin));
                return;
            }
        };

        if req.text.len() > MAX_TEXT_BYTES {
            let _ = request.respond(json_response(413, "{\"error\":\"text too large\"}", allow_origin));
            return;
        }

        let threshold = req.threshold.unwrap_or(default_threshold);
        let tasks = vec![SchemaTask::Entities(labels)];

        // Scan each window, shifting per-window offsets back to global ones, then
        // pool and mask once over the original text so overlaps resolve uniformly.
        let wins = windows(&req.text);
        let parts = wins.len();
        let mut pooled: Vec<ExtractedEntity> = Vec::new();
        for (offset, window) in wins {
            let params = InferenceParams { threshold, flat_ner: true };
            match engine.extract(window, &tasks, Some(params)) {
                Ok((ents, _rel, _cls)) => {
                    for mut e in ents {
                        e.start_char += offset;
                        e.end_char += offset;
                        pooled.push(e);
                    }
                }
                Err(e) => {
                    let _ = request.respond(json_response(500, &format!("{{\"error\":\"infer: {e}\"}}"), allow_origin));
                    return;
                }
            }
        }

        // Resolve model spans against user regex matches. Rules take precedence,
        // but only over the bytes they cover — a partial overlap keeps the model
        // span's remainder rather than dropping it (see resolve_entities).
        let rule_ents = regex_entities(&req.text, &rules);
        let selected = resolve_entities(&req.text, pooled, rule_ents);
        let redacted = mask_pii_text(&req.text, &selected);
        let entities: Vec<Ent> = selected
            .into_iter()
            .map(|e| Ent { label: e.label, text: e.text, start: e.start_char, end: e.end_char, score: e.score })
            .collect();
        let resp_body = serde_json::to_string(&ClassifyResp { entities, redacted, parts }).unwrap();
        let _ = request.respond(json_response(200, &resp_body, allow_origin));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_span(label: &str, text: &str, start: usize, end: usize, score: f32) -> ExtractedEntity {
        ExtractedEntity {
            text: text.to_string(),
            label: label.to_string(),
            score,
            start_tok: 0,
            end_tok: 0,
            start_char: start,
            end_char: end,
        }
    }

    #[test]
    fn regex_matches_produce_byte_offset_spans() {
        let rules = vec![CompiledRule::new("Account", r"ACC-\d{4}").unwrap()];
        let text = "ref ACC-1234 end";
        let ents = regex_entities(text, &rules);
        assert_eq!(ents.len(), 1);
        let e = &ents[0];
        assert_eq!(e.label, "Account");
        assert_eq!(&text[e.start_char..e.end_char], "ACC-1234");
        assert_eq!(e.score, RULE_SCORE);
    }

    fn resolve(text: &str, model: Vec<ExtractedEntity>, rules: &[CompiledRule]) -> String {
        let rule_ents = regex_entities(text, rules);
        let selected = resolve_entities(text, model, rule_ents);
        mask_pii_text(text, &selected)
    }

    #[test]
    fn rule_fully_covering_model_span_wins() {
        // Rule and model span cover the same text; the rule label wins.
        let text = "id ACC-1234 x";
        let model = vec![model_span("id_num", "ACC-1234", 3, 11, 0.999)];
        let rules = vec![CompiledRule::new("Account", r"ACC-\d{4}").unwrap()];
        assert_eq!(resolve(text, model, &rules), "id [ACCOUNT] x");
    }

    #[test]
    fn partial_overlap_keeps_model_remainder() {
        // Regression for the leak where a rule on part of a longer model span
        // evicted the whole span. Rule "First" matches "John"; the model tagged
        // "John Smith" as a name. The surname must stay redacted.
        let text = "John Smith paid";
        let model = vec![model_span("name", "John Smith", 0, 10, 0.999)];
        let rules = vec![CompiledRule::new("First", r"John").unwrap()];
        assert_eq!(resolve(text, model, &rules), "[FIRST] [NAME] paid");
    }

    #[test]
    fn rule_inside_model_span_splits_into_two_residuals() {
        // Rule matches the middle of a model span -> mask both sides.
        let text = "aa MID bb";
        let model = vec![model_span("name", "aa MID bb", 0, 9, 0.9)];
        let rules = vec![CompiledRule::new("Mid", r"MID").unwrap()];
        assert_eq!(resolve(text, model, &rules), "[NAME] [MID] [NAME]");
    }

    #[test]
    fn non_overlapping_rule_and_model_spans_coexist() {
        let text = "ACC-1234 and bob@x.com";
        let model = vec![model_span("email", "bob@x.com", 13, 22, 0.99)];
        let rules = vec![CompiledRule::new("Account", r"ACC-\d{4}").unwrap()];
        assert_eq!(resolve(text, model, &rules), "[ACCOUNT] and [EMAIL]");
    }

    #[test]
    fn per_rule_match_cap_does_not_starve_later_rules() {
        // A broad rule that hits the per-rule cap must not stop a later rule from
        // matching. "x" appears far more than the cap; "KEEP" appears once after.
        let text = format!("{} KEEP", "x ".repeat(MAX_MATCHES_PER_RULE + 50));
        let rules = vec![
            CompiledRule::new("Broad", r"x").unwrap(),
            CompiledRule::new("Keep", r"KEEP").unwrap(),
        ];
        let ents = regex_entities(&text, &rules);
        let broad = ents.iter().filter(|e| e.label == "Broad").count();
        let keep = ents.iter().filter(|e| e.label == "Keep").count();
        assert_eq!(broad, MAX_MATCHES_PER_RULE); // capped
        assert_eq!(keep, 1); // still ran despite the earlier rule saturating
    }

    #[test]
    fn invalid_pattern_returns_error() {
        assert!(CompiledRule::new("bad", r"(unclosed").is_err());
    }

    #[test]
    fn empty_matches_are_skipped() {
        // `a*` matches the empty string at every position; none should be kept.
        let rules = vec![CompiledRule::new("star", r"a*").unwrap()];
        let ents = regex_entities("bbb", &rules);
        assert!(ents.is_empty());
    }

    #[test]
    fn compile_rules_splits_ok_and_errors_and_skips() {
        let rules = [
            ("Good", r"\d+", true),
            ("Disabled", r"\d+", false),   // skipped: disabled
            ("  ", r"\d+", true),          // skipped: blank name
            ("NoPattern", "", true),       // skipped: empty pattern
            ("Bad", r"(oops", true),       // error: won't compile
        ];
        let (ok, errs) = compile_rules(rules.iter().map(|&(n, p, e)| (n, p, e)));
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].label, "Good");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].name, "Bad");
    }
}
