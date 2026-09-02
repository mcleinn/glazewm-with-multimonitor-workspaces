use anyhow::Context;
use tracing::info;

use super::{activate_workspace, focus_spanning_workspace};
use crate::{
  commands::{
    container::set_focused_descendant, workspace::deactivate_workspace,
  },
  models::{Workspace, WorkspaceTarget},
  traits::CommonGetters,
  user_config::UserConfig,
  wm_state::WmState,
};

/// Focuses a workspace by a given target.
///
/// This target can be a workspace name, the most recently focused
/// workspace, the next workspace, the previous workspace, or the workspace
/// in a given direction from the currently focused workspace.
///
/// The workspace will be activated if it isn't already active.
pub fn focus_workspace(
  target: WorkspaceTarget,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let focused_workspace = state
    .focused_container()
    .and_then(|focused| focused.workspace())
    .context("No workspace is currently focused.")?;

  // Pages of spanning ("virtual desktop") workspaces switch all monitors
  // at once.
  let page_key = match &target {
    WorkspaceTarget::Name(name) => {
      state.resolve_spanning_target(name, config)
    }
    WorkspaceTarget::Recent => state
      .recent_workspace_name
      .as_deref()
      .and_then(|name| state.resolve_spanning_target(name, config)),
    _ => None,
  };

  if let Some(page_key) = page_key {
    return focus_spanning_workspace(&page_key, state, config);
  }

  let (target_workspace_name, target_workspace) =
    state.workspace_by_target(&focused_workspace, target, config)?;

  // Retrieve or activate the target workspace by its name.
  let target_workspace = match target_workspace {
    Some(_) => anyhow::Ok(target_workspace),
    _ => match target_workspace_name {
      Some(name) => {
        activate_workspace(Some(&name), None, state, config)?;

        Ok(state.workspace_by_name(&name))
      }
      _ => Ok(None),
    },
  }?;

  if let Some(target_workspace) = target_workspace {
    focus_workspace_instance(&target_workspace, state)?;

    // Save the currently focused workspace as recent. For instances of
    // spanning workspaces, the page label is saved instead so that
    // recent-toggling returns to the whole page.
    state.recent_workspace_name = Some(focused_workspace.logical_name());
    state.pending_sync.queue_cursor_jump();
  }

  Ok(())
}

/// Focuses a single active workspace, displaying it on its monitor.
///
/// For instances of spanning workspaces, only this one instance is
/// affected (other monitors keep their displayed workspace). Focusing the
/// whole page is done via `focus_spanning_workspace`.
pub fn focus_workspace_instance(
  target_workspace: &Workspace,
  state: &mut WmState,
) -> anyhow::Result<()> {
  info!("Focusing workspace: {target_workspace}");

  // Get the currently displayed workspace on the same monitor that the
  // workspace to focus is on.
  let displayed_workspace = target_workspace
    .monitor()
    .and_then(|monitor| monitor.displayed_workspace())
    .context("No workspace is currently displayed.")?;

  // Set focus to whichever window last had focus in workspace. If the
  // workspace has no windows, then set focus to the workspace itself.
  let container_to_focus = target_workspace
    .descendant_focus_order()
    .next()
    .unwrap_or_else(|| target_workspace.clone().into());

  set_focused_descendant(&container_to_focus, None);
  state.pending_sync.queue_focus_change();

  // Display the workspace to switch focus to.
  state
    .pending_sync
    .queue_container_to_redraw(displayed_workspace)
    .queue_container_to_redraw(target_workspace.clone());

  // Get empty workspace to destroy (if one is found). Cannot destroy
  // empty workspaces if they're the only workspace on the monitor.
  let workspace_to_destroy =
    state.workspaces().into_iter().find(|workspace| {
      !workspace.config().keep_alive
        && !workspace.has_children()
        && !workspace.is_displayed()
    });

  if let Some(workspace) = workspace_to_destroy {
    deactivate_workspace(workspace, state)?;
  }

  Ok(())
}
