// Tauri desktop front-end for the PII redaction daemon.
//
// Responsibilities:
//   * spawn the `pii_server` on a background thread (loads the model once),
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

use pii_server::{
    init_ort, load_engine, new_live_state, LiveSettings, LiveState, ModelSource, Server,
    ServerConfig, DEFAULT_LABELS, DEFAULT_PORT, DEFAULT_THRESHOLD,
};
use serde::{Deserialize, Serialize};
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, State, WindowEvent,
};
use tauri_plugin_autostart::ManagerExt;

/// Persisted user settings (written to <app_config_dir>/config.json).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct AppConfig {
    port: u16,
    /// Bearer token the extension must present. Empty = unauthenticated (dev).
    token: String,
    threshold: f32,
    /// Entity labels currently enabled for redaction.
    enabled_labels: Vec<String>,
    /// Launch on login.
    autostart: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            port: DEFAULT_PORT,
            token: String::new(),
            threshold: DEFAULT_THRESHOLD,
            enabled_labels: DEFAULT_LABELS.iter().map(|s| s.to_string()).collect(),
            autostart: false,
        }
    }
}

impl AppConfig {
    fn to_live(&self) -> LiveSettings {
        LiveSettings {
            token: if self.token.trim().is_empty() { None } else { Some(self.token.clone()) },
            labels: self.enabled_labels.clone(),
            threshold: self.threshold,
        }
    }
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

fn load_config(path: &PathBuf) -> AppConfig {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
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

fn start_server_thread(app: &AppHandle, shared: &Shared) {
    let model = resolve_model_source(app);
    let port = shared.bound_port;
    let live = shared.live.clone();
    let ready = shared.ready.clone();

    let config = ServerConfig { port, model, ..ServerConfig::default() };

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
fn save_config(app: AppHandle, state: State<Shared>, config: AppConfig) -> Result<ConfigView, String> {
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
        .invoke_handler(tauri::generate_handler![get_config, save_config, get_status])
        .setup(|app| {
            let handle = app.handle().clone();
            ensure_ort_dylib(&handle);

            let config_path = app
                .path()
                .app_config_dir()
                .map(|d| d.join("config.json"))
                .unwrap_or_else(|_| PathBuf::from("config.json"));
            let cfg = load_config(&config_path);

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

            start_server_thread(&handle, &shared);
            app.manage(shared);

            // Tray icon + menu.
            let open = MenuItemBuilder::with_id("open", "Open Settings").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
            let menu = MenuBuilder::new(app).items(&[&open, &quit]).build()?;
            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Privacy Redactor")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => show_settings(app),
                    "quit" => app.exit(0),
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
            // Keep running in the tray even with no visible windows.
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                api.prevent_exit();
            }
        });
}
