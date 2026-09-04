use wm_platform::Rect;

use crate::{models::Monitor, traits::CommonGetters};

/// How strongly a `MonitorIdentity` matches a monitor.
///
/// Variants are ordered from weakest to strongest match.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum HomeMatch {
  /// Same left-to-right position in a layout with the same number of
  /// monitors.
  Index,
  /// Same display device, at different pixel bounds.
  Device,
  /// Same pixel bounds, on a different (or unknown) display device.
  Bounds,
  /// Same display device at the same pixel bounds.
  DeviceAndBounds,
}

/// Snapshot of a monitor's identity, used to find the monitor again after
/// the monitor layout changed (e.g. a disconnect followed by a reconnect).
///
/// A monitor that reappears with the same device identifier and bounds is
/// an unambiguous match. When the two disagree, the bounds win: RDP
/// sessions hand out generic device identifiers (`Default_Monitor`, only
/// distinguished by a UID) that are re-assigned to different monitors on
/// every reconnect, whereas a monitor's position and size in the layout
/// are stable. The position within the layout is the last resort, for
/// monitors that changed both device and resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MonitorIdentity {
  #[cfg(target_os = "windows")]
  pub device_path: Option<String>,
  #[cfg(target_os = "macos")]
  pub device_uuid: String,
  pub bounds: Rect,
  /// Left-to-right position of the monitor within its layout.
  pub index: usize,
  /// Number of monitors in the layout the snapshot was taken in.
  pub monitor_count: usize,
}

impl MonitorIdentity {
  /// Captures the identity of the given monitor within its current
  /// layout.
  pub fn of(monitor: &Monitor) -> Self {
    let properties = monitor.native_properties();

    Self {
      #[cfg(target_os = "windows")]
      device_path: properties.device_path,
      #[cfg(target_os = "macos")]
      device_uuid: properties.device_uuid,
      bounds: properties.bounds,
      index: monitor.index(),
      monitor_count: layout_size(monitor),
    }
  }

  /// Whether the given monitor is the one this identity was captured
  /// from.
  ///
  /// Returns the strongest matching criterion, or `None` if the monitor
  /// doesn't match by any criterion.
  pub fn matches(&self, monitor: &Monitor) -> Option<HomeMatch> {
    let properties = monitor.native_properties();

    let is_same_device = {
      #[cfg(target_os = "windows")]
      {
        self.device_path.is_some()
          && self.device_path == properties.device_path
      }
      #[cfg(target_os = "macos")]
      {
        self.device_uuid == properties.device_uuid
      }
    };

    let is_same_bounds = self.bounds == properties.bounds;

    let is_same_position = self.index == monitor.index()
      && self.monitor_count == layout_size(monitor);

    match (is_same_device, is_same_bounds) {
      (true, true) => Some(HomeMatch::DeviceAndBounds),
      (false, true) => Some(HomeMatch::Bounds),
      (true, false) => Some(HomeMatch::Device),
      (false, false) => is_same_position.then_some(HomeMatch::Index),
    }
  }
}

/// Number of monitors in the layout the given monitor is part of.
fn layout_size(monitor: &Monitor) -> usize {
  monitor.parent().map_or(1, |parent| parent.child_count())
}

#[cfg(test)]
mod tests {
  use wm_platform::Rect;

  use super::{HomeMatch, MonitorIdentity};
  use crate::test_utils::mock_monitor_layout as mock_layout;

  fn bounds(x: i32) -> Rect {
    Rect::from_xy(x, 0, 1000, 800)
  }

  #[test]
  fn captures_position_within_layout() {
    let monitors = mock_layout(&[bounds(0), bounds(1000), bounds(2000)]);
    let identity = MonitorIdentity::of(&monitors[1]);

    assert_eq!(identity.index, 1);
    assert_eq!(identity.monitor_count, 3);
    assert_eq!(identity.bounds, bounds(1000));
  }

  #[test]
  fn matches_by_bounds() {
    let old_layout = mock_layout(&[bounds(0), bounds(1000)]);
    let identity = MonitorIdentity::of(&old_layout[1]);

    // Same bounds at a different position and monitor count.
    let new_layout =
      mock_layout(&[bounds(-1000), bounds(0), bounds(1000)]);

    assert_eq!(identity.matches(&new_layout[2]), Some(HomeMatch::Bounds));
    assert_eq!(identity.matches(&new_layout[1]), None);
  }

  #[test]
  fn matches_by_index_only_with_same_monitor_count() {
    let old_layout = mock_layout(&[bounds(0), bounds(1000)]);
    let identity = MonitorIdentity::of(&old_layout[1]);

    // Different resolutions, same monitor count.
    let same_count = mock_layout(&[
      Rect::from_xy(0, 0, 500, 400),
      Rect::from_xy(500, 0, 500, 400),
    ]);
    assert_eq!(identity.matches(&same_count[1]), Some(HomeMatch::Index));
    assert_eq!(identity.matches(&same_count[0]), None);

    let other_count = mock_layout(&[
      Rect::from_xy(0, 0, 500, 400),
      Rect::from_xy(500, 0, 500, 400),
      Rect::from_xy(1000, 0, 500, 400),
    ]);
    assert_eq!(identity.matches(&other_count[1]), None);
  }

  #[cfg(target_os = "windows")]
  #[test]
  fn matches_by_device_path() {
    use crate::models::NativeMonitorProperties;

    let device_path = r"\\?\DISPLAY#A#UID257".to_string();

    let old_layout = mock_layout(&[bounds(0), bounds(1000)]);
    old_layout[1].set_native_properties(
      NativeMonitorProperties::mock()
        .bounds(bounds(1000))
        .device_path(device_path.clone())
        .call(),
    );
    let identity = MonitorIdentity::of(&old_layout[1]);

    let new_layout = mock_layout(&[Rect::from_xy(0, 0, 500, 400)]);
    new_layout[0].set_native_properties(
      NativeMonitorProperties::mock()
        .bounds(Rect::from_xy(0, 0, 500, 400))
        .device_path(device_path)
        .call(),
    );

    assert_eq!(identity.matches(&new_layout[0]), Some(HomeMatch::Device));

    // Missing device paths never match each other (and neither bounds
    // nor position match here).
    let unknown = mock_layout(&[
      Rect::from_xy(0, 0, 640, 480),
      Rect::from_xy(640, 0, 640, 480),
    ]);
    let identity = MonitorIdentity::of(&unknown[1]);
    assert_eq!(identity.matches(&new_layout[0]), None);
  }

  #[cfg(target_os = "windows")]
  #[test]
  fn same_device_at_same_bounds_is_the_strongest_match() {
    use crate::models::NativeMonitorProperties;

    let device_path = r"\\?\DISPLAY#A#UID257".to_string();

    let old_layout = mock_layout(&[bounds(0), bounds(1000)]);
    old_layout[1].set_native_properties(
      NativeMonitorProperties::mock()
        .bounds(bounds(1000))
        .device_path(device_path.clone())
        .call(),
    );
    let identity = MonitorIdentity::of(&old_layout[1]);

    let new_layout = mock_layout(&[bounds(0), bounds(1000)]);
    new_layout[1].set_native_properties(
      NativeMonitorProperties::mock()
        .bounds(bounds(1000))
        .device_path(device_path)
        .call(),
    );

    assert_eq!(
      identity.matches(&new_layout[1]),
      Some(HomeMatch::DeviceAndBounds)
    );
  }

  #[cfg(target_os = "windows")]
  #[test]
  fn bounds_beat_a_reassigned_device_path() {
    use crate::models::NativeMonitorProperties;

    // An RDP session re-assigns its generic device paths on reconnect:
    // the portrait monitor's old path now belongs to the wide one.
    let portrait = Rect::from_xy(-1080, 0, 1080, 1920);
    let wide = Rect::from_xy(0, 0, 3840, 1600);
    let path_a = r"\\?\DISPLAY#Default_Monitor#UID258".to_string();
    let path_b = r"\\?\DISPLAY#Default_Monitor#UID259".to_string();

    let old_layout = mock_layout(&[portrait.clone(), wide.clone()]);
    old_layout[0].set_native_properties(
      NativeMonitorProperties::mock()
        .bounds(portrait.clone())
        .device_path(path_a.clone())
        .call(),
    );
    let identity = MonitorIdentity::of(&old_layout[0]);

    let new_layout = mock_layout(&[portrait.clone(), wide.clone()]);
    new_layout[0].set_native_properties(
      NativeMonitorProperties::mock()
        .bounds(portrait)
        .device_path(path_b)
        .call(),
    );
    new_layout[1].set_native_properties(
      NativeMonitorProperties::mock()
        .bounds(wide)
        .device_path(path_a)
        .call(),
    );

    assert_eq!(identity.matches(&new_layout[0]), Some(HomeMatch::Bounds));
    assert_eq!(identity.matches(&new_layout[1]), Some(HomeMatch::Device));
    assert!(HomeMatch::Bounds > HomeMatch::Device);
  }

  #[test]
  fn match_strength_is_ordered() {
    assert!(HomeMatch::DeviceAndBounds > HomeMatch::Bounds);
    assert!(HomeMatch::Bounds > HomeMatch::Device);
    assert!(HomeMatch::Device > HomeMatch::Index);
  }
}
