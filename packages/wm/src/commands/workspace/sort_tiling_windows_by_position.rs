use wm_common::TilingDirection;
use wm_platform::Rect;

use crate::{
  models::Workspace,
  traits::{CommonGetters, TilingDirectionGetters, WindowGetters},
};

/// Orders the tiling windows of a workspace by where they currently are
/// on screen.
///
/// Windows that already exist when the WM starts are managed in z-order,
/// which bears no relation to how they are arranged on screen, so the
/// layout would otherwise be shuffled on every start of the WM.
///
/// Only the order of the workspace's own tiling children is changed;
/// floating windows keep their place, and nested split containers are
/// left alone, since a nested layout can't be recovered from window
/// positions.
pub fn sort_tiling_windows_by_position(workspace: &Workspace) {
  let tiling_direction = workspace.tiling_direction();
  let mut children = workspace.borrow_children_mut();

  let (indexes, mut tiling_children): (Vec<_>, Vec<_>) = children
    .iter()
    .enumerate()
    .filter(|(_, child)| child.is_tiling_window())
    .map(|(index, child)| (index, child.clone()))
    .unzip();

  tiling_children.sort_by_key(|child| {
    let frame = child.as_window_container().map_or_else(
      |_| Rect::from_xy(0, 0, 0, 0),
      |window| window.native_properties().frame,
    );

    // Sort along the tiling direction first, so that windows of a
    // horizontal workspace are ordered left-to-right and windows of a
    // vertical workspace top-to-bottom.
    match tiling_direction {
      TilingDirection::Horizontal => (frame.left, frame.top),
      TilingDirection::Vertical => (frame.top, frame.left),
    }
  });

  // Write the sorted windows back into the places that tiling windows
  // occupied, so that floating windows stay where they are.
  for (index, child) in indexes.into_iter().zip(tiling_children) {
    children[index] = child;
  }
}

#[cfg(test)]
mod tests {
  use wm_common::{FloatingStateConfig, TilingDirection, WindowState};
  use wm_platform::Rect;

  use super::sort_tiling_windows_by_position;
  use crate::{
    models::{NonTilingWindow, TilingWindow, Workspace},
    traits::CommonGetters,
  };

  /// Mocks a tiling window at the given position.
  fn mock_window(x: i32, y: i32) -> TilingWindow {
    TilingWindow::mock()
      .floating_placement(Rect::from_xy(x, y, 100, 100))
      .call()
  }

  #[test]
  fn orders_horizontal_workspace_left_to_right() {
    let right = mock_window(500, 0);
    let left = mock_window(0, 0);

    let workspace = Workspace::mock()
      .tiling_direction(TilingDirection::Horizontal)
      .tiling_containers(vec![right.clone().into(), left.clone().into()])
      .call();

    sort_tiling_windows_by_position(&workspace);

    let ids = workspace
      .children()
      .iter()
      .map(CommonGetters::id)
      .collect::<Vec<_>>();

    assert_eq!(ids, vec![left.id(), right.id()]);
  }

  #[test]
  fn orders_vertical_workspace_top_to_bottom() {
    let bottom = mock_window(0, 500);
    let top = mock_window(0, 0);

    let workspace = Workspace::mock()
      .tiling_direction(TilingDirection::Vertical)
      .tiling_containers(vec![bottom.clone().into(), top.clone().into()])
      .call();

    sort_tiling_windows_by_position(&workspace);

    let ids = workspace
      .children()
      .iter()
      .map(CommonGetters::id)
      .collect::<Vec<_>>();

    assert_eq!(ids, vec![top.id(), bottom.id()]);
  }

  #[test]
  fn keeps_floating_windows_in_place() {
    let right = mock_window(500, 0);
    let left = mock_window(0, 0);

    let floating = NonTilingWindow::mock()
      .state(WindowState::Floating(FloatingStateConfig::default()))
      .call();

    let workspace = Workspace::mock()
      .tiling_direction(TilingDirection::Horizontal)
      .tiling_containers(vec![right.clone().into()])
      .non_tiling_windows(vec![floating.clone()])
      .call();

    // Attach the second tiling window after the floating one, so that
    // the floating window sits between the two tiling windows.
    crate::commands::container::attach_container(
      &left.clone().into(),
      &workspace.clone().into(),
      None,
    )
    .expect("Failed to attach window.");

    sort_tiling_windows_by_position(&workspace);

    let ids = workspace
      .children()
      .iter()
      .map(CommonGetters::id)
      .collect::<Vec<_>>();

    assert_eq!(ids, vec![left.id(), floating.id(), right.id()]);
  }
}
