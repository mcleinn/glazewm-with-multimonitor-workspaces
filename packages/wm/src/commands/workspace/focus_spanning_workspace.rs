use std::collections::VecDeque;

use anyhow::Context;
use tracing::info;
use wm_common::{PageKey, WmEvent};

use super::{
  activate_spanning_instance, deactivate_workspace, focus_workspace,
};
use crate::{
  commands::{
    container::set_focused_descendant, monitor::repage_spanning_desktops,
  },
  models::{Monitor, Workspace, WorkspaceTarget},
  traits::CommonGetters,
  user_config::UserConfig,
  wm_state::WmState,
};

/// Focuses a page of a spanning ("virtual desktop") workspace group,
/// switching the displayed workspace on every monitor at once.
///
/// Monitors without an instance of the page get one synthesized on
/// demand. The monitor that currently has focus keeps focus. All changes
/// accumulate into a single pending sync, so the switch is applied in one
/// platform pass.
pub fn focus_spanning_workspace(
  page_key: &PageKey,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let focused_workspace = state
    .focused_container()
    .and_then(|focused| focused.workspace())
    .context("No workspace is currently focused.")?;

  let origin_name = focused_workspace.logical_name();
  let target_name = page_key.to_string();

  if origin_name == target_name
    && config.value.general.toggle_workspace_on_refocus
  {
    if let Some(recent_name) = state.recent_workspace_name.clone() {
      if recent_name != target_name {
        return focus_workspace(
          WorkspaceTarget::Name(recent_name),
          state,
          config,
        );
      }
    }
  }

  info!("Focusing spanning workspace page: {page_key}");

  // Let instances of disconnected monitors take any free slot of the
  // page first, so that no empty instance gets synthesized in their
  // place.
  repage_spanning_desktops(state, config)?;

  // When already on the page, this still re-syncs monitors that drifted
  // (e.g. after focusing a single instance). All operations are no-ops
  // when consistent.
  let has_display_changes =
    sync_monitors_to_page(page_key, state, config)?;

  if has_display_changes {
    if origin_name != target_name {
      state.recent_workspace_name = Some(origin_name);
    }

    state.pending_sync.queue_cursor_jump();
  }

  Ok(())
}

/// Displays a page of a spanning workspace group on every monitor,
/// synthesizing missing instances, and focuses its instance on the
/// focused monitor.
///
/// Unlike `focus_spanning_workspace`, this has no toggle-on-refocus,
/// recent-workspace or cursor side effects, so it's safe to call while
/// handling monitor layout changes.
///
/// Returns `true` if the displayed workspace changed on any monitor.
pub fn sync_monitors_to_page(
  page_key: &PageKey,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<bool> {
  let focused_monitor = state
    .focused_container()
    .and_then(|focused| focused.monitor())
    .context("No monitor is currently focused.")?;

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
    has_display_changes |= sync_monitor_to_page(
      monitor,
      page_key,
      &focused_monitor,
      state,
      config,
    )?;
  }

  if has_display_changes {
    // Destroy all empty hidden workspaces. A page switch can hide up to
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

    // Close the gaps in page numbering left by destroyed instances.
    repage_spanning_desktops(state, config)?;
  }

  Ok(has_display_changes)
}

/// Displays the page's workspace instance on a single monitor,
/// synthesizing the instance if the monitor doesn't have one yet.
///
/// Only the focused monitor receives global focus; other monitors have
/// their displayed workspace changed in place.
///
/// Returns `true` if the monitor's displayed workspace changed.
fn sync_monitor_to_page(
  monitor: &Monitor,
  page_key: &PageKey,
  focused_monitor: &Monitor,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<bool> {
  let displayed_workspace = monitor
    .displayed_workspace()
    .context("No workspace is currently displayed.")?;

  let instance = match spanning_instance_on_monitor(monitor, page_key) {
    Some(instance) => instance,
    None => activate_spanning_instance(page_key, monitor, state, config)?,
  };

  let is_focused_monitor = monitor.id() == focused_monitor.id();
  let is_already_displayed = displayed_workspace.id() == instance.id();

  if is_already_displayed && !is_focused_monitor {
    return Ok(false);
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
    return Ok(false);
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

/// The workspace instance of a spanning page on the given monitor,
/// preferring the most recently focused one should there be several.
pub fn spanning_instance_on_monitor(
  monitor: &Monitor,
  page_key: &PageKey,
) -> Option<Workspace> {
  spanning_instances_on_monitor(monitor, page_key).pop_front()
}

/// All workspace instances of a spanning page on the given monitor,
/// ordered from most to least recently focused.
pub fn spanning_instances_on_monitor(
  monitor: &Monitor,
  page_key: &PageKey,
) -> VecDeque<Workspace> {
  monitor
    .borrow_child_focus_order()
    .iter()
    .filter_map(|workspace_id| monitor.child_by_id(workspace_id))
    .filter_map(|container| container.as_workspace().cloned())
    .filter(|workspace| {
      workspace.config().page_key().as_ref() == Some(page_key)
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use wm_common::PageKey;

  use super::{
    spanning_instance_on_monitor, spanning_instances_on_monitor,
  };
  use crate::{
    commands::container::set_focused_descendant,
    models::{Monitor, Workspace},
  };

  /// Mocks a spanning workspace instance with the given name and page.
  fn mock_instance(
    name: &str,
    group_name: &str,
    page: usize,
  ) -> Workspace {
    let workspace = Workspace::mock().name(name.to_string()).call();

    let mut config = workspace.config();
    config.spanning_group = Some(group_name.to_string());
    config.spanning_page = page;
    workspace.set_config(config);

    workspace
  }

  fn page(group: &str, page: usize) -> PageKey {
    PageKey {
      group: group.to_string(),
      page,
    }
  }

  #[test]
  fn finds_instance_by_page() {
    let monitor = Monitor::mock()
      .workspaces(vec![
        Workspace::mock().name("1".to_string()).call(),
        mock_instance("2#abc123", "2", 1),
        mock_instance("2/2", "2", 2),
      ])
      .call();

    let instance = spanning_instance_on_monitor(&monitor, &page("2", 1))
      .expect("Instance should be found.");
    assert_eq!(instance.config().name, "2#abc123");

    let instance = spanning_instance_on_monitor(&monitor, &page("2", 2))
      .expect("Instance should be found.");
    assert_eq!(instance.config().name, "2/2");

    assert!(
      spanning_instance_on_monitor(&monitor, &page("2", 3)).is_none()
    );
    assert!(
      spanning_instance_on_monitor(&monitor, &page("3", 1)).is_none()
    );
  }

  #[test]
  fn prefers_most_recently_focused_duplicate() {
    let duplicate = mock_instance("2#dup", "2", 1);

    let monitor = Monitor::mock()
      .workspaces(vec![mock_instance("2", "2", 1), duplicate.clone()])
      .call();

    // The first attached workspace is at the front of the focus order.
    let instance = spanning_instance_on_monitor(&monitor, &page("2", 1))
      .expect("Instance should be found.");
    assert_eq!(instance.config().name, "2");

    // After focusing the duplicate, it takes precedence.
    set_focused_descendant(
      &duplicate.clone().into(),
      Some(&monitor.clone().into()),
    );

    let instance = spanning_instance_on_monitor(&monitor, &page("2", 1))
      .expect("Instance should be found.");
    assert_eq!(instance.config().name, "2#dup");
  }

  #[test]
  fn lists_all_duplicates_by_recency() {
    let duplicate = mock_instance("2#dup", "2", 1);

    let monitor = Monitor::mock()
      .workspaces(vec![
        mock_instance("2", "2", 1),
        Workspace::mock().name("1".to_string()).call(),
        duplicate.clone(),
      ])
      .call();

    set_focused_descendant(
      &duplicate.clone().into(),
      Some(&monitor.clone().into()),
    );

    let names = spanning_instances_on_monitor(&monitor, &page("2", 1))
      .iter()
      .map(|workspace| workspace.config().name)
      .collect::<Vec<_>>();

    assert_eq!(names, vec!["2#dup".to_string(), "2".to_string()]);
    assert!(
      spanning_instances_on_monitor(&monitor, &page("3", 1)).is_empty()
    );
  }

  #[test]
  fn bounded_focus_changes_displayed_workspace() {
    let target = mock_instance("2", "2", 1);

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
