# ClipCloak — Firefox build

A Gecko (Firefox) MV3 port of `../extension-client`. All logic, styling and
icons are shared with the Chrome client and mirrored here by
`../scripts/sync-firefox.sh`; only `manifest.json` differs.

## What differs from the Chrome client

| | Chrome client | Firefox build |
|---|---|---|
| Extension identity | pinned `key` → fixed `chrome-extension://…` id | `browser_specific_settings.gecko.id` = `pii-redactor@semplifica.ai` |
| Background | `service_worker: background.js` | `background.scripts: [ai-sites.js, background.js]` (event page) |
| Native host auth | `allowed_origins` (extension origin) | `allowed_extensions` (extension id) |

`background.js` is byte-for-byte identical in both builds — it guards the
worker-only `importScripts`, so it runs both as a Chrome service worker and as a
Firefox background script.

## Load it (temporary, for development)

1. Run the ClipCloak tray app once so it installs the Firefox
   native-messaging host manifest (to `~/.mozilla/native-messaging-hosts/` on
   Linux, or the `HKCU\Software\Mozilla\NativeMessagingHosts` registry key on
   Windows).
2. Open `about:debugging#/runtime/this-firefox` → **Load Temporary Add-on…** →
   pick `extension-firefox/manifest.json`.

A temporary add-on keeps the pinned `gecko.id`, so native-messaging pairing
works. (Permanent installation requires a signed `.xpi` via addons.mozilla.org.)

## CORS note

The daemon pins CORS to the Chrome extension origin by default. Firefox's
`moz-extension://<uuid>` origin is randomised per install, so if you hit a CORS
error, start the daemon with:

```sh
PII_ALLOWED_ORIGINS='moz-extension://*' ./run.sh
```

The bearer token (exchanged over native messaging) remains the real access
control; the wildcard only relaxes the origin echo.

## Keeping in sync

After editing anything shared in `../extension-client` (except its manifest),
re-run:

```sh
../scripts/sync-firefox.sh
```
