use std::time::Duration;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer};

pub fn deserialize_optional_duration<'de, D>(
    deserializer: D
) -> Result<Option<Duration>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawDuration {
        Seconds(u64),
        Text(String),
    }

    let raw = Option::<RawDuration>::deserialize(deserializer)?;
    match raw {
        None => Ok(None),
        Some(RawDuration::Seconds(secs)) => Ok(Some(Duration::from_secs(secs))),
        Some(RawDuration::Text(value)) => {
            let value = value.trim();
            if value.is_empty() {
                return Ok(None);
            }

            humantime::parse_duration(value).map(Some).map_err(D::Error::custom)
        }
    }
}

pub fn deserialize_duration<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: From<Duration> + Default,
{
    let s: Option<String> = Option::deserialize(deserializer)?;
    match s {
        Some(duration_str) => humantime::parse_duration(&duration_str)
            .map(T::from)
            .map_err(serde::de::Error::custom),
        None => Ok(T::default()),
    }
}
