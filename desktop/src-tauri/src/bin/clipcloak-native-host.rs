// Chrome Native Messaging host: a tiny stdio process that Chrome launches on
// demand when the extension calls `chrome.runtime.sendNativeMessage`. Its only
// job is to hand back the daemon's port + bearer token from the tray app's
// persisted config, so the extension never needs a manually-typed URL/token.
//
// Protocol: each message is a 4-byte little-endian length prefix followed by
// that many bytes of UTF-8 JSON, on both stdin and stdout.
//
// The manifest that tells Chrome about this host (name, path, allowed_origins)
// is written by the tray app (see `native_host::install`) — this binary is
// never launched directly by a user.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::PathBuf;

#[derive(Deserialize)]
struct AppConfig {
    port: u16,
    token: String,
}

#[derive(Serialize)]
struct PairReply {
    ok: bool,
    port: Option<u16>,
    token: Option<String>,
    error: Option<String>,
}

// Matches the tray app's `app_config_dir()/config.json` (Tauri's default is
// the platform config dir joined with the app identifier).
fn config_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("ai.semplifica.clipcloak").join("config.json"))
}

fn read_message() -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    if let Err(e) = io::stdin().read_exact(&mut len_buf) {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            return Ok(None);
        }
        return Err(e);
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    io::stdin().read_exact(&mut buf)?;
    Ok(Some(buf))
}

fn write_message(reply: &PairReply) -> io::Result<()> {
    let body = serde_json::to_vec(reply).unwrap_or_default();
    let len = (body.len() as u32).to_le_bytes();
    let mut out = io::stdout();
    out.write_all(&len)?;
    out.write_all(&body)?;
    out.flush()
}

fn pair_reply() -> PairReply {
    let cfg = config_path().and_then(|p| std::fs::read_to_string(p).ok());
    match cfg.and_then(|s| serde_json::from_str::<AppConfig>(&s).ok()) {
        Some(cfg) => PairReply { ok: true, port: Some(cfg.port), token: Some(cfg.token), error: None },
        None => PairReply {
            ok: false,
            port: None,
            token: None,
            error: Some("ClipCloak isn't running — open the tray app once to pair.".to_string()),
        },
    }
}

fn main() {
    loop {
        match read_message() {
            Ok(Some(_msg)) => {
                if write_message(&pair_reply()).is_err() {
                    break;
                }
            }
            Ok(None) | Err(_) => break,
        }
    }
}
