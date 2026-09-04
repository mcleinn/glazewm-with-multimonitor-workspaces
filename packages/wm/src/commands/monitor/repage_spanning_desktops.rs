use std::cmp::Reverse;

use anyhow::Context;
use tracing::info;
use wm_common::{PageKey, WmEvent, WorkspaceConfig};

use crate::{
  commands::{
    container::{
      attach_container, detach_container, set_focused_descendant,
    },
    workspace::{
      activate_default_workspace, deactivate_workspace, sort_workspaces,
      spanning_instance_config,
    },
  },
  models::{Container, Monitor, MonitorIdentity, Orientation, Workspace},
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
/// 1). The remaining instances (screens of disconnected monitors, and
/// extra screens created on further pages) are packed onto further
/// pages, one instance per monitor and page, in a fixed order, so that a
/// group with more instances than monitors is browsed page by page
/// without gaps. Layouts are transposed to fit the orientation of the
/// monitor they end up on, and transposed back at home.
///
/// Hidden, empty instances away from their home are destroyed instead of
/// occupying a page slot. Monitors left without any workspace get a
/// default one. The previously focused container keeps focus, even if
/// its workspace moved to another monitor.
///
/// Callers are responsible for re-syncing the displayed page (e.g. via
/// `sync_monitors_to_page`) when this returns `true`.
///
/// Returns `true` if any instance was moved, renumbered, transposed or
/// destroyed, or a default workspace was activated.
pub fn repage_spanning_desktops(
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<bool> {
  let monitors = state.monitors();

  if monitors.is_empty() {
    return Ok(false);
  }

  let focused_container = state.focused_container();
  let workspaces = state.workspaces();
  let mut has_changes = false;

  for group_config in config.spanning_workspace_configs() {
    let mut instances = Vec::new();

    for workspace in &workspaces {
      let workspace_config = workspace.config();

      if workspace_config.spanning_group.as_deref()
        != Some(group_config.name.as_str())
      {
        continue;
      }

      let host =
        workspace.monitor().context("Workspace has no monitor.")?;

      // A first-page instance without a recorded home is the screen of
      // its current host (e.g. an extra screen that got packed onto the
      // first page of a monitor without a screen of its own).
      if workspace.home().is_none() && workspace_config.spanning_page <= 1
      {
        workspace.set_home(Some(MonitorIdentity::of(&host)));
      }

      let is_at_home = workspace
        .home()
        .is_some_and(|home| home.matches(&host).is_some());

      // `keep_alive` only protects instances on their home monitor.
      let is_kept_alive = workspace_config.keep_alive && is_at_home;

      let is_disposable = !workspace.has_children()
        && !workspace.is_displayed()
        && !is_kept_alive;

      if is_disposable {
        deactivate_workspace(workspace.clone(), state)?;
        has_changes = true;
      } else {
        instances.push(workspace.clone());
      }
    }

    for placement in assign_pages(&instances, &monitors) {
      has_changes |= apply_placement(&placement, group_config, state)?;
    }
  }

  for monitor in &monitors {
    if monitor.child_count() == 0 {
      activate_default_workspace(monitor, state, config)?;
      has_changes = true;
    }

    sort_workspaces(monitor, config)?;
  }

  // Moving the focused workspace to the back of another monitor's focus
  // order silently changes which container is focused. Restore focus
  // (which also displays the moved workspace on its new monitor).
  let is_still_attached = focused_container
    .as_ref()
    .and_then(CommonGetters::monitor)
    .and_then(|monitor| monitor.parent())
    .is_some();

  if let Some(focused_container) =
    focused_container.filter(|_| is_still_attached)
  {
    let has_lost_focus = state
      .focused_container()
      .is_none_or(|current| current.id() != focused_container.id());

    if has_lost_focus {
      set_focused_descendant(&focused_container, None);
      state.pending_sync.queue_focus_change();
    }
  }

  Ok(has_changes)
}

/// Computes where each instance of one spanning workspace group should
/// live, given the connected monitors (sorted by position).
///
/// 1. Instances are matched to their home monitor (strongest `HomeMatch`
///    first, ties broken in favor of the current host, then by the home's
///    position). Each monitor takes at most one instance; these form page
///    1.
/// 2. The remaining instances fill the free slots page by page, from left
///    to right: first the screens of disconnected monitors (by their
///    home's layout size and position), then instances without a home
///    (extra screens created on further pages, in their current order).
/// 3. Page numbers are compacted so that they're contiguous.
///
/// The result only depends on the instances' homes and, for instances
/// without a home, their current order, so repeated calls are stable.
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

/// Swaps horizontal and vertical tiling throughout a workspace, so that
/// a layout made for one monitor orientation fits the other.
///
/// Callers must keep `Workspace::is_transposed` in sync.
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

/// Whether the workspace's layout has to be transposed (or transposed
/// back) to fit the orientation of the given monitor.
///
/// Decided from the orientation the layout was made for and whether it's
/// currently transposed, not from the orientation of the current host:
/// instances can be parked on a monitor without being transposed (e.g.
/// when their monitor is removed).
pub fn needs_transposition(
  workspace: &Workspace,
  monitor: &Monitor,
) -> anyhow::Result<bool> {
  let should_be_transposed =
    workspace.layout_orientation() != Orientation::of(&monitor.to_rect()?);

  Ok(should_be_transposed != workspace.is_transposed())
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

/// Step 2: packs the instances that didn't match a home monitor into the
/// free slots, page by page and from left to right.
///
/// Screens of disconnected monitors come first, ordered by their home's
/// layout size and position, so that e.g. the screens of a 4-monitor
/// layout shown on 2 monitors keep their left-to-right order. Instances
/// without a home follow in their current order.
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

  let mut page = 1;

  for instance_index in orphans {
    loop {
      if free_slots.len() < page {
        free_slots.push((0..monitors.len()).collect());
      }

      if !free_slots[page - 1].is_empty() {
        let monitor_index = free_slots[page - 1].remove(0);

        placements.push(PagePlacement {
          workspace: instances[instance_index].clone(),
          monitor: monitors[monitor_index].clone(),
          page,
          is_home: false,
        });

        break;
      }

      page += 1;
    }
  }
}

/// Step 3: renumbers pages so that they're contiguous, starting at 1.
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

/// Moves an instance to its assigned monitor and page, transposing its
/// layout to fit the monitor's orientation.
///
/// Returns `true` if anything about the instance changed.
fn apply_placement(
  placement: &PagePlacement,
  group_config: &WorkspaceConfig,
  state: &mut WmState,
) -> anyhow::Result<bool> {
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

  // A layout made for a portrait monitor is a vertical stack; shown on
  // a landscape monitor it would be squashed. Flipping is its own
  // inverse, so the original layout is restored back home.
  if needs_transposition(workspace, &placement.monitor)? {
    info!("Transposing {workspace} to fit {}.", placement.monitor);

    transpose_layout(workspace);
    workspace.set_is_transposed(!workspace.is_transposed());
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
    state
      .pending_sync
      .queue_container_to_redraw(workspace.clone());

    state.emit_event(WmEvent::WorkspaceUpdated {
      updated_workspace: workspace.to_dto()?,
    });
  }

  Ok(has_changes)
}

#[cfg(test)]
mod tests {
  use std::collections::HashMap;

  use uuid::Uuid;
  use wm_common::TilingDirection;
  use wm_platform::Rect;

  use super::{assign_pages, needs_transposition, transpose_layout};
  use crate::{
    commands::container::attach_container,
    models::{
      Monitor, MonitorIdentity, Orientation, SplitContainer, TilingWindow,
      Workspace,
    },
    test_utils::mock_monitor_layout,
    traits::{CommonGetters, TilingDirectionGetters},
  };

  /// Monitor bounds of a 4-monitor layout.
  fn bounds(index: usize) -> Rect {
    let x = i32::try_from(1000 * index).expect("Index fits.");
    Rect::from_xy(x, 0, 1000, 800)
  }

  /// Mocks an instance of spanning group `1` attached to `host`, with
  /// the given home (`None` for an extra screen without a home) and
  /// page.
  fn mock_instance(
    name: &str,
    host: &Monitor,
    home: Option<&Monitor>,
    page: usize,
  ) -> Workspace {
    let workspace = Workspace::mock().name(name.to_string()).call();

    let mut config = workspace.config();
    config.spanning_group = Some("1".to_string());
    config.spanning_page = page;
    workspace.set_config(config);
    workspace.set_home(home.map(MonitorIdentity::of));

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
        mock_instance(
          &format!("s{i}"),
          &monitors[i],
          Some(&monitors[i]),
          1,
        )
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
      mock_instance("s0", &monitors[0], Some(&old_layout[0]), 1),
      mock_instance("s1", &monitors[1], Some(&old_layout[1]), 1),
      mock_instance("s2", &monitors[0], Some(&old_layout[2]), 1),
      mock_instance("s3", &monitors[0], Some(&old_layout[3]), 1),
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
      mock_instance("s0", &monitors[0], Some(&reduced_layout[0]), 1),
      mock_instance("s1", &monitors[1], Some(&reduced_layout[1]), 1),
      mock_instance("s2", &monitors[0], Some(&old_layout[2]), 2),
      mock_instance("s3", &monitors[1], Some(&old_layout[3]), 2),
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
      mock_instance("s0", &monitors[0], Some(&old_layout[0]), 1),
      mock_instance("s1", &monitors[1], Some(&old_layout[1]), 1),
      mock_instance("s2", &monitors[0], Some(&old_layout[2]), 2),
      mock_instance("s3", &monitors[1], Some(&old_layout[3]), 2),
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
      mock_instance("s3", &monitors[0], Some(&old_layout[3]), 1),
      mock_instance("s1", &monitors[1], Some(&old_layout[1]), 1),
      mock_instance("s0", &monitors[0], Some(&old_layout[0]), 1),
      mock_instance("s2", &monitors[1], Some(&old_layout[2]), 1),
    ];

    let placements = placements_by_name(&instances, &monitors);

    assert_eq!(placements["s0"], (0, 1, false));
    assert_eq!(placements["s1"], (1, 1, false));
    assert_eq!(placements["s2"], (0, 2, false));
    assert_eq!(placements["s3"], (1, 2, false));
  }

  #[test]
  fn pages_are_repacked_after_a_transient_layout() {
    let old_layout =
      mock_monitor_layout(&[bounds(0), bounds(1), bounds(2), bounds(3)]);
    let monitors = mock_monitor_layout(&[bounds(0), bounds(1)]);

    // While only monitor 0 was connected, every instance needed its own
    // page. With monitor 1 back, the pages are packed again instead of
    // keeping one half-empty page per instance.
    let instances = vec![
      mock_instance("s0", &monitors[0], Some(&old_layout[0]), 1),
      mock_instance("s1", &monitors[1], Some(&old_layout[1]), 1),
      mock_instance("s2", &monitors[0], Some(&old_layout[2]), 2),
      mock_instance("s3", &monitors[0], Some(&old_layout[3]), 3),
    ];

    let placements = placements_by_name(&instances, &monitors);

    assert_eq!(placements["s0"], (0, 1, true));
    assert_eq!(placements["s1"], (1, 1, true));
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
      mock_instance("s0", &monitors[0], Some(&old_layout[0]), 1),
      mock_instance("s1", &monitors[1], Some(&old_layout[1]), 1),
      mock_instance("s3", &monitors[1], Some(&old_layout[3]), 3),
    ];

    let placements = placements_by_name(&instances, &monitors);

    assert_eq!(placements["s3"], (0, 2, false));
  }

  #[test]
  fn conflicting_claims_prefer_the_current_host() {
    let old_layout = mock_monitor_layout(&[bounds(0), bounds(1)]);
    let monitors = mock_monitor_layout(&[bounds(0), bounds(1)]);

    // Both instances match monitor 0 by bounds equally well.
    let instances = vec![
      mock_instance("a", &monitors[1], Some(&old_layout[0]), 1),
      mock_instance("b", &monitors[0], Some(&old_layout[0]), 1),
    ];

    let placements = placements_by_name(&instances, &monitors);

    assert_eq!(placements["b"], (0, 1, true));
    assert_eq!(placements["a"], (1, 1, false));
  }

  #[test]
  fn extra_screens_never_claim_a_home_slot() {
    let monitors = mock_monitor_layout(&[bounds(0), bounds(1)]);

    // Monitor 1 came back: its own screen `s1` was parked on monitor 0,
    // while an extra screen `x` (created on a further page while monitor
    // 1 hosted it) has no home and must not take monitor 1's first page.
    let instances = vec![
      mock_instance("x", &monitors[1], None, 3),
      mock_instance("s0", &monitors[0], Some(&monitors[0]), 1),
      mock_instance("s1", &monitors[0], Some(&monitors[1]), 1),
    ];

    let placements = placements_by_name(&instances, &monitors);

    assert_eq!(placements["s0"], (0, 1, true));
    assert_eq!(placements["s1"], (1, 1, true));
    assert_eq!(placements["x"], (0, 2, false));
  }

  #[test]
  fn extra_screens_follow_screens_of_disconnected_monitors() {
    let old_layout =
      mock_monitor_layout(&[bounds(0), bounds(1), bounds(2), bounds(3)]);
    let monitors = mock_monitor_layout(&[bounds(0), bounds(1)]);

    let instances = vec![
      mock_instance("s0", &monitors[0], Some(&old_layout[0]), 1),
      mock_instance("s1", &monitors[1], Some(&old_layout[1]), 1),
      mock_instance("x", &monitors[1], None, 2),
      mock_instance("y", &monitors[0], None, 3),
      mock_instance("s3", &monitors[0], Some(&old_layout[3]), 4),
    ];

    let placements = placements_by_name(&instances, &monitors);

    assert_eq!(placements["s3"], (0, 2, false));
    assert_eq!(placements["x"], (1, 2, false));
    assert_eq!(placements["y"], (0, 3, false));
  }

  #[test]
  fn extra_screens_fill_free_first_page_slots() {
    let monitors = mock_monitor_layout(&[bounds(0), bounds(1)]);

    // A monitor without a screen of its own takes the extra screen.
    let instances = vec![
      mock_instance("s0", &monitors[0], Some(&monitors[0]), 1),
      mock_instance("x", &monitors[0], None, 2),
    ];

    let placements = placements_by_name(&instances, &monitors);

    assert_eq!(placements["s0"], (0, 1, true));
    assert_eq!(placements["x"], (1, 1, false));
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
  fn transposition_depends_on_layout_orientation_not_on_host() {
    let portrait = Rect::from_xy(-1080, 0, 1080, 1920);
    let monitors = mock_monitor_layout(&[portrait, bounds(1)]);

    // A portrait layout parked (untransposed) on a landscape monitor.
    let workspace = Workspace::mock()
      .tiling_direction(TilingDirection::Vertical)
      .layout_orientation(Orientation::Portrait)
      .call();
    attach_container(
      &workspace.clone().into(),
      &monitors[1].clone().into(),
      None,
    )
    .expect("Failed to attach workspace.");

    assert!(needs_transposition(&workspace, &monitors[1]).unwrap());
    assert!(!needs_transposition(&workspace, &monitors[0]).unwrap());

    // Once transposed, staying on the landscape monitor needs nothing,
    // while going back home needs the layout transposed back.
    workspace.set_is_transposed(true);

    assert!(!needs_transposition(&workspace, &monitors[1]).unwrap());
    assert!(needs_transposition(&workspace, &monitors[0]).unwrap());
  }

  #[test]
  fn instances_without_home_are_treated_as_orphans() {
    let monitors = mock_monitor_layout(&[bounds(0)]);

    let workspace = mock_instance("x", &monitors[0], None, 1);

    let placements = placements_by_name(&[workspace], &monitors);

    assert_eq!(placements["x"], (0, 1, false));
  }
}
