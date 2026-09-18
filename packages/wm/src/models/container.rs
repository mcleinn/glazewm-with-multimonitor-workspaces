use std::{
  cell::{Ref, RefMut},
  collections::VecDeque,
};

use ambassador::Delegate;
use enum_as_inner::EnumAsInner;
use uuid::Uuid;
use wm_common::{
  ActiveDrag, ContainerDto, DisplayState, GapsConfig, TilingDirection,
  WindowRuleConfig, WindowState,
};
use wm_platform::{Direction, NativeWindow, Rect, RectDelta};

#[allow(clippy::wildcard_imports)]
use crate::{
  models::{
    Monitor, NativeWindowProperties, NonTilingWindow, RootContainer,
    SplitContainer, TilingWindow, Workspace,
  },
  traits::*,
  user_config::UserConfig,
};

/// A container of any type.
///
/// Uses:
///
///  * [`wm_macros::SubEnum`] to define subtypes of containers.
///  * [`wm_macros::EnumFromInner`] to define conversions between the enum
///    and wrapped types.
///  * [`ambassador::Delegate`] to delegate common getters to the contained
///    types. E.g. implements [`CommonGetters`] for [Container] by
///    forwarding the call to the item contained in the enum variant.
///
/// # Example
/// Conversion between the different container types:
/// ```
/// use wm::models::{Container, DirectionContainer, SplitContainer, TilingContainer};
/// use wm::traits::{TilingSizeGetters, TilingDirectionGetters};
///
/// fn example(split: SplitContainer) {
///   // Convert a `SplitContainer` into a `Container`
///   let container: Container = split.into(); // Will be a `Container::Split`
///
///   // Could also have gone straight to a [TilingContainer] from SplitContainer
///   // let tiling: TilingContainer = split.into(); // Will be a `TilingContainer::Split`
///
///   // Try to convert a [Container] into a sub container type ([TilingContainer] in this case).
///   let tiling: TilingContainer = container.try_into().unwrap(); // Will be a `TilingContainer::Split`
///   tiling.tiling_size(); // Can use methods from the `TilingSizeGetters` trait.
///
///   // Try to convert a one sub container type into another. ([TilingContainer] to [DirectionContainer] in this case).
///   let direction: DirectionContainer = tiling.try_into().unwrap(); // Will be a `DirectionContainer::Split`
///   direction.tiling_direction(); // Can use methods from the `TilingDirectionGetters` trait.
///
///   // Convert a sub container back into a [Container]
///   let container: Container = direction.into(); // Will be a `Container::Split`
/// }
/// ```
#[derive(
  Clone,
  Debug,
  EnumAsInner,
  wm_macros::EnumFromInner,
  Delegate,
  wm_macros::SubEnum,
)]
#[delegate(CommonGetters)]
#[delegate(PositionGetters)]
#[subenum(defaults, {
  /// Subenum of [Container]
  #[derive(Clone, Debug, EnumAsInner, Delegate, wm_macros::EnumFromInner)]
  #[delegate(CommonGetters)]
  #[delegate(PositionGetters)]
})]
#[subenum(TilingContainer, {
  /// Subset of containers that implement the following traits:
  /// * `CommonGetters`
  /// * `PositionGetters`
  /// * `TilingSizeGetters`
  #[delegate(TilingSizeGetters)]
})]
#[subenum(WindowContainer, {
  /// Subset of containers that implement the following traits:
  /// * `CommonGetters`
  /// * `PositionGetters`
  /// * `WindowGetters`
  #[delegate(WindowGetters)]
})]
#[subenum(DirectionContainer, {
  /// Subset of containers that implement the following traits:
  /// * `CommonGetters`
  /// * `PositionGetters`
  /// * `DirectionGetters`
  #[delegate(TilingDirectionGetters)]
})]
pub enum Container {
  Root(RootContainer),
  Monitor(Monitor),
  #[subenum(DirectionContainer)]
  Workspace(Workspace),
  #[subenum(TilingContainer, DirectionContainer)]
  Split(SplitContainer),
  #[subenum(TilingContainer, WindowContainer)]
  TilingWindow(TilingWindow),
  #[subenum(WindowContainer)]
  NonTilingWindow(NonTilingWindow),
}

impl Container {
  /// Gets the window that should have OS focus while this container is
  /// the WM's focused container.
  ///
  /// Returns `None` if the desktop window should have focus instead. That
  /// is the case for a workspace, and for a minimized window: a workspace
  /// whose windows are all minimized resolves its focus to one of them.
  /// Giving OS focus to a minimized window would make
  /// `handle_window_focused` restore it.
  pub fn native_focus_target(&self) -> Option<WindowContainer> {
    self
      .as_window_container()
      .ok()
      .filter(|window| window.state() != WindowState::Minimized)
  }
}

impl PartialEq for Container {
  fn eq(&self, other: &Self) -> bool {
    self.id() == other.id()
  }
}

impl Eq for Container {}

impl PartialEq for TilingContainer {
  fn eq(&self, other: &Self) -> bool {
    self.id() == other.id()
  }
}

impl Eq for TilingContainer {}

impl PartialEq for WindowContainer {
  fn eq(&self, other: &Self) -> bool {
    self.id() == other.id()
  }
}

impl Eq for WindowContainer {}

impl std::fmt::Display for WindowContainer {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    // Truncate title if longer than 20 chars. Need to use `chars()`
    // instead of byte slices to handle invalid byte indices.
    let title = {
      let title = self.native_properties().title;
      if title.len() > 20 {
        format!("{}...", title.chars().take(17).collect::<String>())
      } else {
        title
      }
    };

    let class = {
      #[cfg(target_os = "windows")]
      {
        self.native_properties().class_name
      }
      #[cfg(not(target_os = "windows"))]
      {
        String::new()
      }
    };

    let process = self.native_properties().process_name;

    write!(
      f,
      "Window(id={:?}, process={}, class={}, title={})",
      self.native().id(),
      process,
      class,
      title,
    )?;

    Ok(())
  }
}

impl PartialEq for DirectionContainer {
  fn eq(&self, other: &Self) -> bool {
    self.id() == other.id()
  }
}

impl Eq for DirectionContainer {}

/// Implements the `Debug` trait for a given container struct.
///
/// Expects that the struct has a `to_dto()` method.
#[macro_export]
macro_rules! impl_container_debug {
  ($type:ty) => {
    impl std::fmt::Debug for $type {
      fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        std::fmt::Debug::fmt(
          &self.to_dto().map_err(|_| std::fmt::Error),
          f,
        )
      }
    }
  };
}

#[cfg(test)]
mod tests {
  use wm_common::{
    FloatingStateConfig, FullscreenStateConfig, WindowState,
  };

  use crate::{
    models::{Container, NonTilingWindow, TilingWindow, Workspace},
    traits::CommonGetters,
  };

  #[test]
  fn native_focus_target_is_the_window() {
    let tiling: Container = TilingWindow::mock().call().into();
    assert_eq!(
      tiling.native_focus_target().map(|window| window.id()),
      Some(tiling.id())
    );

    for state in [
      WindowState::Floating(FloatingStateConfig::default()),
      WindowState::Fullscreen(FullscreenStateConfig::default()),
    ] {
      let window: Container =
        NonTilingWindow::mock().state(state).call().into();

      assert_eq!(
        window.native_focus_target().map(|window| window.id()),
        Some(window.id())
      );
    }
  }

  #[test]
  fn native_focus_target_is_the_desktop_for_minimized_windows() {
    let window: Container = NonTilingWindow::mock()
      .state(WindowState::Minimized)
      .call()
      .into();

    assert!(window.native_focus_target().is_none());
  }

  #[test]
  fn native_focus_target_is_the_desktop_for_workspaces() {
    let workspace: Container = Workspace::mock().call().into();

    assert!(workspace.native_focus_target().is_none());
  }
}
