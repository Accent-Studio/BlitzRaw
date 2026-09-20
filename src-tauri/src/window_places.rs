//! Where the windows sit on the desk, so a saved layout can put them back.
//!
//! A layout profile used to describe the inside of the windows and nothing
//! about the windows themselves: which panel is in which column, how wide the
//! columns are, which tab is showing. So two profiles made on two different
//! screen arrangements were byte for byte identical and loading either did
//! nothing, which read as the feature being broken when it was simply not the
//! feature. This is the part that was missing.
//!
//! Everything here is in **physical pixels of the visible window**, which is
//! what `panel_window::shown_place` reports and not what the window says about
//! itself. See the note there: since Windows 10 a window's reported rectangle
//! includes a transparent resize border that nobody can see, and placing by it
//! leaves a gap.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::panel_window::{
    FLOATING_LABEL, MAIN_LABEL, PanelWindowPlace, move_window_to, shown_place,
};

/// Both windows, as a layout profile records them.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct WindowPlaces {
    /// Where the photo is. `None` when it could not be read, which a profile
    /// stores as "this profile has nothing to say about the main window".
    pub main: Option<PanelWindowPlace>,
    /// Maximised is not a rectangle, so it travels beside one. A profile that
    /// records a maximised window restores it maximised on whichever screen the
    /// rectangle names, which is how a maximised window moves between screens.
    pub main_maximized: bool,
    /// Where the panels are, and `None` when the floating window is not open.
    pub panel: Option<PanelWindowPlace>,
}

/// Reads where both windows are now.
#[tauri::command]
pub fn get_window_places(app_handle: AppHandle) -> WindowPlaces {
    let main_window = app_handle.get_webview_window(MAIN_LABEL);

    let main_maximized = main_window
        .as_ref()
        .and_then(|window| window.is_maximized().ok())
        .unwrap_or(false);

    // A maximised window reports the whole screen, which is not where it would
    // go if it were restored, but it is the screen it is on and that is the
    // part a profile needs. The maximised flag carries the rest.
    let main = main_window.as_ref().and_then(shown_place);

    let panel = app_handle
        .get_webview_window(FLOATING_LABEL)
        .as_ref()
        .and_then(shown_place);

    WindowPlaces {
        main,
        main_maximized,
        panel,
    }
}

/// Puts both windows where a profile says they were.
///
/// **Async, and it has to stay that way.** Opening the floating window builds a
/// webview, and Tauri deadlocks on Windows if that is asked for from a
/// synchronous command: the main thread waits for the event loop and the event
/// loop waits for the main thread. That fault was reverted three times before
/// it was understood. See the note at the top of `panel_window.rs`.
#[tauri::command]
pub async fn apply_window_places(
    places: WindowPlaces,
    app_handle: AppHandle,
) -> Result<(), String> {
    if let Some(main_window) = app_handle.get_webview_window(MAIN_LABEL) {
        if let Some(place) = places.main {
            // Unmaximised first, because a maximised window ignores being moved
            // and would silently stay where it was.
            if main_window.is_maximized().unwrap_or(false) {
                let _ = main_window.unmaximize();
            }
            move_window_to(&main_window, place);

            if places.main_maximized {
                let _ = main_window.maximize();
            }
        } else if places.main_maximized && !main_window.is_maximized().unwrap_or(false) {
            let _ = main_window.maximize();
        }
    }

    match places.panel {
        Some(place) => {
            // Opening it if it is not open is the point: a profile that had the
            // panels out should bring them out.
            if app_handle.get_webview_window(FLOATING_LABEL).is_none() {
                crate::panel_window::open_floating_window(app_handle.clone()).await?;
            }
            if let Some(panel_window) = app_handle.get_webview_window(FLOATING_LABEL) {
                move_window_to(&panel_window, place);
            }
        }
        None => {
            // A profile saved with the panels docked closes the window, so that
            // loading it is a complete description rather than a partial one.
            if let Some(panel_window) = app_handle.get_webview_window(FLOATING_LABEL) {
                let _ = panel_window.close();
            }
        }
    }

    Ok(())
}

// ============ BLITZRAW: the panels go to the other screen ============
/// Whether two rectangles overlap at all.
///
/// Used to decide which monitor a window is "on", which is a question with no
/// exact answer for a window straddling two. Overlap with the larger area wins;
/// this is the test that feeds that.
// Nothing calls this today: the monitor question is answered by comparing
// overlap areas, which needs the area rather than the yes or no. Kept because
// it is the clearest statement of what "on this screen" means, and its test
// documents the straddling case.
#[allow(dead_code)]
pub fn overlaps(a: (i32, i32, u32, u32), b: (i32, i32, u32, u32)) -> bool {
    let (ax, ay, aw, ah) = a;
    let (bx, by, bw, bh) = b;
    ax < bx + bw as i32 && bx < ax + aw as i32 && ay < by + bh as i32 && by < ay + ah as i32
}

/// How much of `place` falls inside `area`, in pixels.
///
/// The window is on whichever monitor holds most of it. A window nudged five
/// pixels over a boundary has not moved screens, and counting the overlap is
/// what says so.
pub fn overlap_area(place: PanelWindowPlace, area: (i32, i32, u32, u32)) -> u64 {
    let (ax, ay, aw, ah) = area;
    let left = place.x.max(ax);
    let top = place.y.max(ay);
    let right = (place.x + place.width as i32).min(ax + aw as i32);
    let bottom = (place.y + place.height as i32).min(ay + ah as i32);

    if right <= left || bottom <= top {
        return 0;
    }
    (right - left) as u64 * (bottom - top) as u64
}

/// Which of `areas` a place mostly sits on, or `None` when it sits on none.
pub fn screen_holding(place: PanelWindowPlace, areas: &[(i32, i32, u32, u32)]) -> Option<usize> {
    let mut best: Option<(usize, u64)> = None;
    for (index, area) in areas.iter().enumerate() {
        let covered = overlap_area(place, *area);
        if covered == 0 {
            continue;
        }
        if best.is_none_or(|(_, most)| covered > most) {
            best = Some((index, covered));
        }
    }
    best.map(|(index, _)| index)
}

/// Which screen the panels should move to, given where the photo just went.
///
/// `None` means leave them alone, and that is the answer for almost everything:
/// one screen, the panels already elsewhere, or nothing having actually
/// changed. Moving a window the user is looking at is rude, so it happens only
/// when the two would otherwise be stacked on top of each other.
///
/// With more than two screens the panels go to the one they were on if the
/// photo has not taken it, and otherwise to the first free one. That keeps a
/// three screen desk from shuffling the panels around every time the photo
/// moves.
pub fn screen_for_panels(
    main_screen: Option<usize>,
    panel_screen: Option<usize>,
    screen_count: usize,
) -> Option<usize> {
    if screen_count < 2 {
        return None;
    }
    let main_screen = main_screen?;

    // Already apart, so there is nothing to solve.
    if panel_screen.is_some_and(|screen| screen != main_screen) {
        return None;
    }

    (0..screen_count).find(|screen| *screen != main_screen)
}
// ========== BLITZRAW END: the panels go to the other screen ==========

/// Every monitor's work area, in the order the system lists them.
fn every_work_area(window: &tauri::WebviewWindow) -> Vec<(i32, i32, u32, u32)> {
    window
        .available_monitors()
        .map(|monitors| {
            monitors
                .into_iter()
                .map(|monitor| {
                    let work = monitor.work_area();
                    (
                        work.position.x,
                        work.position.y,
                        work.size.width,
                        work.size.height,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Moves the panels off the screen the photo just arrived on.
///
/// Called when the main window has moved. Almost every call does nothing: it
/// needs two screens, an open floating window, and the two of them to have
/// ended up on the same one. Moving a window somebody is looking at is rude, so
/// it happens only when they would otherwise be stacked.
///
/// The panels keep their shape. They are placed against the far edge of their
/// new screen, the edge away from the photo, because that is where a column of
/// panels belongs and it is where they were put by hand every time.
pub fn keep_panels_off_the_photo(app_handle: &AppHandle) {
    let Some(main_window) = app_handle.get_webview_window(MAIN_LABEL) else {
        return;
    };
    let Some(panel_window) = app_handle.get_webview_window(FLOATING_LABEL) else {
        return;
    };

    let screens = every_work_area(&main_window);
    if screens.len() < 2 {
        return;
    }

    let (Some(main_place), Some(panel_place)) =
        (shown_place(&main_window), shown_place(&panel_window))
    else {
        return;
    };

    let main_screen = screen_holding(main_place, &screens);
    let panel_screen = screen_holding(panel_place, &screens);

    let Some(target) = screen_for_panels(main_screen, panel_screen, screens.len()) else {
        return;
    };
    let (area_x, area_y, area_width, area_height) = screens[target];

    // Against the edge furthest from the photo, keeping the size and the
    // distance from the top that the window already had.
    let width = panel_place.width.min(area_width);
    let height = panel_place.height.min(area_height);
    let against_the_left = main_screen.is_some_and(|screen| screens[screen].0 > area_x);

    let wanted = PanelWindowPlace {
        x: if against_the_left {
            area_x
        } else {
            area_x + area_width as i32 - width as i32
        },
        y: area_y,
        width,
        height,
    };

    log::info!(
        "The photo moved to screen {main_screen:?}, so the panels go to screen {target} at {},{} sized {}x{}",
        wanted.x,
        wanted.y,
        wanted.width,
        wanted.height
    );

    move_window_to(&panel_window, wanted);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(x: i32, y: i32, width: u32, height: u32) -> PanelWindowPlace {
        PanelWindowPlace {
            x,
            y,
            width,
            height,
        }
    }

    /// The desk this was written for: a 4K at 150% to the left of an ultrawide.
    const FOUR_K: (i32, i32, u32, u32) = (-3840, 0, 3840, 2097);
    const ULTRAWIDE: (i32, i32, u32, u32) = (0, 0, 3440, 1400);

    #[test]
    fn a_window_is_on_the_screen_it_sits_on() {
        let screens = [FOUR_K, ULTRAWIDE];
        assert_eq!(
            screen_holding(place(-3000, 100, 800, 600), &screens),
            Some(0)
        );
        assert_eq!(screen_holding(place(500, 100, 800, 600), &screens), Some(1));
    }

    #[test]
    fn a_window_straddling_two_belongs_to_the_one_holding_most_of_it() {
        let screens = [FOUR_K, ULTRAWIDE];
        // Ten pixels over the boundary is not a move to the next screen.
        assert_eq!(screen_holding(place(-810, 0, 820, 600), &screens), Some(0));
        // Most of the way across is.
        assert_eq!(screen_holding(place(-100, 0, 820, 600), &screens), Some(1));
    }

    #[test]
    fn a_window_on_no_screen_at_all_belongs_to_none() {
        assert_eq!(
            screen_holding(place(9000, 9000, 100, 100), &[FOUR_K, ULTRAWIDE]),
            None
        );
    }

    #[test]
    fn one_screen_means_the_panels_never_move() {
        assert_eq!(screen_for_panels(Some(0), Some(0), 1), None);
    }

    #[test]
    fn panels_stacked_on_the_photo_move_to_the_other_screen() {
        assert_eq!(screen_for_panels(Some(1), Some(1), 2), Some(0));
        assert_eq!(screen_for_panels(Some(0), Some(0), 2), Some(1));
    }

    #[test]
    fn panels_already_on_another_screen_are_left_alone() {
        assert_eq!(screen_for_panels(Some(0), Some(1), 2), None);
        assert_eq!(screen_for_panels(Some(1), Some(0), 2), None);
    }

    #[test]
    fn panels_nowhere_in_particular_are_given_a_screen() {
        // Off every screen, which happens after a monitor is unplugged.
        assert_eq!(screen_for_panels(Some(0), None, 2), Some(1));
    }

    #[test]
    fn nothing_happens_when_the_photo_is_nowhere() {
        assert_eq!(screen_for_panels(None, Some(0), 2), None);
    }

    #[test]
    fn three_screens_do_not_shuffle_the_panels_every_time() {
        // The panels are on screen 2 and the photo moves to screen 1. They are
        // already apart, so they stay.
        assert_eq!(screen_for_panels(Some(1), Some(2), 3), None);
        // The photo lands on top of them, so they take the first free screen.
        assert_eq!(screen_for_panels(Some(2), Some(2), 3), Some(0));
    }

    #[test]
    fn overlapping_is_not_touching() {
        assert!(overlaps(FOUR_K, (-100, 0, 200, 200)));
        // The ultrawide starts exactly where the 4K ends, and sharing an edge
        // is not overlapping.
        assert!(!overlaps(FOUR_K, ULTRAWIDE));
    }
}
