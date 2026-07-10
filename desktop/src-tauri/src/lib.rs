// Tauri desktop front-end for the PII redaction daemon.
//
// Responsibilities:
//   * spawn the `clipcloak_server` on a background thread (loads the model once),
//   * expose a small tray icon (Open Settings / Quit) so it lives in the system
//     tray rather than a taskbar window,
//   * present a settings window (port, token, threshold, per-label toggles,
//     autostart) whose changes are persisted to a JSON config and pushed to the
//     running server's live state.
//
// Live vs. restart: token, threshold and label toggles apply immediately (they
// live behind the server's RwLock). Port changes are persisted but only take
// effect on the next launch — the model is expensive to reload, so we don't
// rebind on the fly; the GUI tells the user a restart is needed.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use clipcloak_server::{
    compile_rules, init_ort, load_engine, new_live_state, test_rules_preview, CompiledRule,
    LiveSettings, LiveState, ModelSource, RuleCompileError, RuleHit, Server, ServerConfig,
    DEFAULT_LABELS, DEFAULT_PORT, DEFAULT_THRESHOLD,
};
use serde::{Deserialize, Serialize};
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, State, WindowEvent,
};
use tauri_plugin_autostart::ManagerExt;

/// A user-defined regex redaction rule as persisted / edited in the GUI. `name`
/// doubles as the entity label and mask tag. Compiled to a `CompiledRule` before
/// it reaches the daemon (see `AppConfig::compiled_rules`).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct RuleConfig {
    name: String,
    pattern: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Persisted user settings (written to <app_config_dir>/config.json).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct AppConfig {
    port: u16,
    /// Bearer token the extension must present. Empty = unauthenticated (dev).
    token: String,
    threshold: f32,
    /// Entity labels currently enabled for redaction.
    enabled_labels: Vec<String>,
    /// User-defined regex rules, applied with precedence over the model. Empty by
    /// default; older configs without this key deserialize to an empty list.
    #[serde(default)]
    rules: Vec<RuleConfig>,
    /// Launch on login.
    autostart: bool,
    /// Chrome/Chromium/Edge extension ids authorised to pair via native
    /// messaging AND pass the daemon's CORS check. Defaults to the pinned Web
    /// Store id. If the Chrome Web Store assigns a different id at publish time,
    /// add it here (or replace the pin) — no rebuild needed, since both the
    /// native-host manifest and the server's allow-list are derived from this
    /// list. Firefox is authorised separately, by gecko id, in the native host.
    #[serde(default = "default_chrome_extension_ids")]
    chrome_extension_ids: Vec<String>,
}

fn default_chrome_extension_ids() -> Vec<String> {
    vec![clipcloak_server::DEFAULT_EXTENSION_ID.to_string()]
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            port: DEFAULT_PORT,
            token: String::new(),
            threshold: DEFAULT_THRESHOLD,
            enabled_labels: DEFAULT_LABELS.iter().map(|s| s.to_string()).collect(),
            rules: Vec::new(),
            autostart: false,
            chrome_extension_ids: default_chrome_extension_ids(),
        }
    }
}

impl AppConfig {
    /// The `chrome-extension://<id>` origins this config authorises (no trailing
    /// slash — matches the `Origin` header the daemon compares against).
    fn chrome_extension_origins(&self) -> Vec<String> {
        self.chrome_extension_ids
            .iter()
            .map(|id| clipcloak_server::chrome_extension_origin(id))
            .collect()
    }

    /// Compile the enabled rules for the daemon, skipping any that are disabled,
    /// blank, or fail to compile. `to_live` is infallible and runs at startup, so
    /// a bad pattern in a stored config must not abort the launch — `save_config`
    /// is where invalid patterns are rejected before they can be persisted.
    fn compiled_rules(&self) -> Vec<CompiledRule> {
        // Shared with the headless daemon and the test-filters preview via
        // `compile_rules` so the three can't drift; here we discard the errors
        // (save_config validates first, so a stored bad pattern is rare).
        compile_rules(self.rules.iter().map(|r| (r.name.as_str(), r.pattern.as_str(), r.enabled))).0
    }

    fn to_live(&self) -> LiveSettings {
        LiveSettings {
            token: if self.token.trim().is_empty() { None } else { Some(self.token.clone()) },
            labels: self.enabled_labels.clone(),
            threshold: self.threshold,
            rules: self.compiled_rules(),
        }
    }
}

/// Validate the enabled rules a Save is about to persist. Returns a readable
/// message naming the first offender, or `Ok(())` if every enabled rule has a
/// non-empty name and a pattern that compiles. Disabled rules are left alone so
/// a work-in-progress draft can still be saved.
fn validate_rules(rules: &[RuleConfig]) -> Result<(), String> {
    for r in rules {
        if !r.enabled {
            continue;
        }
        if r.name.trim().is_empty() {
            return Err("A rule is enabled but has no name.".to_string());
        }
        if r.pattern.is_empty() {
            return Err(format!("Rule \"{}\" is enabled but has no pattern.", r.name.trim()));
        }
        if let Err(e) = CompiledRule::new(r.name.trim(), &r.pattern) {
            return Err(format!("Rule \"{}\": {e}", r.name.trim()));
        }
    }
    Ok(())
}

/// What the settings window renders: current config + the full label catalogue
/// (so it can show a toggle for every label, not just the enabled ones) + status.
#[derive(Serialize)]
struct ConfigView {
    config: AppConfig,
    all_labels: Vec<String>,
    model_ready: bool,
    /// True when the persisted port differs from the port the server actually
    /// bound at startup — the GUI shows a "restart to apply" hint.
    restart_needed: bool,
}

/// Shared app state managed by Tauri and read by the server thread.
struct Shared {
    live: LiveState,
    /// Set true once the model has loaded and the server is listening.
    ready: Arc<AtomicBool>,
    /// Port the server actually bound at startup (immutable for this run).
    bound_port: u16,
    config_path: PathBuf,
    config: Mutex<AppConfig>,
}

/// A fresh 128-bit bearer token (32 hex chars) for extension ↔ daemon auth.
fn generate_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Name Chrome/Chromium look up in NativeMessagingHosts — must match
/// `NATIVE_HOST` in `extension-client/background.js` and the manifest's
/// `allowed_origins` must match `DEFAULT_EXTENSION_ORIGIN`.
const NATIVE_HOST_NAME: &str = "ai.semplifica.clipcloak";

/// Gecko extension id the Firefox build pins (see
/// `extension-firefox/manifest.json`). Firefox's native-messaging manifest
/// authorises callers by extension id (`allowed_extensions`) rather than by the
/// `chrome-extension://` origin Chrome uses.
const FIREFOX_EXTENSION_ID: &str = "pii-redactor@semplifica.ai";

/// Registers the `clipcloak-native-host` sidecar with the browser so the extension
/// can pair with zero manual config. Best-effort: a failure here just means
/// the extension falls back to showing "not paired" until the tray app has
/// run once with the sidecar present.
fn install_native_messaging_host(_app: &AppHandle, chrome_extension_ids: &[String]) {
    let host_binary_name = if cfg!(windows) { "clipcloak-native-host.exe" } else { "clipcloak-native-host" };
    let exe_dir = match std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())) {
        Some(d) => d,
        None => return,
    };
    let host_path = exe_dir.join(host_binary_name);
    if !host_path.exists() {
        eprintln!("Native messaging host not found at {}; pairing unavailable", host_path.display());
        return;
    }

    // Chrome/Chromium/Edge authorise the host by the extension's origin;
    // Firefox authorises it by the extension id. Otherwise the two manifests
    // are identical. Chrome requires an exact `chrome-extension://<id>/` (with
    // trailing slash) per entry — no wildcards — so we list every configured id.
    let allowed_origins: Vec<String> = chrome_extension_ids
        .iter()
        .map(|id| format!("chrome-extension://{id}/"))
        .collect();
    let manifest = serde_json::json!({
        "name": NATIVE_HOST_NAME,
        "description": "ClipCloak pairing host",
        "path": host_path.to_string_lossy(),
        "type": "stdio",
        "allowed_origins": allowed_origins,
    });
    let firefox_manifest = serde_json::json!({
        "name": NATIVE_HOST_NAME,
        "description": "ClipCloak pairing host",
        "path": host_path.to_string_lossy(),
        "type": "stdio",
        "allowed_extensions": [FIREFOX_EXTENSION_ID],
    });
    let manifest_str = match serde_json::to_string_pretty(&manifest) {
        Ok(s) => s,
        Err(_) => return,
    };
    let firefox_manifest_str = match serde_json::to_string_pretty(&firefox_manifest) {
        Ok(s) => s,
        Err(_) => return,
    };

    #[cfg(target_os = "linux")]
    {
        if let Some(config_dir) = dirs::config_dir() {
            for browser in ["google-chrome", "chromium"] {
                let dir = config_dir.join(browser).join("NativeMessagingHosts");
                if std::fs::create_dir_all(&dir).is_ok() {
                    let _ = std::fs::write(dir.join(format!("{NATIVE_HOST_NAME}.json")), &manifest_str);
                }
            }
        }
        // Firefox looks under ~/.mozilla, not the XDG config dir.
        if let Some(home) = dirs::home_dir() {
            let dir = home.join(".mozilla").join("native-messaging-hosts");
            if std::fs::create_dir_all(&dir).is_ok() {
                let _ = std::fs::write(dir.join(format!("{NATIVE_HOST_NAME}.json")), &firefox_manifest_str);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(config_dir) = _app.path().app_config_dir() {
            let _ = std::fs::create_dir_all(&config_dir);
            use winreg::enums::HKEY_CURRENT_USER;
            use winreg::RegKey;
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);

            let manifest_path = config_dir.join(format!("{NATIVE_HOST_NAME}.json"));
            if std::fs::write(&manifest_path, &manifest_str).is_ok() {
                for browser_key in [
                    format!("Software\\Google\\Chrome\\NativeMessagingHosts\\{NATIVE_HOST_NAME}"),
                    format!("Software\\Microsoft\\Edge\\NativeMessagingHosts\\{NATIVE_HOST_NAME}"),
                ] {
                    if let Ok((key, _)) = hkcu.create_subkey(&browser_key) {
                        let _ = key.set_value("", &manifest_path.to_string_lossy().to_string());
                    }
                }
            }

            // Firefox uses a separate manifest (allowed_extensions) and its own
            // registry hive.
            let firefox_path = config_dir.join(format!("{NATIVE_HOST_NAME}.firefox.json"));
            if std::fs::write(&firefox_path, &firefox_manifest_str).is_ok() {
                let key_path = format!("Software\\Mozilla\\NativeMessagingHosts\\{NATIVE_HOST_NAME}");
                if let Ok((key, _)) = hkcu.create_subkey(&key_path) {
                    let _ = key.set_value("", &firefox_path.to_string_lossy().to_string());
                }
            }
        }
    }
}

fn load_config(path: &PathBuf) -> AppConfig {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Load config, guaranteeing a non-empty token so the daemon is authenticated by
/// default (secure-by-default): a first run — or an older config that predates
/// tokens — gets a freshly generated one, persisted immediately. The user can
/// still clear it in the UI to run open, but that's now an explicit choice.
fn load_or_init_config(path: &PathBuf) -> AppConfig {
    let existed = path.exists();
    let mut cfg = load_config(path);
    let mut dirty = !existed;
    if cfg.token.trim().is_empty() {
        cfg.token = generate_token();
        dirty = true;
    }
    if dirty {
        let _ = save_config_file(path, &cfg);
    }
    cfg
}

fn save_config_file(path: &PathBuf, cfg: &AppConfig) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(cfg)?)?;
    Ok(())
}

/// Resolve the model directory: explicit env override, else a bundled
/// `semplifica/` resource next to the binary (set up by the installer). Returns
/// None to fall back to a HuggingFace download.
fn resolve_model_source(app: &AppHandle) -> ModelSource {
    if let Ok(dir) = std::env::var("PII_MODELS_DIR") {
        return ModelSource::LocalDir(dir);
    }
    if let Ok(res) = app.path().resource_dir() {
        let bundled = res.join("semplifica");
        if bundled.join("tokenizer.json").exists() {
            return ModelSource::LocalDir(bundled.to_string_lossy().into_owned());
        }
    }
    ServerConfig::default().model
}

/// If ORT_DYLIB_PATH isn't set, try a bundled libonnxruntime next to the binary.
fn ensure_ort_dylib(app: &AppHandle) {
    if std::env::var_os("ORT_DYLIB_PATH").is_some() {
        return;
    }
    if let Ok(res) = app.path().resource_dir() {
        for name in ["libonnxruntime.so", "onnxruntime.dll", "libonnxruntime.dylib"] {
            let candidate = res.join(name);
            if candidate.exists() {
                std::env::set_var("ORT_DYLIB_PATH", candidate);
                return;
            }
        }
    }
}

fn start_server_thread(app: &AppHandle, shared: &Shared, allowed_origins: Vec<String>) {
    let model = resolve_model_source(app);
    let port = shared.bound_port;
    let live = shared.live.clone();
    let ready = shared.ready.clone();

    let config = ServerConfig { port, model, allowed_origins };

    std::thread::spawn(move || {
        if let Err(e) = init_ort() {
            eprintln!("ORT init failed: {e}");
            return;
        }
        let engine = match load_engine(&config.model) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Model load failed: {e}");
                return;
            }
        };
        ready.store(true, Ordering::Relaxed);
        let server = Server::new(config, live);
        if let Err(e) = server.serve(engine) {
            eprintln!("Server stopped: {e}");
        }
    });
}

fn show_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

// ---- IPC commands (called from the settings window) ----

#[tauri::command]
fn get_config(state: State<Shared>) -> ConfigView {
    let cfg = state.config.lock().unwrap().clone();
    ConfigView {
        restart_needed: cfg.port != state.bound_port,
        config: cfg,
        all_labels: DEFAULT_LABELS.iter().map(|s| s.to_string()).collect(),
        model_ready: state.ready.load(Ordering::Relaxed),
    }
}

#[tauri::command]
fn save_config(app: AppHandle, state: State<Shared>, mut config: AppConfig) -> Result<ConfigView, String> {
    // Reject invalid regex rules before anything is applied or persisted, so a
    // bad pattern surfaces in the UI instead of being silently dropped at load.
    validate_rules(&config.rules)?;

    // `chrome_extension_ids` is a config-file-only escape hatch (no UI control),
    // so the settings window never sends it back. Preserve the stored value so a
    // Save can't silently reset an operator's manually-added Chrome id.
    config.chrome_extension_ids = state.config.lock().unwrap().chrome_extension_ids.clone();

    // Apply live-tunable settings immediately.
    {
        let mut live = state.live.write().unwrap();
        *live = config.to_live();
    }
    // Toggle autostart to match.
    let autolaunch = app.autolaunch();
    let _ = if config.autostart { autolaunch.enable() } else { autolaunch.disable() };

    // Persist.
    save_config_file(&state.config_path, &config).map_err(|e| e.to_string())?;
    *state.config.lock().unwrap() = config.clone();

    Ok(ConfigView {
        restart_needed: config.port != state.bound_port,
        config,
        all_labels: DEFAULT_LABELS.iter().map(|s| s.to_string()).collect(),
        model_ready: state.ready.load(Ordering::Relaxed),
    })
}

#[tauri::command]
fn get_status(state: State<Shared>) -> serde_json::Value {
    serde_json::json!({
        "model_ready": state.ready.load(Ordering::Relaxed),
        "port": state.bound_port,
    })
}

/// What the "test filters" panel gets back: the regex-only redaction of the
/// sample, the matched rule names, and any per-rule compile errors. Model
/// detection is intentionally excluded — this tests the rules.
#[derive(Serialize)]
struct RuleTestResult {
    redacted: String,
    matches: Vec<RuleHit>,
    errors: Vec<RuleCompileError>,
}

/// Compile the (possibly unsaved) rules from the editor and run them over the
/// sample text, without touching the model. Enabled rules that fail to compile
/// are reported in `errors`; the rest produce `matches` and the `redacted`
/// preview. Reuses the daemon's own compile/match/mask path (`compile_rules` +
/// `test_rules_preview`) so the preview cannot drift from live behaviour.
#[tauri::command]
fn test_rules(rules: Vec<RuleConfig>, sample: String) -> RuleTestResult {
    let (compiled, errors) =
        compile_rules(rules.iter().map(|r| (r.name.as_str(), r.pattern.as_str(), r.enabled)));
    let (matches, redacted) = test_rules_preview(&sample, &compiled);
    RuleTestResult { redacted, matches, errors }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // Second launch just surfaces the existing settings window.
            show_settings(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![get_config, save_config, get_status, test_rules])
        .setup(|app| {
            let handle = app.handle().clone();
            ensure_ort_dylib(&handle);

            let config_path = app
                .path()
                .app_config_dir()
                .map(|d| d.join("config.json"))
                .unwrap_or_else(|_| PathBuf::from("config.json"));
            let cfg = load_or_init_config(&config_path);

            // Register the native host AFTER loading config so its allow-list
            // reflects any operator-added Chrome ids (see chrome_extension_ids).
            install_native_messaging_host(&handle, &cfg.chrome_extension_ids);

            let shared = Shared {
                live: new_live_state(cfg.to_live()),
                ready: Arc::new(AtomicBool::new(false)),
                bound_port: cfg.port,
                config_path,
                config: Mutex::new(cfg.clone()),
            };

            // Sync autostart with the persisted preference on launch.
            let autolaunch = app.autolaunch();
            let _ = if cfg.autostart { autolaunch.enable() } else { autolaunch.disable() };

            start_server_thread(&handle, &shared, cfg.chrome_extension_origins());
            app.manage(shared);

            // Tray icon + menu.
            let open = MenuItemBuilder::with_id("open", "Open Settings").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
            let menu = MenuBuilder::new(app).items(&[&open, &quit]).build()?;
            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("ClipCloak")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => show_settings(app),
                    // Signal intent, then force-exit: the ExitRequested guard
                    // below otherwise keeps the app alive in the tray, and on
                    // some desktops app.exit() alone doesn't tear down cleanly.
                    "quit" => {
                        app.cleanup_before_exit();
                        std::process::exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_settings(tray.app_handle());
                    }
                })
                .build(app)?;

            // Closing the settings window hides it to the tray instead of quitting.
            if let Some(window) = app.get_webview_window("settings") {
                let w = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = w.hide();
                    }
                });
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error building tauri app")
        .run(|_app, event| {
            // Keep running in the tray when the last window is closed (code
            // == None), but honour an explicit app.exit() from the Quit menu
            // (code == Some(_)), otherwise the app can never be quit.
            if let tauri::RunEvent::ExitRequested { code, api, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}
