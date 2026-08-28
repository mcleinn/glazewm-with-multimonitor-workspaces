use std::{collections::HashMap, env, fs, path::PathBuf};

use anyhow::{Context, Result};
use wm_common::{
  InvokeCommand, KeybindingConfig, MatchType, ParsedConfig,
  WindowMatchConfig, WindowRuleConfig, WindowRuleEvent, WorkspaceConfig,
};

use crate::{
  models::{Monitor, WindowContainer, Workspace},
  traits::{CommonGetters, WindowGetters},
};

/// Resource string for the sample config file.
///
/// This fork generates the multimonitor config (spanning workspaces via
/// `monitors: 'all'` and `initial_monitor: 'cursor'` enabled) instead of
/// the upstream sample config, since the fork's features are why it's
/// installed in the first place. `sample-config.yaml` is kept as the
/// upstream-behavior reference.
const SAMPLE_CONFIG: &str =
  include_str!("../../../resources/assets/multimonitor-config.yaml");

#[derive(Debug)]
pub struct UserConfig {
  /// Path to the user config file.
  pub path: PathBuf,

  /// Parsed user config value.
  pub value: ParsedConfig,

  /// Unparsed user config string.
  pub value_str: String,

  /// Hashmap of window rule event types (e.g. `WindowRuleEvent::Manage`)
  /// and the corresponding window rules of that type.
  window_rules_by_event: HashMap<WindowRuleEvent, Vec<WindowRuleConfig>>,
}

impl UserConfig {
  /// Creates an instance of `UserConfig`. Reads and validates the user
  /// config from the given path.
  ///
  /// Creates a new config file from sample if it doesn't exist.
  pub fn new(config_path: Option<PathBuf>) -> anyhow::Result<Self> {
    let default_config_path = home::home_dir()
      .context("Unable to get home directory.")?
      .join(".glzr/glazewm/config.yaml");

    let config_path = config_path
      .or_else(|| env::var("GLAZEWM_CONFIG_PATH").ok().map(PathBuf::from))
      .unwrap_or(default_config_path);

    let (config_value, config_str) = Self::read(&config_path)?;

    let window_rules_by_event = Self::window_rules_by_event(&config_value);

    Ok(Self {
      path: config_path,
      value: config_value,
      value_str: config_str,
      window_rules_by_event,
    })
  }

  /// Reads and validates the user config from the given path.
  ///
  /// Creates a new config file from sample if it doesn't exist.
  fn read(
    config_path: &PathBuf,
  ) -> anyhow::Result<(ParsedConfig, String)> {
    if !config_path.exists() {
      Self::create_sample(config_path)?;
    }

    let config_str = fs::read_to_string(config_path)
      .context("Unable to read config file.")?;

    // TODO: Improve error formatting of serde_yaml errors. Something
    // similar to https://github.com/AlexanderThaller/format_serde_error
    let config_value: ParsedConfig = serde_yaml::from_str(&config_str)?;

    Self::validate_workspace_configs(&config_value);

    Ok((config_value, config_str))
  }

  /// Emits non-fatal warnings for problematic workspace configs.
  fn validate_workspace_configs(config_value: &ParsedConfig) {
    for workspace_config in &config_value.workspaces {
      if workspace_config.monitors.is_some() {
        if workspace_config.bind_to_monitor.is_some() {
          tracing::warn!(
            "Workspace '{}' sets both `monitors` and `bind_to_monitor`; `monitors` takes precedence.",
            workspace_config.name
          );
        }

        if workspace_config.name.contains('#') {
          tracing::warn!(
            "Workspace '{}' contains '#', which is reserved for synthesized instances of spanning workspaces.",
            workspace_config.name
          );
        }
      }
    }
  }

  /// Initializes a new config file from the sample config resource.
  fn create_sample(config_path: &PathBuf) -> Result<()> {
    let parent_dir =
      config_path.parent().context("Invalid config path.")?;

    fs::create_dir_all(parent_dir).with_context(|| {
      format!("Unable to create directory {}.", config_path.display())
    })?;

    fs::write(config_path, SAMPLE_CONFIG).with_context(|| {
      format!("Unable to write to {}.", config_path.display())
    })?;

    Ok(())
  }

  pub fn reload(&mut self) -> anyhow::Result<()> {
    let (config_value, config_str) = Self::read(&self.path)?;

    self.window_rules_by_event =
      Self::window_rules_by_event(&config_value);
    self.value = config_value;
    self.value_str = config_str;

    Ok(())
  }

  fn default_window_rules(
    config_value: &ParsedConfig,
  ) -> Vec<WindowRuleConfig> {
    let mut window_rules = Vec::new();

    let floating_defaults =
      &config_value.window_behavior.state_defaults.floating;

    // Default float rules.
    window_rules.push(WindowRuleConfig {
      commands: vec![InvokeCommand::SetFloating {
        centered: Some(floating_defaults.centered),
        shown_on_top: Some(floating_defaults.shown_on_top),
        x_pos: None,
        y_pos: None,
        width: None,
        height: None,
      }],
      match_window: vec![
        WindowMatchConfig {
          window_class: Some(MatchType::Equals { equals:
          // W10/W11 system dialog shown when moving and deleting files.
          "OperationStatusWindow".to_string(),
        }),
          ..WindowMatchConfig::default()
        },
        WindowMatchConfig {
          window_class: Some(MatchType::Equals { equals:
          // W10/W11 system dialogs (e.g. File Explorer save/open dialog).
          "#32770".to_string(),
        }),
          ..WindowMatchConfig::default()
        },
      ],
      on: vec![WindowRuleEvent::Manage],
      run_once: true,
    });

    // Default ignore rules.
    window_rules.push(WindowRuleConfig {
      commands: vec![InvokeCommand::Ignore],
      match_window: vec![
        WindowMatchConfig {
          window_process: Some(MatchType::Equals {
            equals: "SearchApp".to_string(),
          }),
          ..WindowMatchConfig::default()
        },
        WindowMatchConfig {
          window_process: Some(MatchType::Equals {
            equals: "SearchHost".to_string(),
          }),
          ..WindowMatchConfig::default()
        },
        WindowMatchConfig {
          window_process: Some(MatchType::Equals {
            equals: "ShellExperienceHost".to_string(),
          }),
          ..WindowMatchConfig::default()
        },
        WindowMatchConfig {
          window_process: Some(MatchType::Equals {
            // W10/11 start menu.
            equals: "StartMenuExperienceHost".to_string(),
          }),
          ..WindowMatchConfig::default()
        },
        WindowMatchConfig {
          window_process: Some(MatchType::Equals {
            // W10/11 screen snipping tool.
            equals: "ScreenClippingHost".to_string(),
          }),
          ..WindowMatchConfig::default()
        },
        WindowMatchConfig {
          window_process: Some(MatchType::Equals {
            // W11 lock screen.
            equals: "LockApp".to_string(),
          }),
          ..WindowMatchConfig::default()
        },
      ],
      on: vec![WindowRuleEvent::Manage],
      run_once: true,
    });

    window_rules
  }

  fn window_rules_by_event(
    config_value: &ParsedConfig,
  ) -> HashMap<WindowRuleEvent, Vec<WindowRuleConfig>> {
    let mut window_rules_by_event = HashMap::new();

    // Combine user-defined window rules with the default ones.
    let default_window_rules = Self::default_window_rules(config_value);
    let all_window_rules = config_value
      .window_rules
      .iter()
      .chain(default_window_rules.iter());

    for window_rule in all_window_rules {
      for event_type in &window_rule.on {
        window_rules_by_event
          .entry(event_type.clone())
          .or_insert_with(Vec::new)
          .push(window_rule.clone());
      }
    }

    window_rules_by_event
  }

  /// Window rules that should be applied to the window when the given
  /// event occurs.
  pub fn pending_window_rules(
    &self,
    window: &WindowContainer,
    event: &WindowRuleEvent,
  ) -> Vec<WindowRuleConfig> {
    let window_title = window.native_properties().title;
    #[cfg(target_os = "windows")]
    let window_class = window.native_properties().class_name;
    let window_process = window.native_properties().process_name;

    let pending_window_rules = self
      .window_rules_by_event
      .get(event)
      .unwrap_or(&Vec::new())
      .iter()
      .filter(|rule| {
        // Skip if window has already ran the rule.
        if window.done_window_rules().contains(rule) {
          return false;
        }

        // Check if the window matches the rule.
        rule.match_window.iter().any(|match_config| {
          let is_process_match = match_config
            .window_process
            .as_ref()
            .is_none_or(|match_type| {
              // TODO: Temp fix for matching Zebar on both platforms with
              // the same process name. Consider using lowercase for every
              // `equals` match type.
              if window_process == "Zebar" {
                match_type.is_match("Zebar")
                  || match_type.is_match("zebar")
              } else {
                match_type.is_match(&window_process)
              }
            });

          let is_class_match = {
            #[cfg(target_os = "windows")]
            {
              match_config.window_class.as_ref().is_none_or(|match_type| {
                match_type.is_match(&window_class)
              })
            }
            #[cfg(not(target_os = "windows"))]
            {
              match_config.window_class.is_none()
            }
          };

          let is_title_match = match_config
            .window_title
            .as_ref()
            .is_none_or(|match_type| match_type.is_match(&window_title));

          is_process_match && is_class_match && is_title_match
        })
      })
      .cloned()
      .collect::<Vec<_>>();

    pending_window_rules
  }

  pub fn inactive_workspace_configs(
    &self,
    active_workspaces: &[Workspace],
  ) -> Vec<&WorkspaceConfig> {
    self
      .value
      .workspaces
      .iter()
      .filter(|config| {
        !active_workspaces
          .iter()
          .any(|workspace| workspace.config().name == config.name)
      })
      .collect()
  }

  pub fn workspace_config_for_monitor(
    &self,
    monitor: &Monitor,
    active_workspaces: &[Workspace],
  ) -> Option<&WorkspaceConfig> {
    let inactive_configs =
      self.inactive_workspace_configs(active_workspaces);

    inactive_configs.into_iter().find(|&config| {
      config
        .bind_to_monitor
        .as_ref()
        .is_some_and(|monitor_index| {
          monitor.index() == *monitor_index as usize
        })
    })
  }

  /// Gets the first inactive workspace config, prioritizing configs that
  /// don't have a monitor binding.
  pub fn next_inactive_workspace_config(
    &self,
    active_workspaces: &[Workspace],
  ) -> Option<&WorkspaceConfig> {
    let inactive_configs =
      self.inactive_workspace_configs(active_workspaces);

    inactive_configs
      .iter()
      .find(|config| config.bind_to_monitor.is_none())
      .or(inactive_configs.first())
      .copied()
  }

  /// Config for a spanning workspace group with the given name, if any.
  pub fn spanning_workspace_config(
    &self,
    workspace_name: &str,
  ) -> Option<&WorkspaceConfig> {
    self.value.workspaces.iter().find(|config| {
      config.name == workspace_name && config.monitors.is_some()
    })
  }

  pub fn workspace_config_index(
    &self,
    workspace_name: &str,
  ) -> Option<usize> {
    self
      .value
      .workspaces
      .iter()
      .position(|config| config.name == workspace_name)
  }

  pub fn sort_workspaces(&self, workspaces: &mut [Workspace]) {
    workspaces.sort_by_key(|workspace| {
      let config = workspace.config();

      // Synthesized instances of spanning workspaces aren't present in
      // the user's config; sort them by their group's config position.
      self.workspace_config_index(&config.name).or_else(|| {
        config
          .spanning_group
          .as_deref()
          .and_then(|group| self.workspace_config_index(group))
      })
    });
  }

  /// Keybinding configs that should be active for the current binding mode
  /// and pause state.
  ///
  /// When paused, only the configs with `InvokeCommand::WmTogglePause` are
  /// returned so that unpausing remains possible.
  pub fn active_keybinding_configs(
    &self,
    binding_modes: &[wm_common::BindingModeConfig],
    is_paused: bool,
  ) -> impl Iterator<Item = KeybindingConfig> {
    let source_configs = if let Some(first_mode) = binding_modes.first() {
      &first_mode.keybindings
    } else {
      &self.value.keybindings
    }
    .clone();

    source_configs.into_iter().filter(move |kb| {
      if is_paused {
        kb.commands
          .contains(&wm_common::InvokeCommand::WmTogglePause)
      } else {
        true
      }
    })
  }
}

#[cfg(test)]
mod tests {
  use std::{collections::HashMap, path::PathBuf};

  use wm_common::{MonitorSelector, ParsedConfig, WorkspaceConfig};

  use super::UserConfig;
  use crate::models::Workspace;

  fn mock_user_config(
    workspace_configs: Vec<WorkspaceConfig>,
  ) -> UserConfig {
    UserConfig {
      path: PathBuf::new(),
      value: ParsedConfig {
        workspaces: workspace_configs,
        ..ParsedConfig::default()
      },
      value_str: String::new(),
      window_rules_by_event: HashMap::new(),
    }
  }

  fn mock_workspace_config(
    name: &str,
    monitors: Option<MonitorSelector>,
  ) -> WorkspaceConfig {
    WorkspaceConfig {
      name: name.to_string(),
      display_name: None,
      bind_to_monitor: None,
      keep_alive: false,
      monitors,
      spanning_group: None,
    }
  }

  #[test]
  fn finds_spanning_workspace_config() {
    let config = mock_user_config(vec![
      mock_workspace_config("1", None),
      mock_workspace_config("2", Some(MonitorSelector::All)),
    ]);

    assert!(config.spanning_workspace_config("2").is_some());
    assert!(config.spanning_workspace_config("1").is_none());
    assert!(config.spanning_workspace_config("9").is_none());
  }

  #[test]
  fn sorts_synthesized_instances_by_group_position() {
    let config = mock_user_config(vec![
      mock_workspace_config("1", None),
      mock_workspace_config("2", Some(MonitorSelector::All)),
      mock_workspace_config("3", None),
    ]);

    let instance = Workspace::mock().name("2#abc123".to_string()).call();
    let mut instance_config = instance.config();
    instance_config.spanning_group = Some("2".to_string());
    instance.set_config(instance_config);

    let mut workspaces = vec![
      Workspace::mock().name("3".to_string()).call(),
      instance,
      Workspace::mock().name("1".to_string()).call(),
    ];

    config.sort_workspaces(&mut workspaces);

    let names = workspaces
      .iter()
      .map(|workspace| workspace.config().name)
      .collect::<Vec<_>>();

    assert_eq!(names, vec!["1", "2#abc123", "3"]);
  }
}
