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
  /// Same pixel bounds.
  Bounds,
  /// Same display device.
  Device,
}

/// Snapshot of a monitor's identity, used to find the monitor again after
/// the monitor layout changed (e.g. a disconnect followed by a reconnect).
///
/// Device identifiers are the most reliable, but are generic within RDP
/// sessions and can change across reboots, so the pixel bounds and the
/// position within the layout serve as fallbacks.
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

    if is_same_device {
      return Some(HomeMatch::Device);
    }

    if self.bounds == properties.bounds {
      return Some(HomeMatch::Bounds);
    }

    let is_same_position = self.index == monitor.index()
      && self.monitor_count == layout_size(monitor);

    is_same_position.then_some(HomeMatch::Index)
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

  #[test]
  fn match_strength_is_ordered() {
    assert!(HomeMatch::Device > HomeMatch::Bounds);
    assert!(HomeMatch::Bounds > HomeMatch::Index);
  }
}
