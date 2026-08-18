use anyhow::Context;
use tracing::info;
use wm_common::{TilingDirection, WmEvent, WorkspaceConfig};

use super::sort_workspaces;
use crate::{
  commands::container::attach_container,
  models::{Monitor, Workspace},
  traits::{CommonGetters, PositionGetters},
  user_config::UserConfig,
  wm_state::WmState,
};

/// Activates a workspace on the target monitor.
///
/// If no workspace name is provided, the first suitable workspace defined
/// in the user's config will be used.
///
/// If no target monitor is provided, the workspace is activated on
/// whichever monitor it is bound to, or the currently focused monitor.
pub fn activate_workspace(
  workspace_name: Option<&str>,
  target_monitor: Option<Monitor>,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let mut workspace_config = workspace_config(
    workspace_name,
    target_monitor.clone(),
    state,
    config,
  )?;

  if workspace_config.monitors.is_some() {
    // Instances of spanning workspaces are keyed by their group name,
    // and ignore `bind_to_monitor`.
    workspace_config.spanning_group = Some(workspace_config.name.clone());
    workspace_config.bind_to_monitor = None;
  }

  let target_monitor = target_monitor
    .or_else(|| {
      workspace_config
        .bind_to_monitor
        .and_then(|index| {
          state
            .monitors()
            .into_iter()
            .find(|monitor| monitor.index() == index as usize)
        })
        .or_else(|| {
          state
            .focused_container()
            .and_then(|focused| focused.monitor())
        })
    })
    .context("Failed to get a target monitor for the workspace.")?;

  let monitor_rect = target_monitor.to_rect()?;

  let tiling_direction = if monitor_rect.height() > monitor_rect.width() {
    TilingDirection::Vertical
  } else {
    TilingDirection::Horizontal
  };

  let workspace = Workspace::new(
    workspace_config.clone(),
    config.value.gaps.clone(),
    tiling_direction,
  );

  // Attach the created workspace to the specified monitor.
  attach_container(
    &workspace.clone().into(),
    &target_monitor.clone().into(),
    None,
  )?;

  sort_workspaces(&target_monitor, config)?;

  info!("Activating workspace: {workspace}");

  state.emit_event(WmEvent::WorkspaceActivated {
    activated_workspace: workspace.to_dto()?,
  });

  Ok(())
}

/// Creates (but does not display) an instance of a spanning workspace
/// group on the given monitor.
///
/// Unlike `activate_workspace`, this doesn't require the group name to be
/// inactive: each monitor gets its own instance with a globally unique
/// name derived from the group name.
///
/// Returns the newly created `Workspace`.
pub fn activate_spanning_instance(
  group_name: &str,
  target_monitor: &Monitor,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<Workspace> {
  let group_config = config
    .spanning_workspace_config(group_name)
    .with_context(|| {
      format!("No spanning workspace config with name '{group_name}'.")
    })?;

  let instance_name =
    instance_name(group_name, target_monitor, state, config);

  let workspace_config =
    spanning_instance_config(group_config, instance_name);

  let monitor_rect = target_monitor.to_rect()?;

  let tiling_direction = if monitor_rect.height() > monitor_rect.width() {
    TilingDirection::Vertical
  } else {
    TilingDirection::Horizontal
  };

  let workspace = Workspace::new(
    workspace_config,
    config.value.gaps.clone(),
    tiling_direction,
  );

  // Attach the created workspace to the specified monitor. The workspace
  // lands at the back of the monitor's focus order, so it isn't displayed
  // until explicitly promoted.
  attach_container(
    &workspace.clone().into(),
    &target_monitor.clone().into(),
    None,
  )?;

  sort_workspaces(target_monitor, config)?;

  info!("Activating spanning workspace instance: {workspace}");

  state.emit_event(WmEvent::WorkspaceActivated {
    activated_workspace: workspace.to_dto()?,
  });

  Ok(workspace)
}

/// Derives the config for an instance of a spanning workspace group.
///
/// Every instance shares the group's `display_name` (falling back to the
/// group name), so status bars show the same label on all monitors.
pub fn spanning_instance_config(
  group_config: &WorkspaceConfig,
  instance_name: String,
) -> WorkspaceConfig {
  WorkspaceConfig {
    name: instance_name,
    display_name: Some(
      group_config
        .display_name
        .clone()
        .unwrap_or_else(|| group_config.name.clone()),
    ),
    bind_to_monitor: None,
    keep_alive: group_config.keep_alive,
    monitors: group_config.monitors,
    spanning_group: Some(group_config.name.clone()),
  }
}

/// Globally unique name for a new instance of a spanning workspace group.
///
/// The first instance takes the plain group name; further instances get a
/// `#<monitor-id>` suffix, uniquified against active workspaces and
/// user-declared config names.
fn instance_name(
  group_name: &str,
  target_monitor: &Monitor,
  state: &WmState,
  config: &UserConfig,
) -> String {
  if state.workspace_by_name(group_name).is_none() {
    return group_name.to_string();
  }

  let monitor_id = target_monitor.id().simple().to_string();
  let base_name = format!("{group_name}#{}", &monitor_id[..8]);

  let is_taken = |name: &str| {
    state.workspace_by_name(name).is_some()
      || config
        .value
        .workspaces
        .iter()
        .any(|config| config.name == name)
  };

  if !is_taken(&base_name) {
    return base_name;
  }

  let mut suffix = 2;
  loop {
    let name = format!("{base_name}-{suffix}");

    if !is_taken(&name) {
      return name;
    }

    suffix += 1;
  }
}

/// Gets config for the workspace to activate.
fn workspace_config(
  workspace_name: Option<&str>,
  target_monitor: Option<Monitor>,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<WorkspaceConfig> {
  let found_config = match workspace_name {
    Some(workspace_name) => config
      .inactive_workspace_configs(&state.workspaces())
      .into_iter()
      .find(|config| config.name == workspace_name)
      .with_context(|| {
        format!(
          "Workspace with name '{workspace_name}' doesn't exist or is already active."
        )
      }),
    None => target_monitor
      .and_then(|target_monitor| {
        config.workspace_config_for_monitor(
          &target_monitor,
          &state.workspaces(),
        )
      })
      .or_else(|| {
        config.next_inactive_workspace_config(&state.workspaces())
      })
      .context("No workspace config available to activate workspace."),
  };

  found_config.cloned()
}

#[cfg(test)]
mod tests {
  use wm_common::{MonitorSelector, WorkspaceConfig};

  use super::spanning_instance_config;

  fn mock_group_config() -> WorkspaceConfig {
    WorkspaceConfig {
      name: "2".to_string(),
      display_name: None,
      bind_to_monitor: Some(1),
      keep_alive: true,
      monitors: Some(MonitorSelector::All),
      spanning_group: None,
    }
  }

  #[test]
  fn derives_instance_config_from_group() {
    let config = spanning_instance_config(
      &mock_group_config(),
      "2#abc123".to_string(),
    );

    assert_eq!(config.name, "2#abc123");
    assert_eq!(config.display_name, Some("2".to_string()));
    assert_eq!(config.bind_to_monitor, None);
    assert!(config.keep_alive);
    assert_eq!(config.monitors, Some(MonitorSelector::All));
    assert_eq!(config.spanning_group, Some("2".to_string()));
  }

  #[test]
  fn instance_inherits_group_display_name() {
    let mut group_config = mock_group_config();
    group_config.display_name = Some("Work".to_string());

    let config =
      spanning_instance_config(&group_config, "2#abc123".to_string());

    assert_eq!(config.display_name, Some("Work".to_string()));
  }
}
