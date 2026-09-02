use anyhow::Context;
use tracing::info;
use wm_common::{PageKey, TilingDirection, WmEvent, WorkspaceConfig};

use super::sort_workspaces;
use crate::{
  commands::container::attach_container,
  models::{Monitor, MonitorIdentity, Workspace},
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

  let is_spanning = workspace_config.monitors.is_some();

  if is_spanning {
    // Instances of spanning workspaces are keyed by their group name,
    // and ignore `bind_to_monitor`. A config activated by name is always
    // the group's first page.
    workspace_config.spanning_group = Some(workspace_config.name.clone());
    workspace_config.spanning_page = 1;
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

  let workspace = Workspace::new(
    workspace_config.clone(),
    config.value.gaps.clone(),
    tiling_direction_for(&target_monitor)?,
  );

  // Attach the created workspace to the specified monitor.
  attach_container(
    &workspace.clone().into(),
    &target_monitor.clone().into(),
    None,
  )?;

  if is_spanning {
    workspace.set_home(Some(MonitorIdentity::of(&target_monitor)));
  }

  sort_workspaces(&target_monitor, config)?;

  info!("Activating workspace: {workspace}");

  state.emit_event(WmEvent::WorkspaceActivated {
    activated_workspace: workspace.to_dto()?,
  });

  Ok(())
}

/// Creates (but does not display) an instance of a spanning workspace
/// page on the given monitor.
///
/// Unlike `activate_workspace`, this doesn't require the group name to be
/// inactive: each monitor gets its own instance with a globally unique
/// name derived from the page label. The monitor becomes the instance's
/// home.
///
/// Returns the newly created `Workspace`.
pub fn activate_spanning_instance(
  page_key: &PageKey,
  target_monitor: &Monitor,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<Workspace> {
  let group_config = config
    .spanning_workspace_config(&page_key.group)
    .with_context(|| {
      format!(
        "No spanning workspace config with name '{}'.",
        page_key.group
      )
    })?;

  let instance_name =
    instance_name(page_key, target_monitor, state, config);

  let workspace_config =
    spanning_instance_config(group_config, page_key, instance_name);

  let workspace = Workspace::new(
    workspace_config,
    config.value.gaps.clone(),
    tiling_direction_for(target_monitor)?,
  );

  // Attach the created workspace to the specified monitor. The workspace
  // lands at the back of the monitor's focus order, so it isn't displayed
  // until explicitly promoted.
  attach_container(
    &workspace.clone().into(),
    &target_monitor.clone().into(),
    None,
  )?;

  workspace.set_home(Some(MonitorIdentity::of(target_monitor)));

  sort_workspaces(target_monitor, config)?;

  info!("Activating spanning workspace instance: {workspace}");

  state.emit_event(WmEvent::WorkspaceActivated {
    activated_workspace: workspace.to_dto()?,
  });

  Ok(workspace)
}

/// Activates a workspace on a monitor that has none.
///
/// Prefers an instance of the spanning page that is currently focused
/// (so the monitor joins the displayed virtual desktop), then the first
/// suitable regular workspace config, and finally the first page of the
/// first spanning group.
pub fn activate_default_workspace(
  monitor: &Monitor,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let focused_page = state
    .focused_container()
    .and_then(|focused| focused.workspace())
    .and_then(|workspace| workspace.config().page_key())
    .filter(|page_key| {
      config.spanning_workspace_config(&page_key.group).is_some()
    });

  if let Some(page_key) = focused_page {
    activate_spanning_instance(&page_key, monitor, state, config)?;
    return Ok(());
  }

  let active_workspaces = state.workspaces();

  let has_inactive_config = config
    .workspace_config_for_monitor(monitor, &active_workspaces)
    .or_else(|| config.next_inactive_workspace_config(&active_workspaces))
    .is_some();

  if has_inactive_config {
    return activate_workspace(None, Some(monitor.clone()), state, config);
  }

  // Every config is active (e.g. all of them are spanning groups with
  // instances elsewhere), so fall back to a new instance of the first
  // spanning group.
  let group_config = config
    .spanning_workspace_configs()
    .next()
    .context("No workspace config available to activate workspace.")?;

  activate_spanning_instance(
    &PageKey::first(&group_config.name),
    monitor,
    state,
    config,
  )?;

  Ok(())
}

/// Derives the config for an instance of a spanning workspace page.
///
/// Every instance of a page shares the same `display_name`, so status
/// bars show the same label on all monitors: the group's display name
/// (falling back to the group name) for the first page, and
/// `<display name>/<page>` for further pages.
///
/// `keep_alive` only applies to the first page; further pages exist only
/// while they hold windows.
pub fn spanning_instance_config(
  group_config: &WorkspaceConfig,
  page_key: &PageKey,
  instance_name: String,
) -> WorkspaceConfig {
  let page = page_key.page.max(1);

  let group_display_name = group_config
    .display_name
    .clone()
    .unwrap_or_else(|| group_config.name.clone());

  let display_name = if page <= 1 {
    group_display_name
  } else {
    format!("{group_display_name}/{page}")
  };

  WorkspaceConfig {
    name: instance_name,
    display_name: Some(display_name),
    bind_to_monitor: None,
    keep_alive: group_config.keep_alive && page <= 1,
    monitors: group_config.monitors,
    spanning_group: Some(group_config.name.clone()),
    spanning_page: page,
  }
}

/// Globally unique name for a new instance of a spanning workspace page.
///
/// The first instance takes the plain page label (e.g. `1` or `1/2`);
/// further instances get a `#<monitor-id>` suffix, uniquified against
/// active workspaces and user-declared config names.
fn instance_name(
  page_key: &PageKey,
  target_monitor: &Monitor,
  state: &WmState,
  config: &UserConfig,
) -> String {
  let is_taken =
    |name: &str| {
      state.workspace_by_name(name).is_some()
        || config.value.workspaces.iter().any(|config| {
          config.name == name && config.name != page_key.group
        })
    };

  let label = page_key.to_string();

  if !is_taken(&label) {
    return label;
  }

  let monitor_id = target_monitor.id().simple().to_string();
  let base_name = format!("{label}#{}", &monitor_id[..8]);

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

/// Tiling direction for a new workspace on the given monitor (vertical
/// for portrait monitors).
fn tiling_direction_for(
  monitor: &Monitor,
) -> anyhow::Result<TilingDirection> {
  let monitor_rect = monitor.to_rect()?;

  Ok(if monitor_rect.height() > monitor_rect.width() {
    TilingDirection::Vertical
  } else {
    TilingDirection::Horizontal
  })
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
  use wm_common::{MonitorSelector, PageKey, WorkspaceConfig};

  use super::spanning_instance_config;

  fn mock_group_config() -> WorkspaceConfig {
    WorkspaceConfig {
      name: "2".to_string(),
      display_name: None,
      bind_to_monitor: Some(1),
      keep_alive: true,
      monitors: Some(MonitorSelector::All),
      spanning_group: None,
      spanning_page: 0,
    }
  }

  #[test]
  fn derives_instance_config_from_group() {
    let config = spanning_instance_config(
      &mock_group_config(),
      &PageKey::first("2"),
      "2#abc123".to_string(),
    );

    assert_eq!(config.name, "2#abc123");
    assert_eq!(config.display_name, Some("2".to_string()));
    assert_eq!(config.bind_to_monitor, None);
    assert!(config.keep_alive);
    assert_eq!(config.monitors, Some(MonitorSelector::All));
    assert_eq!(config.spanning_group, Some("2".to_string()));
    assert_eq!(config.spanning_page, 1);
    assert_eq!(config.page_key(), Some(PageKey::first("2")));
  }

  #[test]
  fn instance_inherits_group_display_name() {
    let mut group_config = mock_group_config();
    group_config.display_name = Some("Work".to_string());

    let config = spanning_instance_config(
      &group_config,
      &PageKey::first("2"),
      "2#abc123".to_string(),
    );

    assert_eq!(config.display_name, Some("Work".to_string()));
  }

  #[test]
  fn further_pages_get_page_suffix_and_no_keep_alive() {
    let page_key = PageKey {
      group: "2".to_string(),
      page: 3,
    };

    let config = spanning_instance_config(
      &mock_group_config(),
      &page_key,
      "2/3".to_string(),
    );

    assert_eq!(config.display_name, Some("2/3".to_string()));
    assert!(!config.keep_alive);
    assert_eq!(config.spanning_page, 3);
    assert_eq!(config.page_key(), Some(page_key));
  }
}
