use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::args::JournalArgs;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalConfig {
    #[serde(default = "default_server")]
    pub server: String,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default = "default_queue_capacity")]
    pub queue_capacity: usize,
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
    #[serde(default = "default_io_timeout_secs")]
    pub io_timeout_secs: u64,
    #[serde(default = "default_heartbeat_secs")]
    pub heartbeat_secs: u64,
    #[serde(default = "default_mapping_ttl_secs")]
    pub mapping_ttl_secs: u64,
    #[serde(default = "default_unit")]
    pub unit: String,
    #[serde(default = "default_identifiers")]
    pub identifiers: Vec<String>,
    #[serde(default = "default_seek_tail")]
    pub seek_tail: bool
}

impl JournalConfig {
    pub fn load() -> Result<Self> {
        let args = JournalArgs::parse(env::args().skip(1))?;
        let config_path = args
            .config_path
            .or_else(resolve_journal_config_path)
            .context(
                "journal config path not found (JOURNAL_CONFIG_PATH or bouncer-journal.yaml/bouncer-journal.yaml)",
            )?;

        let mut config = load_config_yaml(&config_path)?;
        config.normalize()?;
        Ok(config)
    }

    fn normalize(&mut self) -> Result<()> {
        self.server = trim_owned(self.server.clone());
        self.source = trim_owned(self.source.clone());
        self.unit = trim_owned(self.unit.clone());

        if self.server.is_empty() {
            bail!("journal config missing `server`");
        }
        if self.source.is_empty() {
            self.source = default_source();
        }
        if self.unit.is_empty() {
            self.unit = default_unit();
        }

        self.identifiers = self
            .identifiers
            .iter()
            .map(|v| trim_owned(v.clone()))
            .filter(|v| !v.is_empty())
            .collect();
        if self.identifiers.is_empty() {
            self.identifiers = default_identifiers();
        }

        self.queue_capacity = self.queue_capacity.max(1);
        self.connect_timeout_secs = self.connect_timeout_secs.max(1);
        self.io_timeout_secs = self.io_timeout_secs.max(1);
        self.mapping_ttl_secs = self.mapping_ttl_secs.max(60);

        Ok(())
    }
}

fn load_config_yaml(path: &Path) -> Result<JournalConfig> {
    let raw = std::fs::read(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    serde_yaml::from_slice(&raw)
        .with_context(|| format!("failed to parse yaml {}", path.display()))
}

fn resolve_journal_config_path() -> Option<PathBuf> {
    if let Some(path) = non_empty_env("JOURNAL_CONFIG_PATH") {
        return Some(PathBuf::from(path));
    }

    if let Some(home) = home_dir() {
        let home_yaml = home.join("journal.yaml");
        if home_yaml.exists() {
            return Some(home_yaml);
        }

        let home_yml = home.join("journal.yaml");
        if home_yml.exists() {
            return Some(home_yml);
        }

        let home_observer_yaml = home.join("journal.yaml");
        if home_observer_yaml.exists() {
            return Some(home_observer_yaml);
        }
    }

    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd_yaml = cwd.join("journal.yaml");
    if cwd_yaml.exists() {
        return Some(cwd_yaml);
    }

    None
}

fn home_dir() -> Option<PathBuf> {
    non_empty_env("HOME").map(PathBuf::from)
}

fn trim_owned(value: String) -> String {
    value.trim().to_string()
}

fn non_empty_env(key: &str) -> Option<String> {
    env::var(key).ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
    })
}

fn default_server() -> String {
    "127.0.0.1:2147".to_string()
}

fn default_source() -> String {
    non_empty_env("HOSTNAME").unwrap_or_else(|| "journal".to_string())
}

fn default_queue_capacity() -> usize {
    4096
}

fn default_connect_timeout_secs() -> u64 {
    5
}

fn default_io_timeout_secs() -> u64 {
    10
}

fn default_heartbeat_secs() -> u64 {
    30
}

fn default_mapping_ttl_secs() -> u64 {
    86_400
}

fn default_unit() -> String {
    "postfix.service".to_string()
}

fn default_identifiers() -> Vec<String> {
    vec![
        "postfix/cleanup".to_string(),
        "postfix/smtp".to_string(),
        "postfix/qmgr".to_string(),
    ]
}

fn default_seek_tail() -> bool {
    true
}
