use crate::transfer::Transfer;
use crate::tunnel::Tunnel;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct AppConfig {
    pub schema_version: u32,
    pub connections: Vec<Connection>,
    pub groups: Vec<Group>,
    pub recent_connection_ids: Vec<String>,
    pub snippets: Vec<CommandSnippet>,
    #[serde(default)]
    pub tunnels: Vec<Tunnel>,
    /// Defaults for every terminal tab.
    #[serde(default)]
    pub terminal: TerminalPreferences,
    #[serde(default)]
    pub sessions: Vec<SessionRecord>,
    #[serde(default)]
    pub transfer_history: Vec<Transfer>,
    #[serde(default)]
    pub workspace: Workspace,
    pub theme: Theme,
    #[serde(default)]
    pub display_density: DisplayDensity,
    pub locale: Locale,
    #[serde(default)]
    pub sidebar: SidebarPreferences,
    #[serde(default)]
    pub sync: SyncPreferences,
    #[serde(default)]
    pub experimental: ExperimentalFeatures,
    /// Which SSH engine runs new sessions. Absent means `russh`: the built-in
    /// engine is the default, and OpenSSH is an explicit choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_engine: Option<SshEngine>,
    #[serde(default)]
    pub telemetry: TelemetryPreferences,
}

/// The global SSH engine setting.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SshEngine {
    /// The system `ssh` binary. An advanced, per-installation choice kept
    /// for compatibility; it gets fixes only.
    Openssh,
    /// The built-in russh engine inside the daemon, with OpenSSH as the
    /// fallback for hosts that need features it does not support. The
    /// default, also for installs that never chose an engine.
    #[default]
    Russh,
}

/// A per-host override of the global [`SshEngine`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnginePreference {
    /// Follow the global setting.
    #[default]
    Auto,
    Russh,
    Openssh,
}

impl AppConfig {
    pub const SCHEMA_VERSION: u32 = 7;

    pub fn new() -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            theme: Theme::System,
            locale: Locale::System,
            workspace: Workspace::Home,
            ..Self::default()
        }
    }
}

/// What the user decided about crash reports and usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TelemetryPreferences {
    /// `None` until the user answered the first-run question; nothing is sent
    /// or recorded while it is `None` or `false`.
    #[serde(default)]
    pub crash_reports: Option<bool>,
    /// Anonymous usage statistics; off unless the user turns them on.
    #[serde(default)]
    pub usage_stats: bool,
}

/// Opt-in features which may expose a larger remote-data surface. They are
/// deliberately disabled for both new and migrated configurations.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExperimentalFeatures {
    /// Enables browsing remote directories and all related file operations.
    /// This stays opt-in because it exposes a larger remote-data surface.
    #[serde(default)]
    pub remote_file_browser: bool,
    pub remote_text_editing: bool,
    pub image_preview: bool,
    pub dual_pane_file_browsing: bool,
    pub git_metadata_sync_ui: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DisplayDensity {
    Compact,
    #[default]
    Comfortable,
    Large,
}

/// Local-only Git metadata sync configuration. Credentials are intentionally
/// absent: system Git and its configured credential helper own authentication.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct SyncPreferences {
    pub display_name: String,
    pub repository_path: Option<String>,
    pub remote_url: Option<String>,
    pub branch: Option<String>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub last_snapshot_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ConnectionTarget {
    Alias {
        alias: String,
    },
    Endpoint {
        hostname: String,
        username: Option<String>,
        port: u16,
    },
    /// A serial port (a router console, a microcontroller): not SSH at all.
    Serial(SerialSettings),
}

/// Parity of a serial line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SerialParity {
    #[default]
    None,
    Odd,
    Even,
}

/// Flow control of a serial line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SerialFlowControl {
    #[default]
    None,
    /// XON/XOFF.
    Software,
    /// RTS/CTS.
    Hardware,
}

/// How to open a serial port. The defaults are the usual 115200 8N1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SerialSettings {
    /// The device path: `/dev/ttyUSB0`, `/dev/cu.usbserial-1410`, `COM3`.
    pub port: String,
    pub baud: u32,
    /// 5 to 8.
    pub data_bits: u8,
    pub parity: SerialParity,
    /// 1 or 2.
    pub stop_bits: u8,
    pub flow_control: SerialFlowControl,
}

impl SerialSettings {
    pub const DEFAULT_BAUD: u32 = 115_200;

    /// 115200 8N1 without flow control on `port`.
    pub fn new(port: impl Into<String>) -> Self {
        Self {
            port: port.into(),
            baud: Self::DEFAULT_BAUD,
            data_bits: 8,
            parity: SerialParity::None,
            stop_bits: 1,
            flow_control: SerialFlowControl::None,
        }
    }
}

impl Default for ConnectionTarget {
    fn default() -> Self {
        Self::Alias {
            alias: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub target: ConnectionTarget,
    pub group_id: Option<String>,
    pub tags: Vec<String>,
    pub favorite: bool,
    pub color: Option<String>,
    pub notes: String,
    pub proxy_jump: Option<String>,
    pub local_forwards: Vec<String>,
    pub remote_forwards: Vec<String>,
    pub dynamic_forwards: Vec<String>,
    pub remote_command: Option<String>,
    pub local_directories: Vec<String>,
    pub remote_directories: Vec<String>,
    pub terminal: TerminalPreferences,
    /// Opt-in shell integration (see `shell-integration/README.md`). Absent in
    /// stored files means off.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shell_integration: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Per-host engine override; absent means `auto`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<EnginePreference>,
    /// An outbound proxy for the first hop (the target, or the first jump
    /// host). Absent means a direct connection; old data and sync payloads
    /// without it stay valid. The proxy password is a host secret, kept in
    /// the vault and never in this record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxySettings>,
}

/// The protocol of an outbound proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyKind {
    /// SOCKS5 (RFC 1928), with username and password (RFC 1929) when the host
    /// has a proxy user. The destination is sent as a name, so the proxy
    /// resolves it.
    Socks5,
    /// HTTP `CONNECT`, with basic authentication when the host has a proxy
    /// user.
    Http,
}

/// Where the outbound proxy of a host is. The password is not part of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ProxySettings {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

impl ProxySettings {
    /// The local proxy of the AnixOps Net client (its mixed listener).
    pub const ANIXOPS_NET_HOST: &'static str = "127.0.0.1";
    pub const ANIXOPS_NET_PORT: u16 = 7890;

    /// `socks5://host:port` or `http://host:port`, for messages and logs;
    /// never contains the user or the password.
    pub fn display(&self) -> String {
        let scheme = match self.kind {
            ProxyKind::Socks5 => "socks5",
            ProxyKind::Http => "http",
        };
        if self.host.contains(':') {
            format!("{scheme}://[{}]:{}", self.host, self.port)
        } else {
            format!("{scheme}://{}:{}", self.host, self.port)
        }
    }
}

impl Connection {
    /// Whether this host is a serial port rather than an SSH destination.
    pub fn is_serial(&self) -> bool {
        matches!(self.target, ConnectionTarget::Serial(_))
    }

    pub fn alias(name: impl Into<String>, alias: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            target: ConnectionTarget::Alias {
                alias: alias.into(),
            },
            group_id: None,
            tags: Vec::new(),
            favorite: false,
            color: None,
            notes: String::new(),
            proxy_jump: None,
            local_forwards: Vec::new(),
            remote_forwards: Vec::new(),
            dynamic_forwards: Vec::new(),
            remote_command: None,
            local_directories: Vec::new(),
            remote_directories: Vec::new(),
            terminal: TerminalPreferences::default(),
            shell_integration: false,
            created_at: now,
            updated_at: now,
            engine: None,
            proxy: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
}

/// Non-sensitive presentation state persisted alongside connection metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SidebarPreferences {
    pub width: f32,
    pub collapsed_group_ids: Vec<String>,
    pub selected_navigation: SidebarNavigation,
    pub selected_tag: Option<String>,
}

impl Default for SidebarPreferences {
    fn default() -> Self {
        Self {
            width: 300.0,
            collapsed_group_ids: Vec::new(),
            selected_navigation: SidebarNavigation::All,
            selected_tag: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SidebarNavigation {
    #[default]
    All,
    Favorites,
    Recent,
    OpenSshConfig,
    Ungrouped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSnippet {
    pub id: String,
    pub name: String,
    pub content: String,
}

/// A local record of an external terminal launch. It intentionally contains no
/// command line, terminal output, authentication material, or SSH diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionRecord {
    pub id: String,
    pub connection_id: Option<String>,
    pub name: String,
    pub target: String,
    pub launched_at: DateTime<Utc>,
    pub verbose: bool,
    pub launched: bool,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Workspace {
    #[default]
    Home,
    Hosts,
    Transfers,
    Keys,
    Settings,
    /// Legacy workspaces are retained solely so schema 5 metadata can be
    /// deserialized and migrated without data loss. They are not navigable.
    Files,
    Snippets,
    Forwarding,
}

impl Workspace {
    pub fn migrated(self) -> Self {
        match self {
            Self::Files | Self::Snippets | Self::Forwarding => Self::Hosts,
            workspace => workspace,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalPreferences {
    pub font_size: f32,
    pub scrollback_lines: usize,
    pub startup_command: Option<String>,
}
impl Default for TerminalPreferences {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            scrollback_lines: 10_000,
            startup_command: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    Light,
    Dark,
    #[default]
    System,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Locale {
    En,
    ZhCn,
    #[default]
    System,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_engine_is_the_default_unless_openssh_was_chosen() {
        assert_eq!(SshEngine::default(), SshEngine::Russh);
        // A configuration that never mentions an engine follows the default.
        let config: AppConfig = serde_json::from_str(
            r#"{"schema_version":7,"connections":[],"groups":[],"recent_connection_ids":[],
                "snippets":[],"tunnels":[],"workspace":"home","theme":"system","locale":"system"}"#,
        )
        .unwrap();
        assert_eq!(config.ssh_engine, None);
        assert_eq!(config.ssh_engine.unwrap_or_default(), SshEngine::Russh);
        // An explicit choice of OpenSSH is kept, and is not written when unset.
        let chosen = AppConfig {
            ssh_engine: Some(SshEngine::Openssh),
            ..AppConfig::new()
        };
        let text = serde_json::to_string(&chosen).unwrap();
        assert!(text.contains(r#""ssh_engine":"openssh""#), "{text}");
        let back: AppConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(back.ssh_engine.unwrap_or_default(), SshEngine::Openssh);
        assert!(!serde_json::to_string(&AppConfig::new())
            .unwrap()
            .contains("ssh_engine"));
    }

    #[test]
    fn a_host_without_a_proxy_reads_and_writes_as_before() {
        let mut connection = Connection::alias("old", "old");
        let text = serde_json::to_string(&connection).unwrap();
        assert!(!text.contains("\"proxy\""), "{text}");
        // A record written before proxies existed has no field at all.
        let back: Connection = serde_json::from_str(&text).unwrap();
        assert_eq!(back.proxy, None);

        connection.proxy = Some(ProxySettings {
            kind: ProxyKind::Socks5,
            host: "127.0.0.1".into(),
            port: 7890,
            username: None,
        });
        let text = serde_json::to_string(&connection).unwrap();
        assert!(
            text.contains(r#""proxy":{"kind":"socks5","host":"127.0.0.1","port":7890}"#),
            "{text}"
        );
        let back: Connection = serde_json::from_str(&text).unwrap();
        assert_eq!(back.proxy, connection.proxy);
        let http: ProxySettings =
            serde_json::from_str(r#"{"kind":"http","host":"p","port":3128,"username":"u"}"#)
                .unwrap();
        assert_eq!(http.kind, ProxyKind::Http);
        assert_eq!(http.display(), "http://p:3128");
    }

    #[test]
    fn experimental_features_default_to_off() {
        let features = ExperimentalFeatures::default();
        assert!(!features.remote_file_browser);
        assert!(!features.remote_text_editing);
        assert!(!features.image_preview);
        assert!(!features.dual_pane_file_browsing);
        assert!(!features.git_metadata_sync_ui);
    }
}
