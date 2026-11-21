//! The JSON Schema contract between the EasySSH daemon and the UI.
//!
//! The schema in `schema/proto.schema.json` is the source of truth. This crate
//! offers the generated Rust types ([`generated`]) and a runtime
//! [`validate`] function. It knows nothing about transports: a message is any
//! `serde_json::Value`.

use jsonschema::Validator;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;

#[rustfmt::skip]
pub mod generated;

pub use generated::*;

/// The protocol schema version. Within one version only additive changes are
/// allowed; see the README.
pub const SCHEMA_VERSION: u32 = 1;

/// The schema document, embedded at build time.
pub const SCHEMA_JSON: &str = include_str!("../schema/proto.schema.json");

/// The command set: `(kind, params schema, result schema)`.
/// An empty result schema means the reply's `ok` is `null`.
pub const COMMANDS: &[(&str, &str, &str)] = &[
    ("proto.hello", "ProtoHelloParams", "ProtoHelloResult"),
    (
        "daemon.shutdown_when_idle",
        "DaemonShutdownWhenIdleParams",
        "DaemonShutdownWhenIdleResult",
    ),
    ("hosts.list", "HostsListParams", "HostsListResult"),
    ("hosts.save", "HostsSaveParams", "HostsSaveResult"),
    ("hosts.delete", "HostsDeleteParams", ""),
    (
        "hosts.import_preview",
        "HostsImportPreviewParams",
        "HostsImportPreviewResult",
    ),
    (
        "hosts.import_apply",
        "HostsImportApplyParams",
        "HostsImportApplyResult",
    ),
    (
        "known_hosts.list",
        "KnownHostsListParams",
        "KnownHostsListResult",
    ),
    (
        "known_hosts.import",
        "KnownHostsImportParams",
        "KnownHostsImportResult",
    ),
    ("known_hosts.remove", "KnownHostsRemoveParams", ""),
    ("session.open", "SessionOpenParams", "SessionOpenResult"),
    ("session.write", "SessionWriteParams", ""),
    ("session.resize", "SessionResizeParams", ""),
    ("session.close", "SessionCloseParams", ""),
    ("session.list", "SessionListParams", "SessionListResult"),
    ("serial.ports", "SerialPortsParams", "SerialPortsResult"),
    (
        "session.attach",
        "SessionAttachParams",
        "SessionAttachResult",
    ),
    ("session.detach", "SessionDetachParams", ""),
    ("askpass.reply", "AskpassReplyParams", ""),
    (
        "askpass.endpoint",
        "AskpassEndpointParams",
        "AskpassEndpointResult",
    ),
    ("hostkey.reply", "HostkeyReplyParams", ""),
    (
        "engine.resolve",
        "EngineResolveParams",
        "EngineResolveResult",
    ),
    (
        "engine.connections",
        "EngineConnectionsParams",
        "EngineConnectionsResult",
    ),
    (
        "monitor.sample",
        "MonitorSampleParams",
        "MonitorSampleResult",
    ),
    ("monitor.signal", "MonitorSignalParams", ""),
    ("sftp.open", "SftpOpenParams", "SftpOpenResult"),
    ("sftp.list", "SftpListParams", "SftpListResult"),
    ("sftp.mkdir", "SftpMkdirParams", ""),
    ("sftp.remove", "SftpRemoveParams", ""),
    ("sftp.rename", "SftpRenameParams", ""),
    ("sftp.chmod", "SftpChmodParams", ""),
    ("sftp.close", "SftpCloseParams", ""),
    (
        "transfer.start",
        "TransferStartParams",
        "TransferStartResult",
    ),
    (
        "transfer.enqueue",
        "TransferEnqueueParams",
        "TransferEnqueueResult",
    ),
    ("transfer.cancel", "TransferCancelParams", ""),
    (
        "transfer.partial",
        "TransferPartialParams",
        "TransferPartialResult",
    ),
    (
        "transfer.discard",
        "TransferDiscardParams",
        "TransferDiscardResult",
    ),
    (
        "transfer.verify",
        "TransferVerifyParams",
        "TransferVerifyOutcome",
    ),
    ("tunnel.start", "TunnelStartParams", "TunnelStartResult"),
    ("tunnel.stop", "TunnelStopParams", ""),
    ("tunnel.list", "TunnelListParams", "TunnelListResult"),
    ("askpass.unlock", "AskpassUnlockParams", ""),
    ("askpass.bind", "AskpassBindParams", ""),
    ("askpass.unbind", "AskpassUnbindParams", ""),
    ("vault.status", "VaultStatusParams", "VaultStatus"),
    ("vault.unlock", "VaultUnlockParams", "VaultStatus"),
    ("vault.lock", "VaultLockParams", "VaultStatus"),
    ("vault.unlock_with", "VaultUnlockWithParams", "VaultStatus"),
    (
        "vault.enable_unlock",
        "VaultEnableUnlockParams",
        "VaultStatus",
    ),
    (
        "vault.disable_unlock",
        "VaultDisableUnlockParams",
        "VaultStatus",
    ),
    (
        "vault.add_unlock_item",
        "VaultAddUnlockItemParams",
        "VaultStatus",
    ),
    (
        "vault.remove_unlock_item",
        "VaultRemoveUnlockItemParams",
        "VaultStatus",
    ),
    (
        "vault.set_passphrase",
        "VaultSetPassphraseParams",
        "VaultStatus",
    ),
    (
        "vault.remove_passphrase",
        "VaultRemovePassphraseParams",
        "VaultStatus",
    ),
    (
        "vault.set_auto_lock",
        "VaultSetAutoLockParams",
        "VaultStatus",
    ),
    (
        "vault.forget_all",
        "VaultForgetAllParams",
        "VaultForgetAllResult",
    ),
    (
        "secrets.status",
        "SecretsStatusParams",
        "SecretsStatusResult",
    ),
    ("secrets.set", "SecretsSetParams", "SecretsStatusResult"),
    ("sync.status", "SyncStatusParams", "SyncStatus"),
    ("sync.signup", "SyncSignupParams", "SyncStatus"),
    ("sync.signin", "SyncSigninParams", "SyncStatus"),
    ("sync.signout", "SyncSignoutParams", "SyncStatus"),
    ("sync.now", "SyncNowParams", "SyncStatus"),
    (
        "sync.devices.list",
        "SyncDevicesListParams",
        "SyncDevicesListResult",
    ),
    ("sync.devices.revoke", "SyncDevicesRevokeParams", ""),
    ("vaults.list", "VaultsListParams", "VaultsListResult"),
    (
        "vault.create_shared",
        "VaultCreateSharedParams",
        "VaultInfo",
    ),
    (
        "vault.members.list",
        "VaultMembersListParams",
        "VaultMembersListResult",
    ),
    (
        "vault.members.lookup",
        "VaultMembersLookupParams",
        "VaultMembersLookupResult",
    ),
    ("vault.members.add", "VaultMembersAddParams", "VaultMember"),
    ("vault.members.remove", "VaultMembersRemoveParams", ""),
    ("vault.members.role", "VaultMembersRoleParams", ""),
    (
        "hosts.move_vault",
        "HostsMoveVaultParams",
        "HostsMoveVaultResult",
    ),
    ("hosts.vaults", "HostsVaultsParams", "HostsVaultsResult"),
    ("hosts.history", "HostsHistoryParams", "HostsHistoryResult"),
    ("hosts.history_restore", "HostsHistoryRestoreParams", ""),
];

/// The event set: `(kind, payload schema)`.
pub const EVENTS: &[(&str, &str)] = &[
    ("session.output", "SessionOutputEvent"),
    ("session.exit", "SessionExitEvent"),
    ("askpass.prompt", "AskpassPromptEvent"),
    ("askpass.resolved", "AskpassResolvedEvent"),
    ("hosts.changed", "HostsChangedEvent"),
    ("hostkey.prompt", "HostkeyPromptEvent"),
    ("hostkey.resolved", "HostkeyResolvedEvent"),
    ("transfer.started", "TransferStartedEvent"),
    ("transfer.progress", "TransferProgressEvent"),
    ("transfer.done", "TransferDoneEvent"),
    ("tunnel.status", "TunnelStatusEvent"),
    ("vault.changed", "VaultChangedEvent"),
    ("sync.state", "SyncStatus"),
];

fn validators() -> &'static HashMap<String, Validator> {
    static VALIDATORS: OnceLock<HashMap<String, Validator>> = OnceLock::new();
    VALIDATORS.get_or_init(|| {
        let root: Value = serde_json::from_str(SCHEMA_JSON).expect("embedded schema is JSON");
        let names: Vec<String> = root["$defs"]
            .as_object()
            .expect("schema has $defs")
            .keys()
            .cloned()
            .collect();
        names
            .into_iter()
            .map(|name| {
                // The same document with a root `$ref` to one definition, so
                // every `#/$defs/...` reference keeps resolving.
                let mut doc = root.clone();
                doc["$ref"] = Value::String(format!("#/$defs/{name}"));
                let validator = jsonschema::draft202012::options()
                    .should_validate_formats(true)
                    .build(&doc)
                    .expect("embedded schema compiles");
                (name, validator)
            })
            .collect()
    })
}

/// Validates `value` against the named definition (for example `"Connection"`
/// or `"AppConfig"`). Returns every violation as a readable string.
pub fn validate(schema_name: &str, value: &Value) -> Result<(), Vec<String>> {
    let validator = validators()
        .get(schema_name)
        .ok_or_else(|| vec![format!("unknown schema {schema_name:?}")])?;
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Validates a `Command` envelope and its payload against the schema for its
/// `kind`. A kind outside [`COMMANDS`] is an error.
pub fn validate_command(value: &Value) -> Result<(), Vec<String>> {
    validate("Command", value)?;
    let kind = value["kind"].as_str().unwrap_or_default();
    let (_, params, _) = COMMANDS
        .iter()
        .find(|(known, _, _)| *known == kind)
        .ok_or_else(|| vec![format!("unknown command kind {kind:?}")])?;
    validate(params, &value["payload"])
}

/// Validates an `Event` envelope and its payload against the schema for its
/// `kind`. A kind outside [`EVENTS`] is an error.
pub fn validate_event(value: &Value) -> Result<(), Vec<String>> {
    validate("Event", value)?;
    let kind = value["kind"].as_str().unwrap_or_default();
    let (_, payload) = EVENTS
        .iter()
        .find(|(known, _)| *known == kind)
        .ok_or_else(|| vec![format!("unknown event kind {kind:?}")])?;
    validate(payload, &value["payload"])
}
