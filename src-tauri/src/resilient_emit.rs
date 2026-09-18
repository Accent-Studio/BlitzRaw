//! Sending an event to every window, without letting one bad window silence
//! the rest.
//!
//! # Why this exists
//!
//! `AppHandle::emit` broadcasts, and inside it Tauri does:
//!
//! ```text
//! webviews.try_for_each(|webview| { ... webview.emit_js(emit_args, &ids)?; ... })
//! ```
//!
//! `try_for_each` stops at the first error. The windows are iterated in the
//! arbitrary order of a hash map, so a single window that fails to take an
//! event silently starves every window after it in that order.
//!
//! That is not theoretical. Opening a panel in a second window left that window
//! blank, and from then on the main window stopped receiving previews and
//! analytics: the events were being sent, and the broadcast was giving up
//! before it reached the window anybody was looking at. Nothing in the log said
//! so, because the caller was `let _ = app_handle.emit(...)`.
//!
//! # What this does instead
//!
//! Emits to each window separately, so a failure costs that window its event
//! and nothing else, and says which window failed rather than swallowing it.
//! Slightly more work per event, and the payloads here are already JSON going
//! across a process boundary, so it does not signify.

use tauri::{AppHandle, Emitter, Manager};

/// Emits to every window, one at a time. Returns how many took it.
///
/// Use for anything the application depends on receiving. A plain `emit` is
/// still fine for something nobody would miss.
pub fn emit_to_every_window<S>(app_handle: &AppHandle, event: &str, payload: S) -> usize
where
    S: serde::Serialize + Clone,
{
    let labels: Vec<String> = app_handle.webview_windows().keys().cloned().collect();
    let mut delivered = 0;

    for label in &labels {
        match app_handle.emit_to(label.as_str(), event, payload.clone()) {
            Ok(()) => delivered += 1,
            Err(e) => {
                // Named, because the whole point is knowing which window is the
                // problem rather than watching the others go quiet.
                log::warn!("Could not send '{event}' to window '{label}': {e}");
            }
        }
    }

    delivered
}

#[cfg(test)]
mod tests {
    /// The reason this module exists, written down so it is not undone.
    ///
    /// There is nothing to unit test here without a running app: the value is
    /// entirely in not using `emit`, and that is enforced by the call sites.
    /// This test guards the one thing that can rot, which is those call sites
    /// quietly going back to a broadcast.
    #[test]
    fn the_events_that_matter_do_not_use_a_plain_broadcast() {
        let lib = include_str!("lib.rs");

        for event in ["analytics-update"] {
            let broadcast = format!("app_handle.emit(\n                    \"{event}\"");
            assert!(
                !lib.contains(&broadcast),
                "'{event}' is back on a plain broadcast, where one bad window silences the rest"
            );
        }
    }
}
