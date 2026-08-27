use serde::{Deserialize, Serialize};
use wm_platform::Rect;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ActiveDrag {
  /// Whether the drag is a move or resize.
  pub operation: Option<ActiveDragOperation>,

  /// Whether the drag is from a floating window.
  ///
  /// If `true`, it means we shouldn't drop the window as a tiling window
  /// on drag end.
  pub is_from_floating: bool,

  /// Initial position when the drag started.
  ///
  /// Used to calculate movement distance.
  pub initial_position: Rect,

  /// Whether the drag was detected when the window was first managed.
  ///
  /// This is the case for windows that are created while the user is
  /// already dragging them (e.g. a browser tab torn off into its own
  /// window). The OS might never emit a drag end event for such windows,
  /// so the drag is additionally ended on release of the mouse button.
  pub is_from_manage: bool,
}

#[derive(Debug, Copy, Clone, Deserialize, PartialEq, Serialize)]
pub enum ActiveDragOperation {
  Move,
  Resize,
}
