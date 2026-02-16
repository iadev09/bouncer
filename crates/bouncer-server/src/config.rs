use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_spool")]
    pub spool: PathBuf,
    pub database_url: String,
    #[serde(default = "default_worker_concurrency")]
    pub worker_concurrency: usize,
    #[serde(default = "default_process_queue_per_worker")]
    pub process_queue_per_worker: usize,
    #[serde(default = "default_incoming_scan_secs")]
    pub incoming_scan_secs: u64,
    #[serde(default)]
    pub imap: ImapConfig,
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = parse_config_path_arg(env::args().skip(1))?
            .or_else(resolve_server_config_path)
            .context(
                "server config path not found (BOUNCER_CONFIG_PATH or bouncer.yaml/bouncer.yml)",
            )?;

        let mut config = load_config_yaml(&config_path)?;
        config.normalize()?;
        config.validate()?;
        Ok(config)
    }

    fn normalize(&mut self) -> Result<()> {
        self.listen = trim_owned(self.listen.clone());
        self.database_url = trim_owned(self.database_url.clone());

        if self.listen.is_empty() {
            self.listen = default_listen();
        }
        if self.spool.as_os_str().is_empty() {
            self.spool = default_spool();
        }
        if self.database_url.is_empty() {
            bail!("server config missing `database_url`");
        }

        self.worker_concurrency = self.worker_concurrency.max(1);
        self.process_queue_per_worker = self.process_queue_per_worker.max(1);
        self.incoming_scan_secs = self.incoming_scan_secs.max(1);
        self.imap.normalize();

        Ok(())
    }

    fn validate(&self) -> Result<()> {
        self.imap.validate()
    }
}

fn parse_config_path_arg<I>(mut args: I) -> Result<Option<PathBuf>>
where
    I: Iterator<Item = String>,
{
    let first = args.next();
    let second = args.next();

    if let Some(arg) = second {
        bail!(
            "too many arguments: {arg} (usage: bouncer-server [config-path])"
        );
    }

    if matches!(first.as_deref(), Some("-h" | "--help")) {
        bail!("usage: bouncer-server [config-path]");
    }

    Ok(first.map(PathBuf::from))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImapConfig {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default = "default_imap_port")]
    pub port: u16,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub pass: Option<String>,
    #[serde(default = "default_imap_mailbox")]
    pub mailbox: String,
    #[serde(default = "default_imap_poll_secs")]
    pub poll_secs: u64,
    #[serde(default = "default_imap_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
    #[serde(default = "default_imap_max_messages_per_poll")]
    pub max_messages_per_poll: usize,
    #[serde(
        default,
        deserialize_with = "bouncer_helpers::de::deserialize_optional_duration"
    )]
    pub max_history: Option<Duration>,
    #[serde(default)]
    pub mark_seen_if_not_exist: bool,
}

impl Default for ImapConfig {
    fn default() -> Self {
        Self {
            host: None,
            port: default_imap_port(),
            user: None,
            pass: None,
            mailbox: default_imap_mailbox(),
            poll_secs: default_imap_poll_secs(),
            connect_timeout_secs: default_imap_connect_timeout_secs(),
            max_messages_per_poll: default_imap_max_messages_per_poll(),
            max_history: None,
            mark_seen_if_not_exist: false,
        }
    }
}

impl ImapConfig {
    pub fn enabled(&self) -> bool {
        self.host.is_some()
    }

    fn normalize(&mut self) {
        self.host = normalize_opt(self.host.clone());
        self.user = normalize_opt(self.user.clone());
        self.pass = normalize_opt(self.pass.clone());
        self.mailbox = trim_owned(self.mailbox.clone());

        if self.mailbox.is_empty() {
            self.mailbox = default_imap_mailbox();
        }

        self.poll_secs = self.poll_secs.max(1);
        self.connect_timeout_secs = self.connect_timeout_secs.max(1);
        self.max_messages_per_poll = self.max_messages_per_poll.max(1);
    }

    fn validate(&self) -> Result<()> {
        if !self.enabled() {
            return Ok(());
        }

        if self.user.is_none() {
            bail!("server config imap enabled but `imap.user` is missing");
        }

        if self.pass.is_none() {
            bail!("server config imap enabled but `imap.pass` is missing");
        }

        Ok(())
    }
}

fn load_config_yaml(path: &Path) -> Result<Config> {
    let raw = std::fs::read(path).with_context(|| {
        format!("failed to read config file {}", path.display())
    })?;
    serde_yaml::from_slice(&raw).with_context(|| {
        format!("failed to parse YAML config {}", path.display())
    })
}

fn resolve_server_config_path() -> Option<PathBuf> {
    if let Some(path) = non_empty_env("BOUNCER_CONFIG_PATH") {
        return Some(PathBuf::from(path));
    }

    if let Some(home) = non_empty_env("HOME") {
        let home_yaml = PathBuf::from(&home).join("bouncer.yaml");
        if home_yaml.exists() {
            return Some(home_yaml);
        }

        let home_yml = PathBuf::from(home).join("bouncer.yml");
        if home_yml.exists() {
            return Some(home_yml);
        }
    }

    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd_yaml = cwd.join("bouncer.yaml");
    if cwd_yaml.exists() {
        return Some(cwd_yaml);
    }

    let cwd_yml = cwd.join("bouncer.yml");
    if cwd_yml.exists() {
        return Some(cwd_yml);
    }

    let legacy_path = cwd.join("Config.yaml");
    if legacy_path.exists() {
        return Some(legacy_path);
    }

    None
}

fn default_listen() -> String {
    "0.0.0.0:2147".to_string()
}

fn default_spool() -> PathBuf {
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    cwd.join("storage/spool/bouncer")
}

fn default_worker_concurrency() -> usize {
    4
}

fn default_process_queue_per_worker() -> usize {
    1024
}

fn default_incoming_scan_secs() -> u64 {
    60
}

fn default_imap_port() -> u16 {
    993
}

fn default_imap_mailbox() -> String {
    "INBOX".to_string()
}

fn default_imap_poll_secs() -> u64 {
    60
}

fn default_imap_connect_timeout_secs() -> u64 {
    10
}

fn default_imap_max_messages_per_poll() -> usize {
    200
}

fn normalize_opt(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
    })
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
