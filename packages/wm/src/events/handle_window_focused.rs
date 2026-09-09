use anyhow::Context;
use tracing::info;
use wm_common::{DisplayState, WindowRuleEvent, WindowState, WmEvent};
use wm_platform::NativeWindow;

use crate::{
  commands::{
    container::set_focused_descendant,
    window::{manage_window, run_window_rules, update_window_state},
    workspace::{focus_workspace, focus_workspace_instance},
  },
  models::WorkspaceTarget,
  traits::{CommonGetters, WindowGetters},
  user_config::UserConfig,
  wm_state::WmState,
};

pub fn handle_window_focused(
  native_window: &NativeWindow,
  state: &mut WmState,
  config: &mut UserConfig,
) -> anyhow::Result<()> {
  // Both have to run before the window is looked up, since they can
  // replace or add the window's container.
  restore_focused_window(native_window, state, config)?;
  manage_focused_window(native_window, state, config)?;

  let found_window = state.window_from_native(native_window);
  let focused_container =
    state.focused_container().context("No focused container.")?;

  // Update the focus sync state. If the OS focused window is not same as
  // the WM's focused container, then the focus is not synced.
  state.is_focus_synced = match focused_container.as_window_container() {
    Ok(window) => *window.native() == *native_window,
    _ => native_window.is_desktop_window().unwrap_or(false),
  };

  // Handle overriding focus on close/minimize. After a window is closed
  // or minimized, the OS or the closed application might automatically
  // switch focus to a different window. To force focus to go to the WM's
  // target focus container, we reassign any focus events 100ms after
  // close/minimize. This will cause focus to briefly flicker to the OS
  // focus target and then to the WM's focus target.
  if should_override_focus(state) {
    state.pending_sync.queue_focus_change();
    return Ok(());
  }

  // Ignore the focus event if window is being hidden by the WM.
  if let Some(window) = &found_window {
    if window.display_state() == DisplayState::Hiding {
      return Ok(());
    }
  }

  // Focus effect should be updated for any change in focus that shouldn't
  // be overwritten. The incoming focus event at this point is either:
  //  1. WM's focus container (window or workspace). This is the desktop
  //     window in the case of a workspace.
  //  2. An ignored window.
  //  3. A window that received manual focus.
  state.pending_sync.queue_focused_effect_update();

  if let Some(window) = found_window {
    let workspace = window.workspace().context("No workspace")?;

    // Native focus has been synced to the WM's focused container.
    if focused_container == window.clone().into() {
      state.is_focus_synced = true;
      state.pending_sync.queue_workspace_to_reorder(workspace);
      return Ok(());
    }

    info!("Window manually focused: {window}");

    // Handle focus events from windows on hidden workspaces. For example,
    // if Discord is forcefully shown by the OS when it's on a hidden
    // workspace, switch focus to Discord's workspace.
    if window.display_state() == DisplayState::Hidden {
      info!("Focusing off-screen window: {window}");

      if workspace.config().spanning_group.is_some() {
        // Display exactly the instance holding the window; resolving
        // its name would select a page instead.
        focus_workspace_instance(&workspace, state)?;
      } else {
        focus_workspace(
          WorkspaceTarget::Name(workspace.config().name),
          state,
          config,
        )?;
      }
    }

    // Update the WM's focus state.
    set_focused_descendant(&window.clone().into(), None);

    // Run window rules for focus events.
    run_window_rules(
      window.clone(),
      &WindowRuleEvent::Focus,
      state,
      config,
    )?;

    state.is_focus_synced = true;
    state.pending_sync.queue_workspace_to_reorder(workspace);

    // Broadcast the focus change event.
    state.emit_event(WmEvent::FocusChanged {
      focused_container: window.to_dto()?,
    });
  }

  Ok(())
}

/// Transitions a window that the WM has as minimized back to its previous
/// state, after the OS has given it focus.
///
/// The OS restores a minimized window when it's focused (e.g. by clicking
/// its taskbar button), but the `MinimizeEnded` event for that restore can
/// arrive after the focus event, and its handler can read the window as
/// still minimized while the restore is in flight. Either way the WM would
/// be left with the window as minimized and minimize it again on its next
/// redraw, so `NativeWindow::is_minimized` is deliberately not consulted
/// here: a window that has focus is never minimized.
fn restore_focused_window(
  native_window: &NativeWindow,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let Some(window) = state.window_from_native(native_window) else {
    return Ok(());
  };

  if window.state() != WindowState::Minimized {
    return Ok(());
  }

  info!("Window restored by focus: {window}");

  window.update_native_properties(|properties| {
    properties.is_minimized = false;
  });

  let target_state = window
    .prev_state()
    .unwrap_or(WindowState::default_from_config(&config.value));

  update_window_state(window, target_state, state, config)?;

  Ok(())
}

/// Manages a window that received focus while unmanaged.
///
/// Windows are normally managed when they're shown, or on startup if
/// they're already visible. A window that is cloaked at that point is
/// skipped as invisible. That's correct for windows on another native
/// virtual desktop, which are managed once a desktop switch uncloaks
/// them, but a cloak left behind by a previous WM instance that exited
/// without restoring its hidden windows is never lifted by the OS. Such a
/// window stays invisible even though the OS focuses it (e.g. when its
/// taskbar button is clicked), so it's managed here, after lifting the
/// stale cloak.
///
/// Managing on focus is limited by the same checks as managing on show,
/// so windows that aren't manageable (e.g. the taskbar) are unaffected.
fn manage_focused_window(
  native_window: &NativeWindow,
  state: &mut WmState,
  config: &mut UserConfig,
) -> anyhow::Result<()> {
  if state.window_from_native(native_window).is_some()
    || state.ignored_windows.contains(native_window)
  {
    return Ok(());
  }

  #[cfg(target_os = "windows")]
  uncloak_stale_window(native_window);

  manage_window(native_window.clone(), None, state, config)
}

/// Lifts a cloak that a previous WM instance left on a focused window.
///
/// A focused window on the current native virtual desktop can't
/// legitimately be shell-cloaked, so its cloak is stale. Windows on
/// another virtual desktop are left alone, since the desktop switch that
/// follows uncloaks them.
#[cfg(target_os = "windows")]
fn uncloak_stale_window(native_window: &NativeWindow) {
  use wm_platform::NativeWindowWindowsExt;

  let has_stale_cloak = native_window.is_shell_cloaked().unwrap_or(false)
    && native_window
      .is_on_current_virtual_desktop()
      .unwrap_or(false);

  if !has_stale_cloak {
    return;
  }

  info!(
    "Lifting stale cloak from focused window: {:?}",
    native_window.id()
  );

  if let Err(err) = native_window.set_cloaked(false) {
    tracing::warn!("Failed to lift stale cloak: {err}");
  }
}

/// Returns true if focus should be reassigned to the WM's focus container.
fn should_override_focus(state: &WmState) -> bool {
  let has_recent_unmanage = state
    .unmanaged_or_minimized_timestamp
    .is_some_and(|time| time.elapsed().as_millis() < 100);

  has_recent_unmanage && !state.is_focus_synced
}
