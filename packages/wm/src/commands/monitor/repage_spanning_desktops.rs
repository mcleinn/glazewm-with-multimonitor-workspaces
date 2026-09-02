use std::cmp::Reverse;

use anyhow::Context;
use tracing::info;
use wm_common::{PageKey, WmEvent, WorkspaceConfig};

use crate::{
  commands::{
    container::{attach_container, detach_container},
    workspace::{
      activate_default_workspace, deactivate_workspace, sort_workspaces,
      spanning_instance_config,
    },
  },
  models::{Container, Monitor, MonitorIdentity, Workspace},
  traits::{
    CommonGetters, PositionGetters, TilingDirectionGetters, WindowGetters,
  },
  user_config::UserConfig,
  wm_state::WmState,
};

/// Target location of an instance of a spanning workspace group.
#[derive(Clone, Debug)]
pub struct PagePlacement {
  pub workspace: Workspace,
  pub monitor: Monitor,
  pub page: usize,
  /// Whether `monitor` is the instance's home monitor, as opposed to a
  /// monitor merely hosting it while its home is disconnected.
  pub is_home: bool,
}

/// Re-distributes the instances of every spanning workspace group over
/// the connected monitors.
///
/// Instances whose home monitor is connected are moved back to it (page
/// 1). The remaining instances (from disconnected monitors) are placed on
/// further pages, one instance per monitor and page, so that a group with
/// more instances than monitors is browsed page by page. Instances keep
/// their current slot whenever it's still available, so repeated calls
/// are stable, and page numbers are kept contiguous.
///
/// Hidden, empty instances away from their home are destroyed instead of
/// occupying a page slot. Monitors left without any workspace get a
/// default one.
///
/// Callers are responsible for queueing redraws and for re-syncing the
/// displayed page (e.g. via `sync_monitors_to_page`).
pub fn repage_spanning_desktops(
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let monitors = state.monitors();

  if monitors.is_empty() {
    return Ok(());
  }

  let workspaces = state.workspaces();

  for group_config in config.spanning_workspace_configs() {
    let mut instances = Vec::new();

    for workspace in &workspaces {
      if workspace.config().spanning_group.as_deref()
        != Some(group_config.name.as_str())
      {
        continue;
      }

      let host =
        workspace.monitor().context("Workspace has no monitor.")?;

      // Instances without a recorded home belong to their current host.
      if workspace.home().is_none() {
        workspace.set_home(Some(MonitorIdentity::of(&host)));
      }

      let is_at_home = workspace
        .home()
        .is_some_and(|home| home.matches(&host).is_some());

      // `keep_alive` only protects instances on their home monitor.
      let is_kept_alive = workspace.config().keep_alive && is_at_home;

      let is_disposable = !workspace.has_children()
        && !workspace.is_displayed()
        && !is_kept_alive;

      if is_disposable {
        deactivate_workspace(workspace.clone(), state)?;
      } else {
        instances.push(workspace.clone());
      }
    }

    for placement in assign_pages(&instances, &monitors) {
      apply_placement(&placement, group_config, state)?;
    }
  }

  for monitor in &monitors {
    if monitor.child_count() == 0 {
      activate_default_workspace(monitor, state, config)?;
    }

    sort_workspaces(monitor, config)?;
  }

  Ok(())
}

/// Computes where each instance of one spanning workspace group should
/// live, given the connected monitors (sorted by position).
///
/// 1. Instances are matched to their home monitor (strongest `HomeMatch`
///    first, ties broken in favor of the current host, then by the home's
///    position). Each monitor takes at most one instance; these form page
///    1.
/// 2. Unmatched instances keep their current monitor and page if that slot
///    is still free.
/// 3. Remaining instances fill the free slots page by page, in order of
///    their home's layout size and position.
/// 4. Page numbers are compacted so that they're contiguous.
///
/// Instances must be attached to one of `monitors`.
pub fn assign_pages(
  instances: &[Workspace],
  monitors: &[Monitor],
) -> Vec<PagePlacement> {
  if monitors.is_empty() {
    return Vec::new();
  }

  let mut placements = Vec::new();
  // Free slots per page (page 1 at index 0), as monitor indices.
  let mut free_slots = vec![(0..monitors.len()).collect::<Vec<_>>()];

  let is_placed = match_home_monitors(
    instances,
    monitors,
    &mut placements,
    &mut free_slots,
  );

  let orphans = (0..instances.len())
    .filter(|&index| !is_placed[index])
    .collect::<Vec<_>>();

  place_orphans(
    &orphans,
    instances,
    monitors,
    &mut placements,
    &mut free_slots,
  );

  compact_pages(&mut placements);

  placements
}

/// Whether the monitor is taller than wide.
fn is_portrait(monitor: &Monitor) -> anyhow::Result<bool> {
  let rect = monitor.to_rect()?;
  Ok(rect.height() > rect.width())
}

/// Swaps horizontal and vertical tiling throughout a workspace, so that
/// a layout made for one monitor orientation fits the other.
pub fn transpose_layout(workspace: &Workspace) {
  workspace.set_tiling_direction(workspace.tiling_direction().inverse());

  for descendant in workspace.descendants() {
    if let Container::Split(split_container) = descendant {
      split_container.set_tiling_direction(
        split_container.tiling_direction().inverse(),
      );
    }
  }
}

/// Position of the workspace's current monitor within `monitors`.
fn host_index(
  workspace: &Workspace,
  monitors: &[Monitor],
) -> Option<usize> {
  let host = workspace.monitor()?;
  monitors
    .iter()
    .position(|monitor| monitor.id() == host.id())
}

/// Step 1: matches instances to their home monitors, placing them on
/// page 1.
///
/// Returns which instances got placed.
fn match_home_monitors(
  instances: &[Workspace],
  monitors: &[Monitor],
  placements: &mut Vec<PagePlacement>,
  free_slots: &mut [Vec<usize>],
) -> Vec<bool> {
  let mut candidates = instances
    .iter()
    .enumerate()
    .filter_map(|(instance_index, workspace)| {
      let home = workspace.home()?;
      let host = host_index(workspace, monitors);

      let candidates = monitors
        .iter()
        .enumerate()
        .filter_map(|(monitor_index, monitor)| {
          let strength = home.matches(monitor)?;
          let is_host = host == Some(monitor_index);
          Some((
            strength,
            is_host,
            home.index,
            instance_index,
            monitor_index,
          ))
        })
        .collect::<Vec<_>>();

      Some(candidates)
    })
    .flatten()
    .collect::<Vec<_>>();

  candidates.sort_by_key(
    |&(strength, is_host, home_index, instance, _)| {
      (Reverse(strength), Reverse(is_host), home_index, instance)
    },
  );

  let mut is_placed = vec![false; instances.len()];

  for (_, _, _, instance_index, monitor_index) in candidates {
    let slot = free_slots[0].iter().position(|&m| m == monitor_index);

    if is_placed[instance_index] {
      continue;
    }

    if let Some(slot) = slot {
      free_slots[0].remove(slot);
      is_placed[instance_index] = true;

      placements.push(PagePlacement {
        workspace: instances[instance_index].clone(),
        monitor: monitors[monitor_index].clone(),
        page: 1,
        is_home: true,
      });
    }
  }

  is_placed
}

/// Steps 2 and 3: places the instances that didn't match a home monitor.
///
/// Instances keep their current slot if it's still free; the rest fill
/// the free slots page by page, ordered by their home's layout size and
/// position.
fn place_orphans(
  orphans: &[usize],
  instances: &[Workspace],
  monitors: &[Monitor],
  placements: &mut Vec<PagePlacement>,
  free_slots: &mut Vec<Vec<usize>>,
) {
  let mut orphans = orphans.to_vec();

  orphans.sort_by_key(|&index| {
    let workspace = &instances[index];
    let home = workspace.home();

    (
      Reverse(home.as_ref().map_or(0, |home| home.monitor_count)),
      home.as_ref().map_or(usize::MAX, |home| home.index),
      workspace.config().spanning_page,
      host_index(workspace, monitors).unwrap_or(usize::MAX),
    )
  });

  let place = |placements: &mut Vec<PagePlacement>,
               instance_index: usize,
               monitor_index: usize,
               page: usize| {
    placements.push(PagePlacement {
      workspace: instances[instance_index].clone(),
      monitor: monitors[monitor_index].clone(),
      page,
      is_home: false,
    });
  };

  // Step 2: keep instances in their current slot where possible.
  let mut unplaced = Vec::new();

  for instance_index in orphans {
    let workspace = &instances[instance_index];
    let page = workspace.config().spanning_page.max(1);

    while free_slots.len() < page {
      free_slots.push((0..monitors.len()).collect());
    }

    let slot = host_index(workspace, monitors).and_then(|host| {
      free_slots[page - 1].iter().position(|&m| m == host)
    });

    match slot {
      Some(slot) => {
        let monitor_index = free_slots[page - 1].remove(slot);
        place(placements, instance_index, monitor_index, page);
      }
      None => unplaced.push(instance_index),
    }
  }

  // Step 3: fill the remaining slots page by page.
  let mut page = 1;

  for instance_index in unplaced {
    loop {
      if free_slots.len() < page {
        free_slots.push((0..monitors.len()).collect());
      }

      if !free_slots[page - 1].is_empty() {
        let monitor_index = free_slots[page - 1].remove(0);
        place(placements, instance_index, monitor_index, page);
        break;
      }

      page += 1;
    }
  }
}

/// Step 4: renumbers pages so that they're contiguous, starting at 1.
fn compact_pages(placements: &mut [PagePlacement]) {
  let mut used_pages = placements
    .iter()
    .map(|placement| placement.page)
    .collect::<Vec<_>>();
  used_pages.sort_unstable();
  used_pages.dedup();

  for placement in placements {
    placement.page = used_pages
      .iter()
      .position(|&page| page == placement.page)
      .map_or(1, |position| position + 1);
  }
}

/// Moves an instance to its assigned monitor and page.
fn apply_placement(
  placement: &PagePlacement,
  group_config: &WorkspaceConfig,
  state: &mut WmState,
) -> anyhow::Result<()> {
  let workspace = &placement.workspace;
  let current_monitor = workspace.monitor().context("No monitor.")?;
  let current_config = workspace.config();
  let mut has_changes = false;

  if current_monitor.id() != placement.monitor.id() {
    info!(
      "Moving {workspace} from {current_monitor} to {} (page {}).",
      placement.monitor, placement.page
    );

    // Attach at the back of the focus order, so the instance stays
    // hidden until the page is displayed.
    detach_container(workspace.clone().into())?;
    attach_container(
      &workspace.clone().into(),
      &placement.monitor.clone().into(),
      None,
    )?;

    // A layout made for a portrait monitor is a vertical stack; shown on
    // a landscape monitor it would be squashed. Flipping is its own
    // inverse, so moving back home restores the original layout.
    if is_portrait(&current_monitor)? != is_portrait(&placement.monitor)? {
      transpose_layout(workspace);
    }

    let windows = workspace
      .descendants()
      .filter_map(|descendant| descendant.as_window_container().ok());

    for window in windows {
      window.set_has_pending_dpi_adjustment(true);

      window.set_floating_placement(
        window
          .floating_placement()
          .translate_to_center(&workspace.to_rect()?),
      );
    }

    has_changes = true;
  }

  if current_config.spanning_page != placement.page {
    let page_key = PageKey {
      group: group_config.name.clone(),
      page: placement.page,
    };

    info!("Assigning {workspace} to page {page_key}.");

    workspace.set_config(spanning_instance_config(
      group_config,
      &page_key,
      current_config.name,
    ));

    has_changes = true;
  }

  if placement.is_home {
    // Refresh the identity, so that later matches use the monitor's
    // current device path and bounds.
    workspace.set_home(Some(MonitorIdentity::of(&placement.monitor)));
  }

  if has_changes {
    state.emit_event(WmEvent::WorkspaceUpdated {
      updated_workspace: workspace.to_dto()?,
    });
  }

  Ok(())
}

#[cfg(test)]
mod tests {
  use std::collections::HashMap;

  use uuid::Uuid;
  use wm_common::TilingDirection;
  use wm_platform::Rect;

  use super::{assign_pages, transpose_layout};
  use crate::{
    commands::container::attach_container,
    models::{
      Monitor, MonitorIdentity, SplitContainer, TilingWindow, Workspace,
    },
    test_utils::mock_monitor_layout,
    traits::{CommonGetters, TilingDirectionGetters},
  };

  /// Monitor bounds of a 4-monitor layout.
  fn bounds(index: usize) -> Rect {
    let x = i32::try_from(1000 * index).expect("Index fits.");
    Rect::from_xy(x, 0, 1000, 800)
  }

  /// Mocks a spanning workspace instance attached to `host`, with the
  /// given home and page.
  fn mock_instance(
    name: &str,
    host: &Monitor,
    home: &Monitor,
    page: usize,
  ) -> Workspace {
    let workspace = Workspace::mock().name(name.to_string()).call();

    let mut config = workspace.config();
    config.spanning_group = Some("1".to_string());
    config.spanning_page = page;
    workspace.set_config(config);
    workspace.set_home(Some(MonitorIdentity::of(home)));

    attach_container(
      &workspace.clone().into(),
      &host.clone().into(),
      None,
    )
    .expect("Failed to attach workspace.");

    workspace
  }

  /// Placements keyed by workspace name: (monitor index, page, `is_home`).
  fn placements_by_name(
    instances: &[Workspace],
    monitors: &[Monitor],
  ) -> HashMap<String, (usize, usize, bool)> {
    let monitor_index = |id: Uuid| {
      monitors
        .iter()
        .position(|monitor| monitor.id() == id)
        .expect("Unknown monitor.")
    };

    assign_pages(instances, monitors)
      .into_iter()
      .map(|placement| {
        (
          placement.workspace.config().name,
          (
            monitor_index(placement.monitor.id()),
            placement.page,
            placement.is_home,
          ),
        )
      })
      .collect()
  }

  #[test]
  fn full_layout_keeps_every_instance_at_home() {
    let monitors = mock_monitor_layout(&[bounds(0), bounds(1), bounds(2)]);

    let instances = (0..3)
      .map(|i| {
        mock_instance(&format!("s{i}"), &monitors[i], &monitors[i], 1)
      })
      .collect::<Vec<_>>();

    let placements = placements_by_name(&instances, &monitors);

    assert_eq!(placements["s0"], (0, 1, true));
    assert_eq!(placements["s1"], (1, 1, true));
    assert_eq!(placements["s2"], (2, 1, true));
  }

  #[test]
  fn disconnected_monitors_form_further_pages() {
    let old_layout =
      mock_monitor_layout(&[bounds(0), bounds(1), bounds(2), bounds(3)]);
    // Monitors 2 and 3 disconnected; their instances were parked on the
    // first surviving monitor.
    let monitors = mock_monitor_layout(&[bounds(0), bounds(1)]);

    let instances = vec![
      mock_instance("s0", &monitors[0], &old_layout[0], 1),
      mock_instance("s1", &monitors[1], &old_layout[1], 1),
      mock_instance("s2", &monitors[0], &old_layout[2], 1),
      mock_instance("s3", &monitors[0], &old_layout[3], 1),
    ];

    let placements = placements_by_name(&instances, &monitors);

    assert_eq!(placements["s0"], (0, 1, true));
    assert_eq!(placements["s1"], (1, 1, true));
    assert_eq!(placements["s2"], (0, 2, false));
    assert_eq!(placements["s3"], (1, 2, false));
  }

  #[test]
  fn reconnected_monitors_take_their_instances_back() {
    let old_layout =
      mock_monitor_layout(&[bounds(0), bounds(1), bounds(2), bounds(3)]);
    let reduced_layout = mock_monitor_layout(&[bounds(0), bounds(1)]);
    let monitors =
      mock_monitor_layout(&[bounds(0), bounds(1), bounds(2), bounds(3)]);

    // Homes of the first two instances were refreshed while the layout
    // was reduced; the paged instances kept their original homes.
    let instances = vec![
      mock_instance("s0", &monitors[0], &reduced_layout[0], 1),
      mock_instance("s1", &monitors[1], &reduced_layout[1], 1),
      mock_instance("s2", &monitors[0], &old_layout[2], 2),
      mock_instance("s3", &monitors[1], &old_layout[3], 2),
    ];

    let placements = placements_by_name(&instances, &monitors);

    for (name, index) in [("s0", 0), ("s1", 1), ("s2", 2), ("s3", 3)] {
      assert_eq!(placements[name], (index, 1, true), "{name}");
    }
  }

  #[test]
  fn unknown_monitors_are_filled_by_position() {
    // Same monitor count but entirely different monitors (e.g. an RDP
    // session): instances match by position only.
    let old_layout =
      mock_monitor_layout(&[bounds(0), bounds(1), bounds(2), bounds(3)]);
    let monitors = mock_monitor_layout(&[
      Rect::from_xy(0, 0, 500, 400),
      Rect::from_xy(500, 0, 500, 400),
      Rect::from_xy(1000, 0, 500, 400),
      Rect::from_xy(1500, 0, 500, 400),
    ]);

    let instances = vec![
      mock_instance("s0", &monitors[0], &old_layout[0], 1),
      mock_instance("s1", &monitors[1], &old_layout[1], 1),
      mock_instance("s2", &monitors[0], &old_layout[2], 2),
      mock_instance("s3", &monitors[1], &old_layout[3], 2),
    ];

    let placements = placements_by_name(&instances, &monitors);

    for (name, index) in [("s0", 0), ("s1", 1), ("s2", 2), ("s3", 3)] {
      assert_eq!(placements[name], (index, 1, true), "{name}");
    }
  }

  #[test]
  fn fewer_unknown_monitors_page_by_home_position() {
    let old_layout =
      mock_monitor_layout(&[bounds(0), bounds(1), bounds(2), bounds(3)]);
    let monitors = mock_monitor_layout(&[
      Rect::from_xy(0, 0, 500, 400),
      Rect::from_xy(500, 0, 500, 400),
    ]);

    // Parked in arbitrary order on the surviving monitors.
    let instances = vec![
      mock_instance("s3", &monitors[0], &old_layout[3], 1),
      mock_instance("s1", &monitors[1], &old_layout[1], 1),
      mock_instance("s0", &monitors[0], &old_layout[0], 1),
      mock_instance("s2", &monitors[1], &old_layout[2], 1),
    ];

    let placements = placements_by_name(&instances, &monitors);

    assert_eq!(placements["s0"], (0, 1, false));
    assert_eq!(placements["s1"], (1, 1, false));
    assert_eq!(placements["s2"], (0, 2, false));
    assert_eq!(placements["s3"], (1, 2, false));
  }

  #[test]
  fn instances_keep_their_slot_when_still_free() {
    let old_layout =
      mock_monitor_layout(&[bounds(0), bounds(1), bounds(2), bounds(3)]);
    let monitors = mock_monitor_layout(&[bounds(0), bounds(1)]);

    // Page 1 has a free slot on monitor 1, but the page-2 instances stay
    // where they are (they may be displayed right now).
    let instances = vec![
      mock_instance("s0", &monitors[0], &old_layout[0], 1),
      mock_instance("s2", &monitors[0], &old_layout[2], 2),
      mock_instance("s3", &monitors[1], &old_layout[3], 2),
    ];

    let placements = placements_by_name(&instances, &monitors);

    assert_eq!(placements["s0"], (0, 1, true));
    assert_eq!(placements["s2"], (0, 2, false));
    assert_eq!(placements["s3"], (1, 2, false));
  }

  #[test]
  fn page_numbers_are_compacted() {
    let old_layout =
      mock_monitor_layout(&[bounds(0), bounds(1), bounds(2), bounds(3)]);
    let monitors = mock_monitor_layout(&[bounds(0), bounds(1)]);

    // Page 2 was emptied and destroyed; page 3 moves up.
    let instances = vec![
      mock_instance("s0", &monitors[0], &old_layout[0], 1),
      mock_instance("s1", &monitors[1], &old_layout[1], 1),
      mock_instance("s3", &monitors[1], &old_layout[3], 3),
    ];

    let placements = placements_by_name(&instances, &monitors);

    assert_eq!(placements["s3"], (1, 2, false));
  }

  #[test]
  fn conflicting_claims_prefer_the_current_host() {
    let old_layout = mock_monitor_layout(&[bounds(0), bounds(1)]);
    let monitors = mock_monitor_layout(&[bounds(0), bounds(1)]);

    // Both instances match monitor 0 by bounds equally well.
    let instances = vec![
      mock_instance("a", &monitors[1], &old_layout[0], 1),
      mock_instance("b", &monitors[0], &old_layout[0], 1),
    ];

    let placements = placements_by_name(&instances, &monitors);

    assert_eq!(placements["b"], (0, 1, true));
    assert_eq!(placements["a"], (1, 1, false));
  }

  #[test]
  fn transposing_flips_workspace_and_nested_splits() {
    let inner_split = SplitContainer::mock()
      .tiling_direction(TilingDirection::Horizontal)
      .tiling_containers(vec![
        TilingWindow::mock().call().into(),
        TilingWindow::mock().call().into(),
      ])
      .call();

    let workspace = Workspace::mock()
      .tiling_direction(TilingDirection::Vertical)
      .tiling_containers(vec![
        TilingWindow::mock().call().into(),
        inner_split.clone().into(),
      ])
      .call();

    transpose_layout(&workspace);

    assert_eq!(workspace.tiling_direction(), TilingDirection::Horizontal);
    assert_eq!(inner_split.tiling_direction(), TilingDirection::Vertical);

    // Transposing twice restores the original layout.
    transpose_layout(&workspace);

    assert_eq!(workspace.tiling_direction(), TilingDirection::Vertical);
    assert_eq!(
      inner_split.tiling_direction(),
      TilingDirection::Horizontal
    );
  }

  #[test]
  fn instances_without_home_are_treated_as_orphans() {
    let monitors = mock_monitor_layout(&[bounds(0)]);

    let workspace = Workspace::mock().name("x".to_string()).call();
    let mut config = workspace.config();
    config.spanning_group = Some("1".to_string());
    config.spanning_page = 1;
    workspace.set_config(config);
    attach_container(
      &workspace.clone().into(),
      &monitors[0].clone().into(),
      None,
    )
    .expect("Failed to attach workspace.");

    let placements = placements_by_name(&[workspace], &monitors);

    assert_eq!(placements["x"], (0, 1, false));
  }
}
