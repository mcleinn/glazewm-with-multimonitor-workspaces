use tracing::info;

use super::deactivate_workspace;
use crate::{
  commands::container::move_container_within_tree, models::Workspace,
  traits::CommonGetters, wm_state::WmState,
};

/// Merges a duplicate instance of a spanning ("virtual desktop") workspace
/// group into another instance of the same group, then deactivates the
/// merged instance.
///
/// Duplicates arise when a monitor is removed and its instances migrate
/// to a monitor that already has one. Leaving them hidden would make
/// their windows unreachable, since hidden workspaces are cloaked.
///
/// Every direct child of `source` (windows and split containers, so the
/// layout within `source` is preserved) is appended to `target`. The
/// caller is responsible for queueing redraws.
pub fn merge_spanning_instances(
  source: &Workspace,
  target: &Workspace,
  state: &mut WmState,
) -> anyhow::Result<()> {
  info!("Merging workspace {source} into {target}.");

  for child in source.children() {
    move_container_within_tree(
      &child,
      &target.clone().into(),
      target.child_count(),
      state,
    )?;
  }

  deactivate_workspace(source.clone(), state)
}
