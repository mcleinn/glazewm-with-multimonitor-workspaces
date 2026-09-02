use std::fmt;

/// Identifies one page of a spanning ("virtual desktop") workspace group.
///
/// Page 1 is the group itself. When fewer monitors are connected than the
/// group has instances, the instances of the missing monitors are shown
/// on further pages, one instance per connected monitor.
///
/// Formats as the plain group name for page 1 (e.g. `1`) and as
/// `<group>/<page>` for further pages (e.g. `1/2`). This label is used
/// for `display_name` of instances, for recent-workspace tracking, and
/// can be passed to `focus --workspace` / `move --workspace`.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PageKey {
  pub group: String,
  pub page: usize,
}

impl PageKey {
  /// Creates the key of the group's first page.
  #[must_use]
  pub fn first(group: &str) -> Self {
    Self {
      group: group.to_string(),
      page: 1,
    }
  }

  /// Parses a page label as produced by `Display` (e.g. `1` or `1/2`).
  ///
  /// `is_group` decides whether a candidate group name is valid, so that
  /// group names containing `/` are matched as a whole first.
  ///
  /// Returns `None` if no valid group name can be found in the label.
  pub fn parse(
    label: &str,
    is_group: impl Fn(&str) -> bool,
  ) -> Option<Self> {
    if is_group(label) {
      return Some(Self::first(label));
    }

    let (group, page) = label.rsplit_once('/')?;
    let page = page.parse::<usize>().ok().filter(|page| *page >= 1)?;

    is_group(group).then(|| Self {
      group: group.to_string(),
      page,
    })
  }
}

impl fmt::Display for PageKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if self.page <= 1 {
      write!(f, "{}", self.group)
    } else {
      write!(f, "{}/{}", self.group, self.page)
    }
  }
}

#[cfg(test)]
mod tests {
  use super::PageKey;

  #[test]
  fn formats_first_page_as_group_name() {
    assert_eq!(PageKey::first("1").to_string(), "1");
    assert_eq!(
      PageKey {
        group: "1".to_string(),
        page: 3
      }
      .to_string(),
      "1/3"
    );
  }

  #[test]
  fn parses_labels() {
    let is_group = |name: &str| name == "1" || name == "a/b";

    assert_eq!(PageKey::parse("1", is_group), Some(PageKey::first("1")));
    assert_eq!(
      PageKey::parse("1/2", is_group),
      Some(PageKey {
        group: "1".to_string(),
        page: 2
      })
    );
    // Group names containing '/' take precedence over page parsing.
    assert_eq!(
      PageKey::parse("a/b", is_group),
      Some(PageKey::first("a/b"))
    );
    assert_eq!(
      PageKey::parse("a/b/2", is_group),
      Some(PageKey {
        group: "a/b".to_string(),
        page: 2
      })
    );
    assert_eq!(PageKey::parse("2", is_group), None);
    assert_eq!(PageKey::parse("1/0", is_group), None);
    assert_eq!(PageKey::parse("1/x", is_group), None);
  }
}
