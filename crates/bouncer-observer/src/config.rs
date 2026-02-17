use std::env;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::args::ObserverArgs;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObserverConfig {
    #[serde(default = "default_listen_udp")]
    pub listen_udp: SocketAddr,
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
    pub mapping_ttl_secs: u64
}

impl ObserverConfig {
    pub fn load() -> Result<Self> {
        let args = ObserverArgs::parse(env::args().skip(1))?;
        let config_path = args
            .config_path
            .or_else(resolve_observer_config_path)
            .context(
            "observer config path not found (OBSERVER_CONFIG_PATH or observer.yaml)",
        )?;
        let mut config = load_observer_config_yaml(&config_path)?;
        config.normalize()?;
        Ok(config)
    }

    fn normalize(&mut self) -> Result<()> {
        self.server = trim_owned(self.server.clone());
        self.source = trim_owned(self.source.clone());

        if self.server.is_empty() {
            anyhow::bail!("observer config missing `server`");
        }
        if self.source.is_empty() {
            self.source = default_source();
        }

        self.queue_capacity = self.queue_capacity.max(1);
        self.connect_timeout_secs = self.connect_timeout_secs.max(1);
        self.io_timeout_secs = self.io_timeout_secs.max(1);

        Ok(())
    }
}

fn resolve_observer_config_path() -> Option<PathBuf> {
    if let Some(path) = non_empty_env("OBSERVER_CONFIG_PATH") {
        return Some(PathBuf::from(path));
    }

    if let Some(home) = home_dir() {
        let home_yaml = home.join("observer.yaml");
        if home_yaml.exists() {
            return Some(home_yaml);
        }
        let home_yml = home.join("observer.yaml");
        if home_yml.exists() {
            return Some(home_yml);
        }
    }

    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd_yaml = cwd.join("observer.yaml");
    if cwd_yaml.exists() {
        return Some(cwd_yaml);
    }
    let cwd_yml = cwd.join("observer.yaml");
    if cwd_yml.exists() {
        return Some(cwd_yml);
    }

    None
}

fn home_dir() -> Option<PathBuf> {
    non_empty_env("HOME").map(PathBuf::from)
}

fn load_observer_config_yaml(path: &Path) -> Result<ObserverConfig> {
    let raw = std::fs::read(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    serde_yaml::from_slice(&raw)
        .with_context(|| format!("failed to parse yaml {}", path.display()))
}

fn non_empty_env(key: &str) -> Option<String> {
    env::var(key).ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
    })
}

fn trim_owned(value: String) -> String {
    value.trim().to_string()
}

fn default_listen_udp() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5140)
}

fn default_server() -> String {
    "127.0.0.1:2147".to_string()
}

fn default_source() -> String {
    non_empty_env("HOSTNAME").unwrap_or_else(|| "observer".to_string())
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
