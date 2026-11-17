//! Session log settings, kept as one JSON value in the app database under
//! [`LogSettings::KEY`]: the global default, a per-host override and the
//! retention in days.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const DEFAULT_RETENTION_DAYS: u32 = 30;
/// Longest retention the UI offers: ten years. 0 keeps logs forever.
pub const MAX_RETENTION_DAYS: u32 = 3650;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LogSettings {
    /// Record sessions of hosts that have no override. Off unless chosen.
    pub record_by_default: bool,
    /// Logs older than this many days are deleted when the daemon starts;
    /// 0 keeps them forever.
    pub retention_days: u32,
    /// Host id to an explicit on or off; a host that is absent follows the
    /// default.
    pub hosts: BTreeMap<String, bool>,
}

impl Default for LogSettings {
    fn default() -> Self {
        LogSettings {
            record_by_default: false,
            retention_days: DEFAULT_RETENTION_DAYS,
            hosts: BTreeMap::new(),
        }
    }
}

impl LogSettings {
    /// The key in the app database's settings table.
    pub const KEY: &'static str = "session_logs";

    /// Whether a new session of `host_id` (`None` for a local shell) is
    /// recorded.
    pub fn should_record(&self, host_id: Option<&str>) -> bool {
        host_id
            .and_then(|id| self.hosts.get(id).copied())
            .unwrap_or(self.record_by_default)
    }

    /// Sets or clears (`None`: follow the default) one host's override.
    pub fn set_host(&mut self, host_id: &str, record: Option<bool>) {
        match record {
            Some(value) => {
                self.hosts.insert(host_id.to_owned(), value);
            }
            None => {
                self.hosts.remove(host_id);
            }
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.retention_days > MAX_RETENTION_DAYS {
            return Err(format!(
                "retention is limited to {MAX_RETENTION_DAYS} days (0 keeps logs forever)"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_by_default_and_host_overrides_win() {
        let mut s = LogSettings::default();
        assert!(!s.should_record(Some("a")));
        assert!(!s.should_record(None));
        s.set_host("a", Some(true));
        assert!(s.should_record(Some("a")));
        assert!(!s.should_record(Some("b")));
        s.record_by_default = true;
        assert!(s.should_record(Some("b")));
        assert!(s.should_record(None));
        s.set_host("b", Some(false));
        assert!(!s.should_record(Some("b")));
        s.set_host("b", None);
        assert!(s.should_record(Some("b")));
    }

    #[test]
    fn missing_fields_take_defaults() {
        let s: LogSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, LogSettings::default());
        assert_eq!(s.retention_days, 30);
        let s: LogSettings =
            serde_json::from_str(r#"{"recordByDefault":true,"hosts":{"x":false}}"#).unwrap();
        assert!(s.record_by_default && !s.should_record(Some("x")));
    }

    #[test]
    fn retention_is_bounded() {
        let mut s = LogSettings::default();
        assert!(s.validate().is_ok());
        s.retention_days = MAX_RETENTION_DAYS + 1;
        assert!(s.validate().is_err());
    }
}
