use crate::{
  commands::window::{unmanage_window, FocusAfterUnmanage},
  models::WindowContainer,
  traits::WindowGetters,
  wm_state::WmState,
};

/// Stops managing a window that the WM isn't allowed to control, and keeps
/// it ignored for the rest of the session.
///
/// Positioning a window fails with an access denied error when it belongs
/// to a process with higher privileges than the WM's (e.g. Task Manager
/// while the WM isn't elevated). Such a window would otherwise keep its
/// tile forever without ever being moved into it, leaving a gap in the
/// layout that can't be filled.
///
/// Such a window stays on screen and often has just been opened and
/// focused (e.g. the "Windows Security" credential dialog of Remote
/// Desktop), so focus is left untouched. Moving focus elsewhere would
/// also raise the tiling windows of the focused workspace above it,
/// leaving it buried behind them.
#[allow(clippy::needless_pass_by_value)]
pub fn ignore_uncontrollable_window(
  window: WindowContainer,
  state: &mut WmState,
) -> anyhow::Result<()> {
  state.ignored_windows.push(window.native().clone());

  // The window may be hidden if it's on a non-displayed workspace.
  #[cfg(target_os = "windows")]
  crate::window_recovery::show_released_window(&window.native());

  unmanage_window(window, FocusAfterUnmanage::Keep, state)
}
