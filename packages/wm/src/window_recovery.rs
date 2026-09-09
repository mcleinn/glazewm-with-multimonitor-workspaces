use std::path::PathBuf;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use wm_platform::{NativeWindow, NativeWindowWindowsExt};

use crate::{models::WindowContainer, traits::WindowGetters};

/// Makes a window that leaves the WM's control visible again.
///
/// A window on a hidden workspace is hidden (or cloaked) by the WM, and
/// nothing would ever show it again once the WM stops tracking it. Its
/// taskbar button is restored as well, in case it was hidden via
/// `show_all_in_taskbar`. Failures are logged rather than returned, since
/// the window is a best-effort concern of the WM by now.
pub fn show_released_window(native_window: &NativeWindow) {
  if let Err(err) = native_window.restore_visibility() {
    warn!("Failed to show released window: {err}");
  }

  if let Err(err) = native_window.set_taskbar_visibility(true) {
    warn!("Failed to show released window in taskbar: {err}");
  }
}

/// Identity of a managed window.
///
/// The process and class name verify that a recorded handle still refers
/// to the same window when it's restored, since the OS recycles handles.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WindowRecord {
  handle: isize,
  process_name: String,
  class_name: String,
}

impl WindowRecord {
  /// Whether the recorded handle still refers to the recorded window.
  fn matches(&self, native_window: &NativeWindow) -> bool {
    native_window.is_valid()
      && native_window
        .process_name()
        .is_ok_and(|name| name == self.process_name)
      && native_window
        .class_name()
        .is_ok_and(|name| name == self.class_name)
  }
}

/// Record of the windows managed by the WM, persisted across runs.
///
/// The WM hides windows on non-displayed workspaces, and the OS never
/// shows them again by itself (in particular, a cloak is never lifted).
/// The exit cleanup and the watcher process restore them, but neither
/// runs when both processes are killed at once (e.g. via Task Manager).
/// The record allows the next WM instance to restore exactly the windows
/// its predecessor left hidden, without touching the system windows that
/// are cloaked by design (e.g. the Start menu).
pub struct ManagedWindowsRecord {
  /// Path of the record file.
  path: PathBuf,

  /// Records as last written to the file, to skip redundant writes.
  last_written: Option<Vec<WindowRecord>>,
}

impl ManagedWindowsRecord {
  /// Creates the record at `~/.glzr/glazewm/managed-windows.json`.
  ///
  /// The file itself is only read or written via the other methods.
  pub fn new() -> anyhow::Result<Self> {
    let path = home::home_dir()
      .context("Unable to get home directory.")?
      .join(".glzr/glazewm/managed-windows.json");

    Ok(Self::at_path(path))
  }

  /// Creates the record at the given file path.
  fn at_path(path: PathBuf) -> Self {
    Self {
      path,
      last_written: None,
    }
  }

  /// Restores the windows that a previous WM instance recorded but didn't
  /// get to restore itself.
  ///
  /// Only windows that still exist and match their recorded identity are
  /// restored. Intended to run before the initial windows are managed, so
  /// that the restored windows are picked up as visible.
  pub fn restore_previous(&self) -> anyhow::Result<()> {
    let records = match std::fs::read_to_string(&self.path) {
      Ok(contents) => serde_json::from_str::<Vec<WindowRecord>>(&contents)
        .with_context(|| {
          format!(
            "Invalid managed windows record: {}",
            self.path.display()
          )
        })?,
      Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
        return Ok(());
      }
      Err(err) => {
        return Err(err).with_context(|| {
          format!(
            "Failed to read managed windows record: {}",
            self.path.display()
          )
        });
      }
    };

    for record in records {
      let native_window = NativeWindow::from_handle(record.handle);

      if !record.matches(&native_window) {
        continue;
      }

      info!(
        "Restoring window left hidden by a previous WM instance: \
         {} ({}, handle={}).",
        record.process_name, record.class_name, record.handle
      );

      show_released_window(&native_window);
    }

    Ok(())
  }

  /// Records the currently managed windows, if they changed since the
  /// last write.
  pub fn update(
    &mut self,
    windows: &[WindowContainer],
  ) -> anyhow::Result<()> {
    let records = windows
      .iter()
      .map(|window| {
        let properties = window.native_properties();

        WindowRecord {
          #[allow(clippy::cast_possible_wrap, clippy::unnecessary_cast)]
          handle: window.native().id().0 as isize,
          process_name: properties.process_name,
          class_name: properties.class_name,
        }
      })
      .collect::<Vec<_>>();

    self.write(records)
  }

  /// Writes the records to the file, unless they're already written.
  fn write(&mut self, records: Vec<WindowRecord>) -> anyhow::Result<()> {
    if self.last_written.as_ref() == Some(&records) {
      return Ok(());
    }

    if let Some(parent) = self.path.parent() {
      std::fs::create_dir_all(parent)?;
    }

    std::fs::write(&self.path, serde_json::to_string(&records)?)
      .with_context(|| {
        format!(
          "Failed to write managed windows record: {}",
          self.path.display()
        )
      })?;

    self.last_written = Some(records);
    Ok(())
  }

  /// Removes the record once the WM has restored its windows on exit, so
  /// that the next instance has nothing to recover.
  pub fn clear(&mut self) -> anyhow::Result<()> {
    match std::fs::remove_file(&self.path) {
      Ok(()) => {}
      Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
      Err(err) => {
        return Err(err).with_context(|| {
          format!(
            "Failed to remove managed windows record: {}",
            self.path.display()
          )
        });
      }
    }

    self.last_written = Some(Vec::new());
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Record file in a unique temporary directory.
  fn temp_record(name: &str) -> ManagedWindowsRecord {
    let dir = std::env::temp_dir()
      .join("glazewm-tests")
      .join(format!("{name}-{}", uuid::Uuid::new_v4()));

    ManagedWindowsRecord::at_path(dir.join("managed-windows.json"))
  }

  fn sample_records() -> Vec<WindowRecord> {
    vec![WindowRecord {
      handle: 1234,
      process_name: "chrome".to_string(),
      class_name: "Chrome_WidgetWin_1".to_string(),
    }]
  }

  #[test]
  fn writes_and_clears_record_file() {
    let mut record = temp_record("write");

    record.write(sample_records()).unwrap();
    let contents = std::fs::read_to_string(&record.path).unwrap();
    let parsed: Vec<WindowRecord> =
      serde_json::from_str(&contents).unwrap();
    assert_eq!(parsed, sample_records());

    record.clear().unwrap();
    assert!(!record.path.exists());
  }

  #[test]
  fn skips_unchanged_write() {
    let mut record = temp_record("unchanged");
    record.write(sample_records()).unwrap();

    // Removing the file behind the record's back reveals whether it's
    // rewritten.
    std::fs::remove_file(&record.path).unwrap();
    record.write(sample_records()).unwrap();
    assert!(!record.path.exists());

    record.write(Vec::new()).unwrap();
    assert!(record.path.exists());
  }

  #[test]
  fn restores_nothing_without_record_file() {
    let record = temp_record("missing");
    record.restore_previous().unwrap();
  }

  #[test]
  fn ignores_records_of_destroyed_windows() {
    let mut record = temp_record("destroyed");
    record.write(sample_records()).unwrap();

    // Handle `1234` doesn't refer to a live window, so nothing is
    // touched and no error is raised.
    record.restore_previous().unwrap();
  }

  #[test]
  fn rejects_invalid_record_file() {
    let record = temp_record("invalid");
    std::fs::create_dir_all(record.path.parent().unwrap()).unwrap();
    std::fs::write(&record.path, "not json").unwrap();

    assert!(record.restore_previous().is_err());
  }
}
