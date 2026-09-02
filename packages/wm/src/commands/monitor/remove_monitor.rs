use anyhow::Context;
use tracing::info;
use wm_common::WmEvent;

use crate::{
  commands::{
    container::{
      attach_container, detach_container, move_container_within_tree,
      set_focused_descendant,
    },
    workspace::sort_workspaces,
  },
  models::Monitor,
  traits::CommonGetters,
  user_config::UserConfig,
  wm_state::WmState,
};

/// Removes a monitor, moving its workspaces to another monitor.
///
/// Regular workspaces are moved as in upstream (a focused workspace stays
/// displayed). Instances of spanning workspaces are parked hidden on the
/// target monitor; `repage_spanning_desktops` later assigns them their
/// page and monitor, so their layout survives until the monitor is
/// reconnected.
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

  let was_focused = state
    .focused_container()
    .and_then(|focused| focused.monitor())
    .is_some_and(|focused_monitor| focused_monitor.id() == monitor.id());

  // Avoid moving empty workspaces.
  let workspaces_to_move =
    monitor.workspaces().into_iter().filter(|workspace| {
      workspace.has_children() || workspace.config().keep_alive
    });

  for workspace in workspaces_to_move {
    if workspace.config().spanning_group.is_some() {
      // Park the instance at the back of the target monitor's focus
      // order, so it's hidden and doesn't displace the target's own
      // instance of the same page.
      detach_container(workspace.clone().into())?;
      attach_container(
        &workspace.clone().into(),
        &target_monitor.clone().into(),
        None,
      )?;
    } else {
      move_container_within_tree(
        &workspace.clone().into(),
        &target_monitor.clone().into(),
        target_monitor.child_count(),
        state,
      )?;
    }

    sort_workspaces(&target_monitor, config)?;

    state.emit_event(WmEvent::WorkspaceUpdated {
      updated_workspace: workspace.to_dto()?,
    });
  }

  detach_container(monitor.clone().into())?;

  if was_focused {
    // The focused container either went with the monitor or sits in a
    // parked, hidden instance. Focus the target monitor's displayed
    // workspace instead, so that native focus doesn't stay on a hidden
    // window (which would get its workspace force-displayed on the next
    // focus event).
    let displayed_workspace = target_monitor
      .displayed_workspace()
      .context("No displayed workspace on target monitor.")?;

    let container_to_focus = displayed_workspace
      .descendant_focus_order()
      .next()
      .unwrap_or_else(|| displayed_workspace.clone().into());

    set_focused_descendant(&container_to_focus, None);
    state.pending_sync.queue_focus_change();
  }

  state.emit_event(WmEvent::MonitorRemoved {
    removed_id: monitor.id(),
    removed_device_name: monitor.native_properties().device_name,
  });

  Ok(())
}
