use wm_common::WindowState;

use crate::{
  commands::window::update_window_state,
  models::WindowContainer,
  traits::{CommonGetters, WindowGetters},
  user_config::UserConfig,
  wm_state::WmState,
};

/// Returns fullscreen windows of their previous state, if another tiling
/// window of their workspace has focus.
///
/// A fullscreen window covers its whole screen, so it hides the tiling
/// windows underneath. That is what the user asked for while the window
/// has focus, but a fullscreen window that *lost* focus to a tiling
/// window of the same screen would either bury the window the user is
/// working in, or be buried by it (the WM brings the focused workspace's
/// tiling windows forward) and leave no way of telling that it is
/// fullscreen at all. Its tile is therefore given back, which also
/// restores the layout the other windows of that screen had.
///
/// Floating windows are not affected, so a floating window can be used
/// on top of a fullscreen one.
pub fn exit_unfocused_fullscreen(
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let windows_to_exit = state
    .windows()
    .into_iter()
    .filter(|window| matches!(window.state(), WindowState::Fullscreen(_)))
    .filter(has_focused_tiling_sibling)
    .collect::<Vec<_>>();

  for window in windows_to_exit {
    tracing::info!("Exiting fullscreen of unfocused window: {window}");

    let target_state = window.toggled_state(window.state(), config);
    update_window_state(window, target_state, state, config)?;
  }

  Ok(())
}

/// Returns whether a tiling window other than the given window has focus
/// within its workspace.
fn has_focused_tiling_sibling(window: &WindowContainer) -> bool {
  window
    .workspace()
    .and_then(|workspace| workspace.descendant_focus_order().next())
    .is_some_and(|focused| {
      focused.id() != window.id()
        && focused
          .as_window_container()
          .is_ok_and(|focused| focused.state() == WindowState::Tiling)
    })
}

#[cfg(test)]
mod tests {
  use wm_common::{
    FloatingStateConfig, FullscreenStateConfig, WindowState,
  };

  use super::has_focused_tiling_sibling;
  use crate::{
    commands::container::set_focused_descendant,
    models::{Container, NonTilingWindow, TilingWindow, Workspace},
    traits::CommonGetters,
  };

  /// Mocks a workspace with a fullscreen window and a sibling window of
  /// the given state, and focuses the sibling.
  fn mock_workspace(
    sibling_state: Option<WindowState>,
  ) -> (NonTilingWindow, Workspace) {
    let fullscreen = NonTilingWindow::mock()
      .state(WindowState::Fullscreen(FullscreenStateConfig::default()))
      .call();

    let workspace = match sibling_state {
      Some(WindowState::Tiling) => Workspace::mock()
        .tiling_containers(vec![TilingWindow::mock().call().into()])
        .non_tiling_windows(vec![fullscreen.clone()])
        .call(),
      Some(state) => Workspace::mock()
        .non_tiling_windows(vec![
          NonTilingWindow::mock().state(state).call(),
          fullscreen.clone(),
        ])
        .call(),
      None => Workspace::mock()
        .non_tiling_windows(vec![fullscreen.clone()])
        .call(),
    };

    (fullscreen, workspace)
  }

  #[test]
  fn detects_focused_tiling_sibling() {
    let (fullscreen, workspace) =
      mock_workspace(Some(WindowState::Tiling));

    let sibling = workspace
      .children()
      .into_iter()
      .find(Container::is_tiling_window)
      .expect("Sibling should exist.");

    set_focused_descendant(&sibling, None);

    assert!(has_focused_tiling_sibling(&fullscreen.clone().into()));

    // The fullscreen window keeps its state while it has focus itself.
    set_focused_descendant(&fullscreen.clone().into(), None);

    assert!(!has_focused_tiling_sibling(&fullscreen.into()));
  }

  #[test]
  fn ignores_focused_floating_sibling() {
    let (fullscreen, workspace) = mock_workspace(Some(
      WindowState::Floating(FloatingStateConfig::default()),
    ));

    let sibling = workspace
      .children()
      .into_iter()
      .find(|child| child.id() != fullscreen.id())
      .expect("Sibling should exist.");

    set_focused_descendant(&sibling, None);

    assert!(!has_focused_tiling_sibling(&fullscreen.into()));
  }

  #[test]
  fn ignores_lone_fullscreen_window() {
    let (fullscreen, _workspace) = mock_workspace(None);

    assert!(!has_focused_tiling_sibling(&fullscreen.into()));
  }
}
