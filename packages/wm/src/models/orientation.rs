use wm_platform::Rect;

/// Orientation of a monitor (or of a layout made for one).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Orientation {
  /// Wider than tall (or square).
  Landscape,
  /// Taller than wide.
  Portrait,
}

impl Orientation {
  /// Orientation of the given bounds.
  #[must_use]
  pub fn of(rect: &Rect) -> Self {
    if rect.height() > rect.width() {
      Self::Portrait
    } else {
      Self::Landscape
    }
  }
}

#[cfg(test)]
mod tests {
  use wm_platform::Rect;

  use super::Orientation;

  #[test]
  fn classifies_bounds() {
    assert_eq!(
      Orientation::of(&Rect::from_xy(0, 0, 1000, 800)),
      Orientation::Landscape
    );
    assert_eq!(
      Orientation::of(&Rect::from_xy(0, 0, 800, 1000)),
      Orientation::Portrait
    );
    assert_eq!(
      Orientation::of(&Rect::from_xy(0, 0, 800, 800)),
      Orientation::Landscape
    );
  }
}
