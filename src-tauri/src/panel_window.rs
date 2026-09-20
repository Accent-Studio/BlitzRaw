//! The floating panel window, for a second screen.
//!
//! # One window, holding a column
//!
//! It began as one window per panel, which is not what a second screen is for:
//! you want the scopes above the navigator above the metadata, in tabs and
//! rows, in one place you can drag where you like. So there is one window and
//! it holds a column arranged exactly the way a sidebar is. The front end does
//! that with the layout manager it already had, over two regions named
//! `floatTop` and `floatBottom`; this side only opens the window.
//!
//! Which panels may go in it is therefore a layout question and lives in the
//! front end, in `useDetachPanel.DETACHABLE`. Only ones that display: anything
//! that edits would have to send its changes back across the gap in order,
//! which is a different piece of work.
//!
//! # Why this is not just moving a panel
//!
//! A window is its own webview with its own JavaScript heap, so the second one
//! cannot see the first one's stores. Native applications do not have this
//! problem: Lightroom and Resolve are one process with one memory space and two
//! windows drawing from the same data. A web front end inherits the browser's
//! wall instead, and the way through it is the backend both windows talk to.
//!
//! Very little has to cross. The analytics worker sends `analytics-update` to
//! every window, so a floating scopes panel listens to the same event the
//! sidebar one does and gets the same numbers at the same time. What goes back
//! and forth otherwise is the arrangement of two regions and a list of scope
//! names.
//!
//! # Why three attempts showed a white rectangle
//!
//! **The window was built from a synchronous command.** Tauri runs one of those
//! on the main thread, and the main thread runs the event loop. Building a
//! webview asks the event loop to build it and waits for the answer, so on
//! Windows the two wait for each other and neither moves again. Tauri says so
//! in its own doc comment on `WebviewWindowBuilder::new`: "On Windows, this
//! function deadlocks when used in a synchronous command and event handlers.
//! You should use `async` commands and separate threads when creating windows."
//!
//! Every symptom is that one sentence:
//!
//! | Seen | Because |
//! |---|---|
//! | the new window is entirely white | WebView2 never finished starting it |
//! | previews stop, scopes stop, progress sticks | the event loop never runs again |
//! | the application cannot be closed | same |
//! | nothing at all in the log | `build()` never returned to the line that logs |
//!
//! That last row is why this took three tries. A hang that eats its own
//! evidence looks exactly like a logging fault, and it was taken for one twice.
//! The logger really could freeze the application, that really is fixed, and it
//! was never what this was.
//!
//! # Two other faults, found the same day, and neither was the cause
//!
//! Worth keeping straight, because finding a real bug is not the same as
//! finding the bug. Loading the floating route in an ordinary browser turned up
//! two genuine defects, both fixed in the front end, and both of which would
//! have produced a white window the moment the deadlock stopped hiding them:
//!
//! - **No theme.** Every colour resolves through an `--app-*` custom property
//!   and the stylesheet gives none of them a value; they are set by an effect
//!   only the main application runs. Measured: `--app-bg-primary` read back as
//!   the empty string. A window without them paints nothing, and an unpainted
//!   WebView2 is white.
//! - **The title bar rendered outside the error boundary,** so anything it
//!   threw unmounted the tree above the boundary and the boundary never ran.
//!
//! The main window going quiet had a third contributor of its own: `emit`
//! broadcasts with `try_for_each`, which gives up at the first window that will
//! not take the event, in the arbitrary order of a hash map, so one bad window
//! silenced the good one. Fixed in `resilient_emit`.
//!
//! # Saying which of those it is, next time
//!
//! A window that shows nothing tells you nothing, so four things are logged and
//! the gaps between them are the diagnosis:
//!
//! | Logged | Means |
//! |---|---|
//! | `Building panel-floating for ...` | the command ran |
//! | `Opened panel-floating ...` | the build returned, so nothing deadlocked |
//! | `panel-floating finished loading <url>` | the webview fetched and parsed the page |
//! | `The floating panel window rendered` | React committed a tree |
//!
//! The first three come from here and cannot be silenced by broken JavaScript.
//! Stopping after the first is the deadlock. Stopping after the second means
//! the page never arrived, which is a URL or a dev-server problem. Stopping
//! after the third means it arrived and threw. All four, and a blank window,
//! means it painted nothing, which is the theme.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

/// Where the floating window was last left, so it opens there again.
///
/// Kept in a small file of its own beside the main window's, rather than in the
/// settings the front end writes. A window's place on a desk is the backend's
/// to observe: it is the only side that sees the window move, and it can write
/// it as the window closes without a round trip to a webview that is going
/// away. It is also then independent of whether the front end's own saving is
/// working, which mattered while that was in doubt.
///
/// **Physical pixels, at both ends.** `outer_position` and `outer_size` report
/// physical, and the window builder's `position` and `inner_size` take
/// **logical**, so writing one and reading the other multiplied the whole thing
/// by the display's scale on the way back. A slim column docked to the side of
/// a 150% laptop screen reopened half as wide again and spilling onto the next
/// monitor. Restored through `set_position` and `set_size` now, which are
/// physical like the readings are.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PanelWindowPlace {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Where the window is right now, kept current as it is moved and resized.
///
/// It used to be read from the window at the moment it was asked to close,
/// which is the same mistake the main window's maximised state was making: a
/// window on its way out does not always still report where it was. Observed
/// while it is alive, written when it goes.
static LAST_PLACE: Mutex<Option<PanelWindowPlace>> = Mutex::new(None);

/// How near an edge counts as being docked to it.
///
/// Windows has no API for "snap this window to that edge": Aero Snap is a
/// gesture, not something an application can ask for. What it actually does is
/// set the window to the work area's own edges, and that is reproducible
/// directly. This is the distance at which a saved edge is taken to have meant
/// the screen edge.
///
/// Sixteen physical pixels. Wide enough to absorb a resize frame, which is
/// about twelve at 150%, and to pull back an edge that has drifted. Narrow
/// enough to leave a window deliberately set twenty pixels off the edge exactly
/// where it was put.
const EDGE_SNAP: i32 = 16;

/// Brings a remembered place onto a monitor that exists, and back onto the
/// edges it was docked to.
///
/// The same window state travels between a 4K screen at 150% and an ultrawide
/// at 100%, and a column that filled the tall one is taller than the wide one it
/// comes back to. Sized down to what the monitor has, then moved until it is
/// fully inside, so the window can always be grabbed by its own title bar.
///
/// **Edges first.** A window docked to the right of a screen is not "at x =
/// 2160", it is "against the right edge", and only the second of those survives
/// a couple of pixels of drift or a change of monitor. Any edge that was within
/// `EDGE_SNAP` of the work area's is put back exactly on it, which is what
/// snapping a window to a screen edge does and is the whole of what was wanted
/// from the native version.
///
/// Measured against the **work area** rather than the whole monitor, so a
/// docked window sits on the taskbar rather than under it.
pub fn fit_place_to_area(
    place: PanelWindowPlace,
    area_x: i32,
    area_y: i32,
    area_width: u32,
    area_height: u32,
) -> PanelWindowPlace {
    let area_right = area_x + area_width as i32;
    let area_bottom = area_y + area_height as i32;

    let near = |edge: i32, target: i32| (edge - target).abs() <= EDGE_SNAP;

    let mut left = place.x;
    let mut top = place.y;
    let mut right = place.x + place.width as i32;
    let mut bottom = place.y + place.height as i32;

    if near(left, area_x) {
        left = area_x;
    }
    if near(top, area_y) {
        top = area_y;
    }
    if near(right, area_right) {
        right = area_right;
    }
    if near(bottom, area_bottom) {
        bottom = area_bottom;
    }

    // A window is at least something, whatever the edges came out as.
    let width = (right - left).max(1) as u32;
    let height = (bottom - top).max(1) as u32;

    let width = width.min(area_width);
    let height = height.min(area_height);

    let max_x = area_x + area_width as i32 - width as i32;
    let max_y = area_y + area_height as i32 - height as i32;

    PanelWindowPlace {
        x: left.clamp(area_x.min(max_x), max_x.max(area_x)),
        y: top.clamp(area_y.min(max_y), max_y.max(area_y)),
        width,
        height,
    }
}

fn place_path(app_handle: &AppHandle) -> Option<PathBuf> {
    Some(crate::data_dir::data_path(
        app_handle,
        "panel_window_state.json",
    ))
}

fn remembered_place(app_handle: &AppHandle) -> Option<PanelWindowPlace> {
    let path = place_path(app_handle)?;
    let text = std::fs::read_to_string(path).ok()?;
    let place: PanelWindowPlace = serde_json::from_str(&text).ok()?;
    // A window smaller than this cannot be grabbed to resize, and one remembered
    // from a monitor that is no longer attached would open off the desk. The
    // size is checked here; the position is left to the window manager, which
    // pulls an off-screen window back on Windows already.
    if place.width < 200 || place.height < 160 {
        return None;
    }
    Some(place)
}

/// Notes where the window is, without writing anything.
///
/// Called on every move and resize, which is a great many times during a drag,
/// so it does no io. A minimised window reports a place that is not where the
/// user left it, so it is not noted.
fn note_place(window: &tauri::WebviewWindow) {
    if window.is_minimized().unwrap_or(false) {
        return;
    }
    let Some(place) = shown_place(window) else {
        return;
    };
    if place.width < 200 || place.height < 160 {
        return;
    }
    *LAST_PLACE.lock().unwrap() = Some(place);
}

fn remember_place(app_handle: &AppHandle, window: &tauri::WebviewWindow) {
    // What was noted while the window was alive, in preference to what it says
    // about itself now, which is while it is being taken apart.
    let noted = *LAST_PLACE.lock().unwrap();
    let place = match noted {
        Some(place) => place,
        None => match shown_place(window) {
            Some(place) if place.width >= 200 && place.height >= 160 => place,
            _ => return,
        },
    };
    let Some(path) = place_path(app_handle) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match serde_json::to_string(&place) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                log::warn!("Could not remember where the floating window was: {e}");
            }
        }
        Err(e) => log::warn!("Could not describe where the floating window was: {e}"),
    }
}

/// Every window this module opens is named with this prefix.
///
/// It has to match the `panel-*` pattern in `capabilities/default.json`. A
/// label outside that pattern gets no permissions at all, and the window opens
/// as a rectangle that cannot even close itself.
const LABEL_PREFIX: &str = "panel-";

/// The one floating window. There is deliberately only ever one.
pub const FLOATING_LABEL: &str = "panel-floating";

/// The window this one belongs to, as named in `tauri.conf.json`.
pub const MAIN_LABEL: &str = "main";

/// Set once the application is on its way out.
///
/// The window closing normally means the user put those panels back, and the
/// sidebar takes them. It closing because the application is closing means
/// nothing of the sort, and treating it the same way would rewrite the saved
/// layout on the way out: every panel filed back into the sidebar, and the next
/// start having forgotten that anything was ever floating. So the shutdown path
/// says so, and `panel-window-closed` is not sent.
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

// ============ BLITZRAW: the two windows travel together ============
/// Brings a window to the front of the z-order without taking the keyboard away
/// from whichever window the user is actually typing in.
///
/// `set_focus` is the obvious call and the wrong one: it activates the window,
/// so raising B because A was focused would immediately focus B, which raises A
/// because B was focused, and the two would trade the foreground back and
/// forth. `SWP_NOACTIVATE` is the difference between "come forward" and "take
/// over", and there is no Tauri call for it.
///
/// Does nothing it cannot do. A window that will not give up its handle is one
/// that is closing, and a raise is not worth an error either way.
#[cfg(windows)]
fn raise_without_taking_focus(window: &tauri::WebviewWindow) {
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
    };

    let Ok(hwnd) = window.hwnd() else {
        return;
    };

    // SAFETY: the handle is the window's own and this call runs while the
    // window is alive, on the thread that just heard from it. Every argument is
    // a constant, and a failed call is reported rather than acted on.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

#[cfg(not(windows))]
fn raise_without_taking_focus(_window: &tauri::WebviewWindow) {}

/// Brings the floating panel window forward, because the main window just came
/// forward and the two belong together.
///
/// Mostly redundant on Windows, where an owned window is already kept above its
/// owner. It is here for the case the owner relationship could not be set up,
/// and for platforms that have no such thing.
pub fn bring_panels_forward(app_handle: &AppHandle) {
    for (label, window) in app_handle.webview_windows() {
        if !is_panel_label(&label) {
            continue;
        }
        if window.is_minimized().unwrap_or(false) {
            let _ = window.unminimize();
        }
        raise_without_taking_focus(&window);
    }
}

/// Brings the main window forward, because a panel window just came forward.
///
/// The other half of the pairing, and the half the window manager does not do
/// for us: activating an owned window raises the owned window and leaves its
/// owner wherever it was, which on two screens means the panel comes back and
/// the photo does not.
fn bring_main_forward(app_handle: &AppHandle) {
    let Some(main_window) = app_handle.get_webview_window(MAIN_LABEL) else {
        return;
    };
    if main_window.is_minimized().unwrap_or(false) {
        let _ = main_window.unminimize();
    }
    raise_without_taking_focus(&main_window);
}
// ========== BLITZRAW END: the two windows travel together ==========

/// Whether a window label names one of ours. The main window is not a panel and
/// must never be swept up by anything that acts on all of them.
fn is_panel_label(label: &str) -> bool {
    label.starts_with(LABEL_PREFIX)
}

// ============ BLITZRAW: the window is smaller than it says it is ============
/// Where the window's **visible** edges are.
///
/// `outer_position` and `outer_size` report the window rectangle, and since
/// Windows 10 that rectangle is bigger than the window looks. DWM draws a
/// resize border **outside** the visible edge and leaves it transparent, so the
/// reported rectangle includes a margin nobody can see: about eight pixels at
/// the sides and eleven at the bottom on a 150% display, and only a couple at
/// the top.
///
/// That margin is why placing the window by its reported rectangle left a gap.
/// The measure-and-correct pass before this worked perfectly and converged on
/// exactly the wrong rectangle: it was hitting the number it was given, and the
/// number was of the wrong thing.
///
/// `DWMWA_EXTENDED_FRAME_BOUNDS` is the question "where does this window
/// actually appear", and it exists because this is a common thing to get wrong.
/// Asking is better than the offsets measured off a screenshot, which would be
/// right on one display and wrong on the next one at a different scale.
#[cfg(windows)]
fn visible_rect(window: &tauri::WebviewWindow) -> Option<PanelWindowPlace> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};

    let hwnd = window.hwnd().ok()?;
    let mut rect = RECT::default();

    // SAFETY: the handle is the window's own and this runs while it is alive.
    // The size passed is the size of the buffer written to, and a failure is
    // reported rather than acted on.
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut rect as *mut RECT).cast(),
            std::mem::size_of::<RECT>() as u32,
        )
        .ok()?;
    }

    if rect.right <= rect.left || rect.bottom <= rect.top {
        return None;
    }

    Some(PanelWindowPlace {
        x: rect.left,
        y: rect.top,
        width: (rect.right - rect.left) as u32,
        height: (rect.bottom - rect.top) as u32,
    })
}

#[cfg(not(windows))]
fn visible_rect(_window: &tauri::WebviewWindow) -> Option<PanelWindowPlace> {
    None
}

/// Where the window appears, falling back to what it says about itself.
///
/// Every part of remembering a place uses this and only this, so the rectangle
/// that is saved, the one the screen edges are measured against and the one the
/// correction pass aims at are all the same rectangle: the one with the visible
/// edges in it.
pub fn shown_place(window: &tauri::WebviewWindow) -> Option<PanelWindowPlace> {
    if let Some(shown) = visible_rect(window) {
        return Some(shown);
    }
    let (Ok(position), Ok(size)) = (window.outer_position(), window.outer_size()) else {
        return None;
    };
    Some(PanelWindowPlace {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    })
}
// ========== BLITZRAW END: the window is smaller than it says it is ==========

/// The next number to ask for, having asked for one and been given another.
///
/// A window does not always end up the size it was asked for. `set_size` moves
/// the **inner** size while what is remembered is the **outer** one, and the
/// difference is a resize frame whose width depends on the platform, the window
/// style and the display's scale. Guessing that difference was tried and was
/// wrong twice: subtracting nothing left the window ten pixels too big, and
/// subtracting `outer - inner` left it about that much too small.
///
/// So it is not guessed. Ask, look at what happened, and add the difference to
/// the next ask. One pass is enough for an offset that does not itself depend
/// on the size, which a window frame does not.
fn corrected(requested: i32, wanted: i32, actual: i32) -> i32 {
    requested + (wanted - actual)
}

/// Puts the window exactly on a remembered rectangle.
///
/// The rectangle is the **outer** one at both ends: it is what `note_place`
/// recorded and it is what the edges the user docked to were measured against.
/// Reproducing it exactly is the whole job, and it matters more than
/// understanding which of the two sizes each setter moves.
///
/// Two rounds at most, and it stops as soon as the window agrees. This runs once
/// as the window opens, before the webview has painted anything, so the extra
/// call costs nothing anybody can see.
pub fn place_exactly(window: &tauri::WebviewWindow, wanted: PanelWindowPlace) {
    let mut width = wanted.width as i32;
    let mut height = wanted.height as i32;

    for _ in 0..3 {
        let _ = window.set_size(tauri::PhysicalSize::new(
            width.max(1) as u32,
            height.max(1) as u32,
        ));
        let Some(actual) = shown_place(window) else {
            break;
        };
        if actual.width == wanted.width && actual.height == wanted.height {
            break;
        }
        width = corrected(width, wanted.width as i32, actual.width as i32);
        height = corrected(height, wanted.height as i32, actual.height as i32);
    }

    // After the size, because resizing a window can move it.
    let mut x = wanted.x;
    let mut y = wanted.y;

    for _ in 0..3 {
        let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
        let Some(actual) = shown_place(window) else {
            break;
        };
        if actual.x == wanted.x && actual.y == wanted.y {
            break;
        }
        x = corrected(x, wanted.x, actual.x);
        y = corrected(y, wanted.y, actual.y);
    }

    match shown_place(window) {
        Some(landed) => log::info!(
            "The floating window shows at {},{} sized {}x{}, wanted {},{} sized {}x{}",
            landed.x,
            landed.y,
            landed.width,
            landed.height,
            wanted.x,
            wanted.y,
            wanted.width,
            wanted.height
        ),
        None => log::warn!("The floating window would not say where it landed"),
    }
}

/// The work area of the monitor a place sits on, in physical pixels.
///
/// Asked of the window because that is what knows which monitors exist. Falls
/// back to the primary, and to nothing at all, in which case the caller uses
/// the place as it stands rather than inventing a screen.
pub fn work_area_for(window: &tauri::WebviewWindow) -> Option<(i32, i32, u32, u32)> {
    let monitor = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())?;
    let work = monitor.work_area();
    Some((
        work.position.x,
        work.position.y,
        work.size.width,
        work.size.height,
    ))
}

/// Moves a window onto a remembered place, fitted to the screen it lands on.
///
/// The rough move first is not redundant: `current_monitor` answers about where
/// the window is now, so it has to be near its destination before the question
/// is worth asking. Then the place is fitted to that monitor's work area and
/// applied exactly. See `fit_place_to_area` and `place_exactly`.
pub fn move_window_to(window: &tauri::WebviewWindow, place: PanelWindowPlace) {
    let _ = window.set_position(tauri::PhysicalPosition::new(place.x, place.y));

    let fitted = match work_area_for(window) {
        Some((x, y, width, height)) => fit_place_to_area(place, x, y, width, height),
        None => place,
    };

    place_exactly(window, fitted);
}

/// Closes every panel window, because the main window is going.
///
/// Tauri keeps the process alive while any window is open, so without this the
/// main window closes, the floating window stays, and the application is still
/// running with nothing to run it from. It belongs to the main window and has
/// nothing to show once that has gone.
pub fn close_every_panel_window(app_handle: &AppHandle) {
    SHUTTING_DOWN.store(true, Ordering::SeqCst);
    for (label, window) in app_handle.webview_windows() {
        if is_panel_label(&label) {
            log::info!("Closing {label} because the main window is closing");
            let _ = window.close();
        }
    }
}

/// Opens the floating panel window, or focuses it if it is already open.
///
/// # This has to be `async`, and that is the whole bug
///
/// Tauri runs a synchronous command on the main thread, and the main thread is
/// the one running the event loop. Building a webview asks the event loop to do
/// it and waits for the answer, so on Windows a window built from a synchronous
/// command waits for a thread that is waiting for it. Tauri says so in its own
/// doc comment on `WebviewWindowBuilder::new`:
///
/// > On Windows, this function deadlocks when used in a synchronous command and
/// > event handlers. You should use `async` commands and separate threads when
/// > creating windows.
///
/// That single word is the entire failure the first three attempts chased. An
/// `async` command is spawned on the runtime instead, so the event loop is free
/// to answer. Nothing here awaits, so the work is the same work; it just
/// happens somewhere that can afford to wait for the main thread.
#[tauri::command]
pub async fn open_floating_window(app_handle: AppHandle) -> Result<(), String> {
    if let Some(existing) = app_handle.get_webview_window(FLOATING_LABEL) {
        let _ = existing.unminimize();
        let _ = existing.set_focus();
        return Ok(());
    }

    let url = WebviewUrl::App("index.html?panel=floating".into());

    // Before the build, not after. The build is the part that can fail to
    // return at all, and a log line on the far side of it says nothing about
    // whether it was reached.
    log::info!("Building {FLOATING_LABEL} for {url:?}");

    // Where it was left last time, if that is still a sensible place.
    let place = remembered_place(&app_handle);
    // Only Windows reassigns this, to give the panel window an owner. Everywhere
    // else the builder is used exactly as it is made, so the `mut` reads as
    // unused and the build fails on warnings.
    #[allow(unused_mut)]
    let mut builder = WebviewWindowBuilder::new(&app_handle, FLOATING_LABEL, url)
        .title("BlitzRaw Panels")
        .inner_size(420.0, 760.0)
        .min_inner_size(260.0, 220.0);

    // ============ BLITZRAW: the two windows travel together ============
    // Owned, in the Win32 sense, rather than paired by hand. From MSDN:
    //
    // - an owned window is always above its owner in the z-order
    // - an owned window is hidden when its owner is minimised
    // - the system destroys an owned window when its owner is destroyed
    //
    // Which is the whole of "minimise together, come forward together, close
    // together", done by the window manager rather than by us watching events
    // and guessing. Nothing here polls and nothing can drift.
    //
    // It also means the panel window loses its own taskbar button, which is
    // correct: it is not a second application, and it has nothing to show
    // without the window it belongs to.
    //
    // `owner_raw` rather than `owner`: the latter consumes the builder and hands
    // it back only when it succeeds, so a failure would leave nothing to build
    // the window from. This one cannot fail, and a window worth having without
    // an owner is worth opening without one.
    #[cfg(windows)]
    {
        match app_handle
            .get_webview_window(MAIN_LABEL)
            .and_then(|main_window| main_window.hwnd().ok())
        {
            Some(owner) => {
                builder = builder.owner_raw(owner);
                log::info!("{FLOATING_LABEL} will be owned by {MAIN_LABEL}");
            }
            None => {
                log::warn!(
                    "No {MAIN_LABEL} window to own {FLOATING_LABEL}; it will float on its own"
                );
            }
        }
    }
    // ========== BLITZRAW END: the two windows travel together ==========

    // BLITZRAW: deliberately not through the builder. `position` and
    // `inner_size` there are **logical** pixels, and what was saved is physical,
    // so on any display that is not at 100% the window came back scaled. Applied
    // after the build instead, through the physical setters, and fitted to the
    // monitor it lands on. See PanelWindowPlace.
    if let Some(place) = place {
        log::info!(
            "Reopening the floating window at {},{} sized {}x{} (physical)",
            place.x,
            place.y,
            place.width,
            place.height
        );
    }
    let window = builder
        // Undecorated to match the main window, which draws its own title bar.
        // TitleBar.tsx asks for the current window rather than naming main, so
        // it controls whichever one it is in and needed no changes.
        //
        // Not transparent, unlike the main window, which needs it for its
        // rounded corners. Nothing here does, and an opaque window is one fewer
        // thing between a fault and seeing it.
        .decorations(false)
        // The instrument the first attempts did not have. A blank window is the
        // same rectangle whether the page never arrived, arrived and threw, or
        // arrived and painted nothing, and the front end can only report the
        // last two. This is on the backend and no amount of broken JavaScript
        // can silence it.
        .on_page_load(move |window, payload| match payload.event() {
            PageLoadEvent::Started => {
                log::info!("{} started loading {}", window.label(), payload.url());
            }
            PageLoadEvent::Finished => {
                log::info!("{} finished loading {}", window.label(), payload.url());
            }
        })
        .build()
        .map_err(|e| format!("Could not open the floating panel window: {e}"))?;

    // The main window is told when this one goes, so the panels can go back to
    // the sidebar they left. Sent on close rather than on destroy: destroy is
    // too late for the webview that has to hear about it.
    let closing = app_handle.clone();
    let closing_window = window.clone();
    window.on_window_event(move |event| {
        // BLITZRAW: clicking the panels on the second screen brings the photo
        // on the first screen forward with them. Without focus, or the two
        // would trade the foreground back and forth forever.
        if matches!(event, WindowEvent::Focused(true)) {
            bring_main_forward(&closing);
        }
        // BLITZRAW: noted while the window is alive rather than read as it is
        // being destroyed. Costs nothing: it writes to memory, not to disk.
        if matches!(event, WindowEvent::Moved(_) | WindowEvent::Resized(_)) {
            note_place(&closing_window);
        }
        if matches!(event, WindowEvent::CloseRequested { .. }) {
            // On close rather than on every move: a window being dragged sends
            // a great many of these, and one write when it settles is enough.
            // Read from the window itself, which is still there at this point
            // and knows where it is better than anything tracking it would.
            remember_place(&closing, &closing_window);
        }
        if matches!(
            event,
            WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed
        ) {
            if SHUTTING_DOWN.load(Ordering::SeqCst) {
                // The application is closing, not the window. Saying otherwise
                // here would dismantle the arrangement and save that, so the
                // next start would forget what it is supposed to remember.
                return;
            }
            log::info!("The floating panel window is closing");
            crate::resilient_emit::emit_to_every_window(
                &closing,
                "panel-window-closed",
                serde_json::json!({}),
            );
        }
    });

    // BLITZRAW: the place, in the units it was written in.
    if let Some(place) = place {
        // A rough move first, only so that `current_monitor` answers with the
        // screen this window belongs on rather than whichever is first.
        let _ = window.set_position(tauri::PhysicalPosition::new(place.x, place.y));

        // Asked after the move, so it is the monitor the window is actually on
        // rather than whichever one happens to be first.
        let area = window
            .current_monitor()
            .ok()
            .flatten()
            .or_else(|| window.primary_monitor().ok().flatten())
            .map(|monitor| {
                let work = monitor.work_area();
                (
                    work.position.x,
                    work.position.y,
                    work.size.width,
                    work.size.height,
                )
            });

        let fitted = match area {
            Some((x, y, w, h)) => fit_place_to_area(place, x, y, w, h),
            None => place,
        };
        if fitted != place {
            log::info!(
                "Fitted the floating window to its monitor: {},{} sized {}x{}",
                fitted.x,
                fitted.y,
                fitted.width,
                fitted.height
            );
            let _ = window.set_position(tauri::PhysicalPosition::new(fitted.x, fitted.y));
        }
        // Asked, measured, corrected. See `place_exactly`.
        place_exactly(&window, fitted);

        *LAST_PLACE.lock().unwrap() = Some(fitted);
    }

    log::info!("Opened {FLOATING_LABEL} in a window of its own");
    Ok(())
}

/// Closes the floating window, if it is open. Closing one that is not open is
/// not an error: the user may have closed it already.
#[tauri::command]
pub fn close_floating_window(app_handle: AppHandle) -> Result<(), String> {
    if let Some(window) = app_handle.get_webview_window(FLOATING_LABEL) {
        window.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Said by the floating window once its page has actually rendered.
///
/// Two things at once. It puts a line in the log, which is what tells a window
/// that failed to load from one that loaded and then broke, since both look
/// like a rectangle. And it tells the main window to send the arrangement that
/// should be showing, which is what makes a window opened on startup fill
/// itself without either side having to guess.
#[tauri::command]
pub fn panel_window_ready(panel: String, app_handle: AppHandle) {
    log::info!("The floating panel window rendered ({panel})");
    crate::resilient_emit::emit_to_every_window(
        &app_handle,
        "panel-window-rendered",
        serde_json::json!({ "panel": panel }),
    );
}

/// Whether the floating window is open. Asked on startup, so the front end can
/// tell a saved arrangement that needs a window from one that already has it.
#[tauri::command]
pub fn floating_window_is_open(app_handle: AppHandle) -> bool {
    app_handle.get_webview_window(FLOATING_LABEL).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_a_window_has_to_stay_async() {
        // This does not compile if `open_floating_window` goes back to being a
        // synchronous command, and that word is the whole of the failure that
        // was reverted three times. Tauri runs a synchronous command on the
        // main thread; building a webview asks the main thread to do it and
        // waits; so on Windows the two wait for each other. The half-built
        // window is white, the application stops rendering and will not close,
        // and nothing is logged because `build()` never returns to the line
        // that would log it.
        //
        // The future is only constructed here, never polled. Constructing it
        // does nothing at all, which is exactly what makes this a compile-time
        // check and not a window opening during `cargo test`.
        fn only_takes_a_future<F: std::future::Future<Output = Result<(), String>>>(
            _: fn(AppHandle) -> F,
        ) {
        }
        only_takes_a_future(open_floating_window);
    }

    #[test]
    fn the_label_is_what_the_capability_file_matches() {
        // capabilities/default.json lists "panel-*". A label outside that
        // pattern gets no permissions at all, and the window opens as a
        // rectangle that cannot even close itself.
        assert!(
            FLOATING_LABEL.starts_with(LABEL_PREFIX),
            "the label has to match the capability pattern"
        );

        let capability = include_str!("../capabilities/default.json");
        assert!(
            capability.contains("panel-*"),
            "the capability file has to cover the windows this opens"
        );
        assert!(
            capability.contains("core:webview:allow-create-webview-window"),
            "creating a window is refused without this permission"
        );
    }

    // ====== BLITZRAW: the two windows travel together ======
    #[test]
    fn the_main_window_is_never_treated_as_a_panel() {
        // Everything that acts on "all the panel windows" closes them, and the
        // main window is in the same map. Getting this wrong would close the
        // application every time it came forward.
        assert!(is_panel_label(FLOATING_LABEL));
        assert!(!is_panel_label(MAIN_LABEL));
        assert!(!is_panel_label(""));
        assert!(!is_panel_label("mainpanel-"));
    }
    // ==== BLITZRAW END: the two windows travel together ====

    // ====== BLITZRAW: the floating window comes back where it was ======
    fn place(x: i32, y: i32, width: u32, height: u32) -> PanelWindowPlace {
        PanelWindowPlace {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn a_window_already_on_the_monitor_is_left_alone() {
        // A slim column docked to the right of a 2560 wide screen.
        let docked = place(2160, 0, 400, 1400);
        assert_eq!(fit_place_to_area(docked, 0, 0, 2560, 1400), docked);
    }

    #[test]
    fn a_window_taller_than_the_monitor_is_cut_down_to_it() {
        // A place saved on the 4K screen, reopened somewhere 1400 tall. 2097
        // does not fit, so the height gives way; the right edge it was docked
        // to does not.
        let from_the_tall_screen = place(-781, 0, 790, 2097);
        let fitted = fit_place_to_area(from_the_tall_screen, -2560, 0, 2560, 1400);

        assert_eq!(fitted.height, 1400);
        assert_eq!(
            fitted.x + fitted.width as i32,
            0,
            "still docked right: {fitted:?}"
        );
        assert!(fitted.y >= 0, "top edge above the work area: {fitted:?}");
    }

    #[test]
    fn a_window_hanging_off_the_right_is_pulled_back_on() {
        let hanging = place(2400, 0, 800, 600);
        let fitted = fit_place_to_area(hanging, 0, 0, 2560, 1400);

        assert_eq!(fitted.x, 2560 - 800);
        assert_eq!(fitted.width, 800);
    }

    #[test]
    fn a_window_on_a_monitor_that_starts_at_a_negative_x_stays_on_it() {
        // The second screen sits to the left of the primary, so its own
        // coordinates are negative and clamping to zero would be wrong.
        let on_the_left_screen = place(-3900, 40, 600, 800);
        let fitted = fit_place_to_area(on_the_left_screen, -3840, 0, 2560, 1400);

        assert_eq!(fitted.x, -3840);
        // Forty is further than the snap reaches, so it is left where it was
        // put rather than pulled to the top.
        assert_eq!(fitted.y, 40);
    }

    /// The reported symptom, in the numbers that were actually on disk.
    ///
    /// The 4K screen is 3840x2160 at 150%, sitting to the left of the primary,
    /// so its work area is `-3840,0` by `3840x2097` once the taskbar is taken
    /// off. `panel_window_state.json` held `-781,0` by `790x2097`, whose right
    /// edge is at **+9**: nine pixels onto the next monitor, which is the
    /// "roughly 10px" that was reported.
    #[test]
    fn the_reported_drift_is_put_back_on_the_edges() {
        let drifted = place(-781, 4, 790, 2104);
        let fitted = fit_place_to_area(drifted, -3840, 0, 3840, 2097);

        assert_eq!(fitted.x + fitted.width as i32, 0, "right edge: {fitted:?}");
        assert_eq!(fitted.y, 0, "top edge: {fitted:?}");
        assert_eq!(
            fitted.y + fitted.height as i32,
            2097,
            "bottom edge: {fitted:?}"
        );
        assert_eq!(
            fitted.x, -781,
            "the left edge is where it was put: {fitted:?}"
        );
    }

    #[test]
    fn snapping_one_edge_does_not_drag_the_other_with_it() {
        // A slim column docked right. Correcting the right edge moves that edge
        // and only that edge, so the width changes by the drift and the left
        // side stays exactly where the user left it.
        let docked_right = place(-810, 0, 800, 2097);
        let fitted = fit_place_to_area(docked_right, -3840, 0, 3840, 2097);

        assert_eq!(fitted.x, -810, "the left edge did not move");
        assert_eq!(
            fitted.width, 810,
            "and the width grew by the ten it was out"
        );
    }

    #[test]
    fn a_correction_closes_the_gap_it_was_given() {
        // Asked for 790, got 780: ten short, so ask for ten more.
        assert_eq!(corrected(790, 790, 780), 800);
        // Asked for 790, got 800: ten over, so ask for ten less.
        assert_eq!(corrected(790, 790, 800), 780);
        // Landed on it, so ask for the same again and the loop stops.
        assert_eq!(corrected(790, 790, 790), 790);
    }

    #[test]
    fn a_correction_applied_twice_lands_exactly() {
        // What the second round does with the first round's answer, for a
        // frame that adds a constant ten pixels however big the window is.
        let frame = 10;
        let wanted = 790;

        let first_request = wanted;
        let first_actual = first_request + frame;
        assert_ne!(
            first_actual, wanted,
            "the frame is what makes this necessary"
        );

        let second_request = corrected(first_request, wanted, first_actual);
        let second_actual = second_request + frame;
        assert_eq!(second_actual, wanted, "the second ask lands on it");
    }

    #[test]
    fn a_window_in_the_middle_is_not_dragged_to_any_edge() {
        let floating = place(-2000, 400, 900, 700);
        assert_eq!(fit_place_to_area(floating, -3840, 0, 3840, 2097), floating);
    }

    #[test]
    fn a_window_larger_than_the_monitor_in_both_directions_fills_it() {
        let huge = place(-500, -500, 9000, 9000);
        let fitted = fit_place_to_area(huge, 0, 0, 1920, 1080);

        assert_eq!(fitted, place(0, 0, 1920, 1080));
    }

    #[test]
    fn the_work_area_is_what_is_fitted_to_not_the_whole_screen() {
        // A taskbar 40px tall at the bottom leaves 1400 of a 1440 screen. A
        // full height window has to end up above it, not under it.
        let full_height = place(0, 0, 500, 1440);
        let fitted = fit_place_to_area(full_height, 0, 0, 2560, 1400);

        assert_eq!(fitted.height, 1400);
        assert_eq!(fitted.y, 0);
    }
    // ==== BLITZRAW END: the floating window comes back where it was ====

    #[test]
    fn closing_the_application_is_not_the_user_putting_panels_back() {
        // The two are the same window event and mean opposite things. Getting
        // this wrong is silent and only shows up as an arrangement quietly
        // forgotten between one session and the next, so it is worth a check
        // that the shutdown path is what sets the flag.
        assert!(
            !SHUTTING_DOWN.load(Ordering::SeqCst),
            "nothing should have set this before a shutdown"
        );
    }
}
