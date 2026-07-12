//! The always-on-top suggestion overlay ("PiP") window.
//!
//! A second webview, frameless and transparent, that floats over the game
//! client and renders the bot's top-N suggestions. It needs no backend
//! plumbing of its own: `bot-response` is already `app.emit()`-ed, which
//! broadcasts to *every* webview, so the overlay just listens for it (see
//! `frontend/src/routes/Overlay.tsx`).
//!
//! Both windows load the same `index.html`; the frontend branches on
//! `getCurrentWindow().label` to decide which root to render. That keeps the
//! window identity in one place (this module's [`LABEL`]) instead of encoding
//! it in a URL that the router would then have to parse back out.
//!
//! Lifecycle is driven entirely by `config.overlay.enabled`:
//!
//! - at startup, `lib::run` calls [`reconcile`] once;
//! - on every `update_config`, the command calls [`reconcile`] again;
//! - the overlay's own close button flips `enabled` to false, which routes
//!   back through the same path.

use crate::config::OverlayConfig;
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_window_state::{StateFlags, WindowExt};
use tracing::{info, warn};

/// Window label. Also the label the `capabilities/overlay.json` capability is
/// scoped to — renaming this without renaming that leaves the overlay webview
/// with no permission to `listen()`, i.e. permanently blank.
pub const LABEL: &str = "overlay";

/// Event carrying a fresh [`OverlayConfig`] to the overlay webview, so
/// top-N / opacity edits in Settings apply without a reopen.
pub const CONFIG_EVENT: &str = "overlay-config";

/// Position and size are the only state worth restoring. The plugin's default
/// (`StateFlags::all()`) would also restore DECORATIONS — putting a title bar
/// back onto a window that is deliberately frameless — so `lib::run` excludes
/// this label from the automatic restore and we do it ourselves.
const RESTORE_FLAGS: StateFlags = StateFlags::POSITION.union(StateFlags::SIZE);

const DEFAULT_WIDTH: f64 = 300.0;
const DEFAULT_HEIGHT: f64 = 210.0;
const MIN_WIDTH: f64 = 190.0;
const MIN_HEIGHT: f64 = 90.0;

pub fn get<R: Runtime>(app: &AppHandle<R>) -> Option<WebviewWindow<R>> {
    app.get_webview_window(LABEL)
}

/// Open the overlay, or re-apply `always_on_top` to the one already open.
pub fn open<R: Runtime>(app: &AppHandle<R>, cfg: &OverlayConfig) -> tauri::Result<()> {
    if let Some(w) = get(app) {
        w.set_always_on_top(cfg.always_on_top)?;
        w.show()?;
        return Ok(());
    }

    let w = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
        .title("Akagi Overlay")
        .inner_size(DEFAULT_WIDTH, DEFAULT_HEIGHT)
        .min_inner_size(MIN_WIDTH, MIN_HEIGHT)
        .decorations(false)
        .transparent(true)
        .always_on_top(cfg.always_on_top)
        .skip_taskbar(true)
        .maximizable(false)
        .minimizable(false)
        .shadow(false)
        // Never steal focus from the game client — the whole point of the
        // overlay is that you don't have to look away, let alone click away.
        .focused(false)
        .build()?;

    // Undecorated windows are still resizable from their edges on Windows and
    // macOS, so no in-page resize grip is needed.
    if let Err(e) = w.restore_state(RESTORE_FLAGS) {
        warn!("overlay: could not restore saved geometry: {e}");
    }

    info!("overlay window opened");
    Ok(())
}

pub fn close<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    if let Some(w) = get(app) {
        w.close()?;
        info!("overlay window closed");
    }
    Ok(())
}

/// Bring the live window in line with `cfg`: open it, close it, or leave it
/// alone. Idempotent — safe to call on every config save.
pub fn reconcile<R: Runtime>(app: &AppHandle<R>, cfg: &OverlayConfig) {
    let result = if cfg.enabled {
        open(app, cfg)
    } else {
        close(app)
    };
    if let Err(e) = result {
        warn!("overlay: reconcile failed: {e}");
        return;
    }
    // Push the (possibly edited) top-N / opacity to a window that is staying
    // open. Emitting to a closed label is a no-op, so this needs no guard.
    if let Err(e) = app.emit_to(LABEL, CONFIG_EVENT, cfg) {
        warn!("overlay: could not emit {CONFIG_EVENT}: {e}");
    }
}
