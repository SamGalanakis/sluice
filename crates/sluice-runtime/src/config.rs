//! The home's `config.json` (SPEC §2.3). Read on each use, so an edit takes effect without a
//! restart. A key with a value of the wrong shape is ignored with a warning and its default
//! applies; a missing or unreadable file means every default.
use serde_json::Value;
use std::{path::Path, time::Duration};

/// `log_max` when config.json does not set it.
pub const DEFAULT_LOG_MAX: usize = 10_000;
/// The port `serve` binds when neither `--port` nor config.json's `http.port` gives one.
pub const DEFAULT_PORT: u16 = 3065;
/// The host `serve` binds when neither `--host` nor config.json's `http.host` gives one.
pub const DEFAULT_HOST: &str = "127.0.0.1";
/// `notify.timeout_s` when config.json does not set it.
pub const DEFAULT_NOTIFY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
pub struct NotifyConfig {
    /// argv, run without a shell.
    pub command: Vec<String>,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HomeConfig {
    /// `http.host`, when set.
    pub host: Option<String>,
    /// `http.port`, when set.
    pub port: Option<u16>,
    /// Records kept per log before a trim; a trim keeps the newest 90%.
    pub log_max: usize,
    /// Minutes an unread settlement may wait before the owner is asked about it.
    pub unread_alert_min: Option<f64>,
    pub notify: Option<NotifyConfig>,
}

impl Default for HomeConfig {
    fn default() -> Self {
        Self {
            host: None,
            port: None,
            log_max: DEFAULT_LOG_MAX,
            unread_alert_min: None,
            notify: None,
        }
    }
}

impl HomeConfig {
    pub fn load(home: &Path) -> Self {
        let path = home.join("config.json");
        let value = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(path=%path.display(), %error, "config.json is not JSON; using defaults");
                    return Self::default();
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(error) => {
                tracing::warn!(path=%path.display(), %error, "cannot read config.json; using defaults");
                return Self::default();
            }
        };
        Self::from_value(&value)
    }

    pub fn from_value(value: &Value) -> Self {
        let mut config = Self::default();
        let ignored = |key: &str| tracing::warn!(key, "config.json value ignored: wrong shape");
        match value.get("http") {
            None | Some(Value::Null) => {}
            Some(Value::Object(http)) => {
                match http.get("host") {
                    None | Some(Value::Null) => {}
                    Some(Value::String(host)) if !host.trim().is_empty() => {
                        config.host = Some(host.clone())
                    }
                    Some(_) => ignored("http.host"),
                }
                match http.get("port") {
                    None | Some(Value::Null) => {}
                    Some(port) => match port.as_u64().and_then(|p| u16::try_from(p).ok()) {
                        Some(port) if port > 0 => config.port = Some(port),
                        _ => ignored("http.port"),
                    },
                }
            }
            Some(_) => ignored("http"),
        }
        match value.get("log_max") {
            None | Some(Value::Null) => {}
            Some(n) => match n.as_u64().and_then(|n| usize::try_from(n).ok()) {
                Some(n) if n > 0 => config.log_max = n,
                _ => ignored("log_max"),
            },
        }
        match value.get("unread_alert_min") {
            None | Some(Value::Null) => {}
            Some(n) => match n.as_f64() {
                Some(n) if n.is_finite() && n > 0.0 => config.unread_alert_min = Some(n),
                _ => ignored("unread_alert_min"),
            },
        }
        match value.get("notify") {
            None | Some(Value::Null) => {}
            Some(Value::Object(notify)) => {
                let command = notify
                    .get("command")
                    .and_then(Value::as_array)
                    .and_then(|argv| {
                        argv.iter()
                            .map(|a| a.as_str().map(str::to_owned))
                            .collect::<Option<Vec<_>>>()
                    });
                let timeout = match notify.get("timeout_s") {
                    None | Some(Value::Null) => Some(DEFAULT_NOTIFY_TIMEOUT),
                    Some(n) => n
                        .as_f64()
                        .filter(|n| n.is_finite() && *n > 0.0)
                        .and_then(|n| Duration::try_from_secs_f64(n).ok()),
                };
                match (command, timeout) {
                    (Some(command), Some(timeout))
                        if command.first().is_some_and(|c| !c.is_empty()) =>
                    {
                        config.notify = Some(NotifyConfig { command, timeout })
                    }
                    _ => ignored("notify"),
                }
            }
            Some(_) => ignored("notify"),
        }
        config
    }

    /// The log trim target: 90% of `log_max`, at least one record.
    pub fn log_keep(&self) -> usize {
        (self.log_max - self.log_max / 10).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_every_key_and_falls_back_on_bad_shapes() {
        let config = HomeConfig::from_value(&json!({
            "fn_dirs": [],
            "http": {"host": "localhost", "port": 7420},
            "log_max": 50,
            "unread_alert_min": 1.5,
            "notify": {"command": ["notify-send", "sluice"], "timeout_s": 2},
        }));
        assert_eq!(config.host.as_deref(), Some("localhost"));
        assert_eq!(config.port, Some(7420));
        assert_eq!((config.log_max, config.log_keep()), (50, 45));
        assert_eq!(config.unread_alert_min, Some(1.5));
        assert_eq!(
            config.notify,
            Some(NotifyConfig {
                command: vec!["notify-send".into(), "sluice".into()],
                timeout: Duration::from_secs(2),
            })
        );
        let bad = HomeConfig::from_value(&json!({
            "http": {"port": 70000},
            "log_max": 0,
            "unread_alert_min": -1,
            "notify": {"command": "notify-send"},
        }));
        assert_eq!(bad, HomeConfig::default());
        assert_eq!(HomeConfig::from_value(&json!({"log_max": 1})).log_keep(), 1);
        assert_eq!(
            HomeConfig::from_value(&json!({"notify": {"command": ["x"]}}))
                .notify
                .unwrap()
                .timeout,
            DEFAULT_NOTIFY_TIMEOUT
        );
    }
}
