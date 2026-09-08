#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error(transparent)]
  Io(#[from] std::io::Error),

  #[error(transparent)]
  #[cfg(target_os = "windows")]
  Windows(#[from] windows::core::Error),

  #[error("Accessibility operation failed for attribute {0} with error code: {1}")]
  #[cfg(target_os = "macos")]
  Accessibility(String, i32),

  #[error(transparent)]
  Parse(#[from] ParseError),

  #[error("Invalid pointer: {0}")]
  InvalidPointer(String),

  #[error("AXValue creation failed: {0}")]
  #[cfg(target_os = "macos")]
  AXValueCreation(String),

  #[error(transparent)]
  ChannelRecv(#[from] std::sync::mpsc::RecvTimeoutError),

  #[error("Channel receive error")]
  OneshotRecv(#[from] tokio::sync::oneshot::error::RecvError),

  #[error(transparent)]
  IntConversion(#[from] std::num::TryFromIntError),

  #[error("Channel send error")]
  ChannelSend,

  #[error("Display enumeration failed")]
  DisplayEnumerationFailed,

  #[error("Display mode not found")]
  DisplayModeNotFound,

  #[error("Primary display not found")]
  PrimaryDisplayNotFound,

  #[error("Not main thread")]
  NotMainThread,

  #[error("Display not found")]
  DisplayNotFound,

  #[error("Display device not found")]
  DisplayDeviceNotFound,

  #[error("Hardware enumeration failed")]
  HardwareEnumerationFailed,

  #[error("Window enumeration failed")]
  WindowEnumerationFailed,

  #[error("Window not found")]
  WindowNotFound,

  #[error("Thread error: {0}")]
  Thread(String),

  #[error("Window message error: {0}")]
  WindowMessage(String),

  #[error("Platform error: {0}")]
  Platform(String),

  #[error("Event loop has been stopped")]
  EventLoopStopped,

  #[error("Keybinding is empty")]
  InvalidKeybinding,
}

impl Error {
  /// Whether the error is the OS denying access to the target of the
  /// operation.
  ///
  /// This is what a window of a process with higher privileges than the
  /// WM's (e.g. Task Manager while the WM isn't elevated) fails with when
  /// the WM tries to move it.
  ///
  /// # Platform-specific
  ///
  /// - **macOS**: Only `std::io::Error`s are recognized.
  #[must_use]
  pub fn is_access_denied(&self) -> bool {
    match self {
      #[cfg(target_os = "windows")]
      Self::Windows(err) => {
        err.code() == windows::Win32::Foundation::E_ACCESSDENIED
      }
      Self::Io(err) => err.kind() == std::io::ErrorKind::PermissionDenied,
      _ => false,
    }
  }
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
  #[error(
    "Invalid length value '{0}': must be of format '10px' or '10%'."
  )]
  Length(String),

  #[error("Invalid keybinding: {0}")]
  Keybinding(String),

  #[error(
    "Invalid opacity value '{0}': must be of format '75%' or '0.75'."
  )]
  Opacity(String),

  #[error(
    "Invalid color '{0}': must be of format '#RRGGBB' or '#RRGGBBAA'."
  )]
  Color(String),

  #[error("Invalid delta value: {0}")]
  Delta(String),

  #[error("Invalid direction '{0}': must be one of 'left', 'right', 'up', or 'down'.")]
  Direction(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
  use super::Error;

  #[test]
  fn access_denied_io_error() {
    let denied = Error::Io(std::io::ErrorKind::PermissionDenied.into());
    assert!(denied.is_access_denied());

    let other = Error::Io(std::io::ErrorKind::NotFound.into());
    assert!(!other.is_access_denied());
  }

  #[test]
  fn access_denied_other_error() {
    assert!(!Error::WindowNotFound.is_access_denied());
  }

  #[cfg(target_os = "windows")]
  #[test]
  fn access_denied_windows_error() {
    use windows::Win32::Foundation::{E_ACCESSDENIED, E_INVALIDARG};

    let denied =
      Error::Windows(windows::core::Error::from(E_ACCESSDENIED));
    assert!(denied.is_access_denied());

    let other = Error::Windows(windows::core::Error::from(E_INVALIDARG));
    assert!(!other.is_access_denied());
  }
}
