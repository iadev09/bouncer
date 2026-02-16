#[cfg(target_os = "linux")]
use std::env;

use tracing_subscriber::EnvFilter;
#[cfg(target_os = "linux")]
use tracing_subscriber::layer::SubscriberExt;
#[cfg(target_os = "linux")]
use tracing_subscriber::util::SubscriberInitExt;

pub fn init_logging(
    default_filter: &str,
    env_key: &str,
    service_name: &str,
) {
    #[cfg(not(target_os = "linux"))]
    let _ = service_name;

    let env_filter = build_env_filter(default_filter, env_key);

    #[cfg(target_os = "linux")]
    {
        if is_running_under_systemd() {
            match tracing_journald::layer() {
                Ok(layer) => {
                    tracing_subscriber::registry()
                        .with(env_filter)
                        .with(layer)
                        .init();
                    return;
                }
                Err(err) => {
                    eprintln!(
                        "{service_name}: journald init failed, falling back to stderr formatter: {err}"
                    );
                }
            }
        }
    }

    tracing_subscriber::fmt().with_env_filter(env_filter).init();
}

fn build_env_filter(
    default_filter: &str,
    env_key: &str,
) -> EnvFilter {
    EnvFilter::try_from_env(env_key)
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new(default_filter))
}

#[cfg(target_os = "linux")]
#[inline]
fn is_running_under_systemd() -> bool {
    env::var_os("JOURNAL_STREAM").is_some()
        || env::var_os("INVOCATION_ID").is_some()
}
