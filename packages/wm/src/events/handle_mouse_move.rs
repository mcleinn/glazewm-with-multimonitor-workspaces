use anyhow::Context;
use wm_common::try_warn;
#[cfg(target_os = "windows")]
use wm_platform::NativeWindowWindowsExt;
use wm_platform::{MouseButton, MouseEvent};

use crate::{
  commands::container::set_focused_descendant,
  events::handle_window_moved_or_resized_end,
  models::WindowContainer,
  traits::{CommonGetters, WindowGetters},
  user_config::UserConfig,
  wm_state::WmState,
};

pub fn handle_mouse_move(
  event: &MouseEvent,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  // Ignore mouse move events if the WM is paused. The mouse listener
  // should anyways be disabled when the WM is paused, but this is just in
  // case any events slipped through while disabling.
  if state.is_paused {
    return Ok(());
  }

  // Detect when a window drag operation has ended by listening to the
  // release of left click.
  //
  // On Windows, this is only done for drags that were detected when the
  // window was first managed (e.g. a torn-off browser tab) and only if
  // the window isn't in an OS move/size loop, since the OS might never
  // emit a `MovedOrResized` event with `is_interactive_end` for those.
  // Otherwise, it leads to race conditions where the mouse event comes in
  // before the `MovedOrResized` event with `is_interactive_end`, and the
  // OS then overrides the window's position when its loop ends. For
  // example, if the user drags to maximize a window, the WS_MAXIMIZED
  // state is sometimes set after the mouse event.
  if let MouseEvent::ButtonUp { button, .. } = event {
    if *button == MouseButton::Left {
      let active_drag_windows = state
        .windows()
        .into_iter()
        .filter(should_end_drag_on_button_up);

      // Only one window should ever be actively dragged at a time, but
      // just in case, iterate over all active drag windows.
      for window in active_drag_windows {
        let new_rect = try_warn!(window.native().frame());

        window.update_native_properties(|properties| {
          properties.frame = new_rect;
        });

        handle_window_moved_or_resized_end(&window, state, config)?;
      }
    }

    return Ok(());
  }

  if let MouseEvent::Move {
    pressed_buttons,
    // LINT: `window_below_cursor` is only used on macOS.
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    window_below_cursor,
    position,
    ..
  } = event
  {
    // Ignore event if left/right-click is down. Otherwise, this causes
    // focus to jitter when a window is being resized by its drag
    // handles. Also ignore if the OS focused window isn't the same as
    // the WM's focused window.
    if pressed_buttons.contains(&MouseButton::Left)
      || pressed_buttons.contains(&MouseButton::Right)
      || !state.is_focus_synced
      || !config.value.general.focus_follows_cursor
    {
      return Ok(());
    }

    let window_under_cursor = {
      #[cfg(target_os = "macos")]
      {
        window_below_cursor.and_then(|window_id| {
          use crate::traits::WindowGetters;

          state
            .windows()
            .into_iter()
            .find(|w| w.native().id() == window_id)
        })
      }
      #[cfg(target_os = "windows")]
      {
        state
          .dispatcher
          .window_from_point(position)?
          .and_then(|native| state.window_from_native(&native))
      }
    };

    // Set focus to whichever window is currently under the cursor.
    if let Some(window) = window_under_cursor {
      let focused_container =
        state.focused_container().context("No focused container.")?;

      if focused_container.id() != window.id() {
        set_focused_descendant(&window.as_container(), None);
        state.pending_sync.queue_focus_change();
      }
    } else {
      // Focus the monitor if no window is under the cursor.
      let cursor_monitor = state
        .monitor_at_point(position)
        .context("No monitor under cursor.")?;

      let focused_monitor = state
        .focused_container()
        .context("No focused container.")?
        .monitor()
        .context("Focused container has no monitor.")?;

      // Avoid setting focus to the same monitor.
      if cursor_monitor.id() != focused_monitor.id() {
        set_focused_descendant(&cursor_monitor.as_container(), None);
        state.pending_sync.queue_focus_change();
      }
    }
  }

  Ok(())
}

/// Whether a window's active drag should be ended on release of the left
/// mouse button.
fn should_end_drag_on_button_up(window: &WindowContainer) -> bool {
  let Some(active_drag) = window.active_drag() else {
    return false;
  };

  #[cfg(target_os = "macos")]
  {
    let _ = active_drag;
    true
  }
  #[cfg(target_os = "windows")]
  {
    // Drags within an OS move/size loop are ended by the `MovedOrResized`
    // event with `is_interactive_end` instead.
    active_drag.is_from_manage
      && !window.native().is_in_move_size_loop().unwrap_or(false)
  }
}
