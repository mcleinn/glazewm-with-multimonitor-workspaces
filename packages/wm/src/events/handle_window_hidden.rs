use tracing::info;
use wm_common::{DisplayState, HideMethod};
use wm_platform::NativeWindow;

use crate::{
  commands::window::unmanage_window, traits::WindowGetters,
  user_config::UserConfig, wm_state::WmState,
};

pub fn handle_window_hidden(
  native_window: &NativeWindow,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let found_window = state.window_from_native(native_window);

  if let Some(window) = found_window {
    info!("Window hidden: {window}");

    // Update the display state.
    if config.value.general.hide_method != HideMethod::PlaceInCorner
      && window.display_state() == DisplayState::Hiding
    {
      window.set_display_state(DisplayState::Hidden);
      return Ok(());
    }

    // Unmanage the window if it's not in a display state transition. Also,
    // since window events are not 100% guaranteed to be in correct order,
    // we need to ignore events where the window is not actually hidden.
    if (config.value.general.hide_method == HideMethod::PlaceInCorner
      || window.display_state() == DisplayState::Shown)
      && !window.native().is_visible().unwrap_or(false)
    {
      // Keep windows hidden by a native virtual desktop switch managed,
      // so that switching back restores the exact layout.
      #[cfg(target_os = "windows")]
      if is_hidden_by_desktop_switch(&window, state) {
        info!(
          "Ignoring hide from native virtual desktop switch: {window}"
        );
        return Ok(());
      }

      unmanage_window(window, state)?;
    }
  }

  Ok(())
}

/// Whether the window was hidden by a native virtual desktop switch
/// (e.g. `ctrl+win+right`), as opposed to being genuinely hidden or
/// deliberately moved to another virtual desktop.
///
/// A desktop switch shell-cloaks the windows of *all* monitors at once,
/// so it's detected by no other managed window remaining on the current
/// virtual desktop. In contrast, when a single window is moved to
/// another desktop, the remaining windows stay on the current desktop
/// and the moved window is unmanaged as usual.
///
/// Windows pinned to all virtual desktops count as remaining, so a
/// desktop switch with pinned managed windows is misdetected as a
/// deliberate move; this is an accepted limitation.
#[cfg(target_os = "windows")]
fn is_hidden_by_desktop_switch(
  window: &crate::models::WindowContainer,
  state: &WmState,
) -> bool {
  use wm_platform::NativeWindowWindowsExt;

  use crate::traits::CommonGetters;

  if !window.native().is_shell_cloaked().unwrap_or(false) {
    return false;
  }

  !state.windows().iter().any(|other| {
    other.id() != window.id()
      && other
        .native()
        .is_on_current_virtual_desktop()
        .unwrap_or(false)
  })
}
