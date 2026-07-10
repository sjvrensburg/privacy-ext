# Reserving the Chrome Web Store extension ID (Option A)

The extension's whole zero-config pairing + CORS story is pinned to one Chrome
extension ID: `ihjamhkkcgbifajnbikldcjfamggnbaj`. It appears in three places:

- `extension-client/manifest.json` → `key` (derives the ID for unpacked loads)
- `server/src/lib.rs` → `DEFAULT_EXTENSION_ID` / `DEFAULT_EXTENSION_ORIGIN`
- the native-host manifest the desktop app writes (`allowed_origins`), now
  derived from `chrome_extension_ids` in the desktop config.

**The risk:** when you first upload to the Chrome Web Store (CWS), the *published*
extension may get a **different** ID than the one you pinned. If it does, the
store build can't pair (native host refuses it) and the daemon's CORS rejects it
— it's dead on arrival until you re-ship the **desktop app** with the new ID.

The goal of this walkthrough is to learn the real published ID **before** you
commit to a public release, so there are no surprises. Best case (you uploaded a
package that carries your `key`) the ID matches the pin and you change nothing.

> After the Option B refactor, a mismatch is no longer an emergency — you can add
> the real ID to the desktop `config.json` without a rebuild (see the end). But
> resolving it *before* launch is still the clean path.

---

## Steps

### 1. One-time: register a CWS developer account
- Go to the [Chrome Web Store Developer Dashboard](https://chrome.google.com/webstore/devconsole).
- Pay the **one-time $5** registration fee. Use the account you want to own the
  listing long-term (transferring later is painful).

### 2. Build the upload zip exactly as CI does
The `package-extensions` job in `.github/workflows/release.yml` produces the
canonical zip. To make one locally without a tag:

```sh
cd extension-client
zip -rX /tmp/pii-redactor-chrome.zip . -x ".keys/*" "*.pem" ".gitignore" "README.md"
```

The `.keys/` and `*.pem` exclusions matter — never upload the signing secret.
Note the zip **includes** the `key` field from `manifest.json`.

### 3. Create the item as a draft (do NOT publish)
- Dashboard → **Add new item** → upload `/tmp/pii-redactor-chrome.zip`.
- Fill in the minimum the dashboard demands (name, one screenshot placeholder,
  a description, and a privacy-policy URL — a stub is fine for a draft).
- **Save Draft. Do not submit for review yet.**

### 4. Read back the assigned ID
The item ID is the 32-character string in the dashboard URL for that item:

```
https://chrome.google.com/webstore/devconsole/<account>/<THIS-IS-THE-ID>/edit
```

It's also shown on the item's **Package** tab. Call it `PUBLISHED_ID`.

### 5. Compare and act

**Case A — `PUBLISHED_ID` == `ihjamhkkcgbifajnbikldcjfamggnbaj`:**
Nothing to do. Your pin is correct. Proceed to real listing assets and publish
when ready.

**Case B — `PUBLISHED_ID` differs:**
You now know the truth before launch. Re-pin to `PUBLISHED_ID` in the three
places. The `key` is the tricky one — you need the public key that Chrome
derives `PUBLISHED_ID` from:

1. Install the draft item (the dashboard lets you install an unpublished draft
   from its testers link), then find the injected `key` in the installed copy:
   - Linux: `~/.config/google-chrome/Default/Extensions/<PUBLISHED_ID>/<ver>/manifest.json`
     — the `"key"` field there is the one to copy.
2. Paste that `key` into `extension-client/manifest.json`.
3. Update `server/src/lib.rs`:
   ```rust
   pub const DEFAULT_EXTENSION_ID: &str = "<PUBLISHED_ID>";
   pub const DEFAULT_EXTENSION_ORIGIN: &str = "chrome-extension://<PUBLISHED_ID>";
   ```
4. Run `./scripts/sync-firefox.sh` (keeps the trees aligned) and rebuild.

Re-verify by reloading the unpacked `extension-client/` — its ID (shown on
`chrome://extensions`) must now equal `PUBLISHED_ID`.

### 6. Only then, build real listing assets and submit for review
Screenshots, promo copy, and a real privacy-policy URL. Expect extra review
scrutiny for `<all_urls>` + `nativeMessaging` — have the justification ready
("on-device PII redaction; the localhost host_permission talks only to the
user's own daemon; no data leaves the machine").

---

## Fallback: if a mismatch is discovered *after* the desktop app already shipped

Thanks to Option B you don't need a new installer. On the affected machine, edit
the desktop `config.json` (created next to the app's config dir) and add the
real ID:

```json
{ "chrome_extension_ids": ["ihjamhkkcgbifajnbikldcjfamggnbaj", "<PUBLISHED_ID>"] }
```

Restart the tray app. On launch it rewrites the native-host manifest's
`allowed_origins` and the daemon's CORS allow-list from this list, so both the
old pin and the new published build pair correctly. Then fold `<PUBLISHED_ID>`
into the source defaults for the next release so new installs get it for free.
