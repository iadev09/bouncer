use std::path::PathBuf;

use anyhow::{Result, bail};

#[derive(Debug, Default)]
pub struct JournalArgs {
    pub config_path: Option<PathBuf>,
}

impl JournalArgs {
    pub fn parse<I>(mut args: I) -> Result<Self>
    where
        I: Iterator<Item = String>,
    {
        let first = args.next();
        let second = args.next();

        if let Some(arg) = second {
            bail!(
                "too many arguments: {arg} (usage: bouncer-journal [config-path])"
            );
        }

        if matches!(first.as_deref(), Some("-h" | "--help")) {
            bail!("usage: bouncer-journal [config-path]");
        }

        Ok(Self { config_path: first.map(PathBuf::from) })
    }
}
