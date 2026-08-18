use anyhow::Context;
use tracing::info;
use wm_common::WmEvent;

use super::{
  activate_spanning_instance, deactivate_workspace, focus_workspace,
};
use crate::{
  commands::container::set_focused_descendant,
  models::{Monitor, Workspace, WorkspaceTarget},
  traits::CommonGetters,
  user_config::UserConfig,
  wm_state::WmState,
};

/// Focuses a spanning ("virtual desktop") workspace group, switching the
/// displayed workspace on every monitor at once.
///
/// Monitors without an instance of the group get one synthesized on
/// demand. The monitor that currently has focus keeps focus. All changes
/// accumulate into a single pending sync, so the switch is applied in one
/// platform pass.
pub fn focus_spanning_workspace(
  group_name: &str,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let focused_workspace = state
    .focused_container()
    .and_then(|focused| focused.workspace())
    .context("No workspace is currently focused.")?;

  let origin_name = focused_workspace.logical_name();

  if origin_name == group_name
    && config.value.general.toggle_workspace_on_refocus
  {
    if let Some(recent_name) = state.recent_workspace_name.clone() {
      if recent_name != group_name {
        return focus_workspace(
          WorkspaceTarget::Name(recent_name),
          state,
          config,
        );
      }
    }
  }

  // When already on the group, the loop below still runs to re-sync
  // monitors that drifted (e.g. after focusing a single instance or a
  // monitor topology change). All operations are no-ops when consistent.

  let focused_monitor = focused_workspace
    .monitor()
    .context("Focused workspace has no monitor.")?;

  info!("Focusing spanning workspace: {group_name}");

  // Monitors in order of most to least recently focused.
  let monitors_by_recency = state
    .root_container
    .borrow_child_focus_order()
    .iter()
    .filter_map(|monitor_id| state.root_container.child_by_id(monitor_id))
    .filter_map(|container| container.as_monitor().cloned())
    .collect::<Vec<_>>();

  let mut has_display_changes = false;

  // Iterate in reverse recency: the bounded `set_focused_descendant`
  // calls below bump each processed monitor to the front of the root's
  // focus order, so reverse iteration restores the original monitor
  // focus order. The focused monitor is processed last and re-focused
  // explicitly.
  for monitor in monitors_by_recency.iter().rev() {
    has_display_changes |= sync_monitor_to_group(
      monitor,
      group_name,
      &focused_monitor,
      state,
      config,
    )?;
  }

  if has_display_changes {
    // Destroy all empty hidden workspaces. A group switch can hide up to
    // one emptied workspace per monitor, so this intentionally destroys
    // more than the single workspace that a regular focus switch does.
    let workspaces_to_destroy = state
      .workspaces()
      .into_iter()
      .filter(|workspace| {
        !workspace.config().keep_alive
          && !workspace.has_children()
          && !workspace.is_displayed()
      })
      .collect::<Vec<_>>();

    for workspace in workspaces_to_destroy {
      deactivate_workspace(workspace, state)?;
    }

    if origin_name != group_name {
      state.recent_workspace_name = Some(origin_name);
    }

    state.pending_sync.queue_cursor_jump();
  }

  Ok(())
}

/// Displays the group's workspace instance on a single monitor,
/// synthesizing the instance if the monitor doesn't have one yet.
///
/// Only the focused monitor receives global focus; other monitors have
/// their displayed workspace changed in place.
///
/// Returns `true` if the monitor's displayed workspace changed.
fn sync_monitor_to_group(
  monitor: &Monitor,
  group_name: &str,
  focused_monitor: &Monitor,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<bool> {
  let displayed_workspace = monitor
    .displayed_workspace()
    .context("No workspace is currently displayed.")?;

  let instance = spanning_instance_on_monitor(monitor, group_name);

  let (instance, is_new_instance) = match instance {
    Some(instance) => (instance, false),
    None => (
      activate_spanning_instance(group_name, monitor, state, config)?,
      true,
    ),
  };

  let is_focused_monitor = monitor.id() == focused_monitor.id();
  let is_already_displayed = displayed_workspace.id() == instance.id();

  if is_already_displayed && !is_focused_monitor {
    return Ok(is_new_instance);
  }

  // Focus the last-focused container within the instance, falling back
  // to the workspace itself when empty.
  let container_to_focus = instance
    .descendant_focus_order()
    .next()
    .unwrap_or_else(|| instance.clone().into());

  if is_focused_monitor {
    set_focused_descendant(&container_to_focus, None);
    state.pending_sync.queue_focus_change();
  } else {
    // Bounding at the monitor changes its displayed workspace without
    // stealing global focus.
    set_focused_descendant(
      &container_to_focus,
      Some(&monitor.clone().into()),
    );
  }

  if is_already_displayed {
    return Ok(is_new_instance);
  }

  state
    .pending_sync
    .queue_container_to_redraw(displayed_workspace.clone())
    .queue_container_to_redraw(instance.clone());

  // Notify subscribers (e.g. status bars) of the display change on this
  // monitor. `FocusChanged` only covers the focused monitor.
  state.emit_event(WmEvent::WorkspaceUpdated {
    updated_workspace: displayed_workspace.to_dto()?,
  });
  state.emit_event(WmEvent::WorkspaceUpdated {
    updated_workspace: instance.to_dto()?,
  });

  Ok(true)
}

/// The workspace instance of a spanning group on the given monitor,
/// preferring the most recently focused one (duplicates are possible
/// after a monitor removal).
pub fn spanning_instance_on_monitor(
  monitor: &Monitor,
  group_name: &str,
) -> Option<Workspace> {
  monitor
    .borrow_child_focus_order()
    .iter()
    .filter_map(|workspace_id| monitor.child_by_id(workspace_id))
    .filter_map(|container| container.as_workspace().cloned())
    .find(|workspace| workspace.logical_name() == group_name)
}

#[cfg(test)]
mod tests {
  use super::spanning_instance_on_monitor;
  use crate::{
    commands::container::set_focused_descendant,
    models::{Monitor, Workspace},
  };

  /// Mocks a spanning workspace instance with the given name and group.
  fn mock_instance(name: &str, group_name: &str) -> Workspace {
    let workspace = Workspace::mock().name(name.to_string()).call();

    let mut config = workspace.config();
    config.spanning_group = Some(group_name.to_string());
    workspace.set_config(config);

    workspace
  }

  #[test]
  fn finds_instance_by_group_name() {
    let monitor = Monitor::mock()
      .workspaces(vec![
        Workspace::mock().name("1".to_string()).call(),
        mock_instance("2#abc123", "2"),
      ])
      .call();

    let instance = spanning_instance_on_monitor(&monitor, "2")
      .expect("Instance should be found.");

    assert_eq!(instance.config().name, "2#abc123");
    assert!(spanning_instance_on_monitor(&monitor, "3").is_none());
  }

  #[test]
  fn prefers_most_recently_focused_duplicate() {
    let duplicate = mock_instance("2#dup", "2");

    let monitor = Monitor::mock()
      .workspaces(vec![mock_instance("2", "2"), duplicate.clone()])
      .call();

    // The first attached workspace is at the front of the focus order.
    let instance = spanning_instance_on_monitor(&monitor, "2")
      .expect("Instance should be found.");
    assert_eq!(instance.config().name, "2");

    // After focusing the duplicate, it takes precedence.
    set_focused_descendant(
      &duplicate.clone().into(),
      Some(&monitor.clone().into()),
    );

    let instance = spanning_instance_on_monitor(&monitor, "2")
      .expect("Instance should be found.");
    assert_eq!(instance.config().name, "2#dup");
  }

  #[test]
  fn bounded_focus_changes_displayed_workspace() {
    let target = mock_instance("2", "2");

    let monitor = Monitor::mock()
      .workspaces(vec![
        Workspace::mock().name("1".to_string()).call(),
        target.clone(),
      ])
      .call();

    assert_eq!(
      monitor.displayed_workspace().map(|ws| ws.config().name),
      Some("1".to_string())
    );

    set_focused_descendant(
      &target.clone().into(),
      Some(&monitor.clone().into()),
    );

    assert_eq!(
      monitor.displayed_workspace().map(|ws| ws.config().name),
      Some("2".to_string())
    );
  }
}
