use anyhow::Context;
use tracing::info;
use wm_common::WmEvent;

use crate::{
  commands::{
    container::{detach_container, move_container_within_tree},
    workspace::{
      merge_spanning_instances, sort_workspaces,
      spanning_instance_on_monitor,
    },
  },
  models::Monitor,
  traits::CommonGetters,
  user_config::UserConfig,
  wm_state::WmState,
};

#[allow(clippy::needless_pass_by_value)]
pub fn remove_monitor(
  monitor: Monitor,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  info!("Removing monitor: {monitor}");

  let target_monitor = state
    .monitors()
    .into_iter()
    .find(|m| m.id() != monitor.id())
    .context("No target monitor to move workspaces.")?;

  // Avoid moving empty workspaces.
  let workspaces_to_move =
    monitor.workspaces().into_iter().filter(|workspace| {
      workspace.has_children() || workspace.config().keep_alive
    });

  for workspace in workspaces_to_move {
    // Merge instances of a spanning workspace group into the target
    // monitor's existing instance, rather than parking them as hidden
    // duplicates whose windows would be unreachable.
    let existing_instance =
      workspace.config().spanning_group.and_then(|group| {
        spanning_instance_on_monitor(&target_monitor, &group)
      });

    if let Some(existing_instance) = existing_instance {
      merge_spanning_instances(&workspace, &existing_instance, state)?;

      state.emit_event(WmEvent::WorkspaceUpdated {
        updated_workspace: existing_instance.to_dto()?,
      });

      continue;
    }

    // Move workspace to target monitor.
    move_container_within_tree(
      &workspace.clone().into(),
      &target_monitor.clone().into(),
      target_monitor.child_count(),
      state,
    )?;

    sort_workspaces(&target_monitor, config)?;

    state.emit_event(WmEvent::WorkspaceUpdated {
      updated_workspace: workspace.to_dto()?,
    });
  }

  detach_container(monitor.clone().into())?;

  state.emit_event(WmEvent::MonitorRemoved {
    removed_id: monitor.id(),
    removed_device_name: monitor.native_properties().device_name,
  });

  Ok(())
}
