//! JumpServer: the assets (hosts) of an organization.
//!
//! `GET {url}/api/v1/assets/hosts/?limit=100&offset=N` signed with an access
//! key (HTTP Signature, draft-cavage, `hmac-sha256` over
//! `(request-target) accept date`, `keyId` = the access key id) plus
//! `X-JMS-ORG`. This is the request AnixOps' fleet tool (`scripts/jms.py` in
//! anixops-ansible) makes. The reply is a page `{"count","next","results"}` or
//! a bare list. Asset fields used: `id`, `name`, `address`, `is_active`,
//! `platform` (an object or a name), `protocols` (`[{"name":"ssh","port":22}]`)
//! and `nodes_display` (full node paths such as `/Default/Prod/Web`).
//!
//! # Gateway mode
//!
//! With the gateway option (the default), every imported host points at the
//! JumpServer KoKo SSH gateway (`host:2222`). KoKo's direct asset login puts
//! the asset in the SSH user name:
//! `ssh -p2222 <jms_user>@<account>@<asset_address>@<gateway>`, so the host's
//! user is `jms_user@account@asset_address` (the address is unambiguous; KoKo
//! also accepts the asset name). The account is the form's "default user"
//! (asset account) field. Very old JumpServer releases may use another form.
//! With the account empty the user is just `jms_user` and the asset is chosen
//! in KoKo's menu (`/` and part of the name, Enter). The JumpServer password
//! and MFA are not stored here; KoKo asks for them as keyboard-interactive
//! prompts, which the app's prompt dialog already shows. See [`gateway_login`].

use crate::error::{Result, ServiceError};
use crate::http::{self, BaseUrl};
use crate::{Fetched, ServiceForm};
use base64::{engine::general_purpose::STANDARD, Engine};
use easyssh_store::import::ImportHost;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::Value;
use sha2::Sha256;

/// The organization used when the form names none (JumpServer's "Default").
pub const DEFAULT_ORG: &str = "00000000-0000-0000-0000-000000000002";
const PAGE_SIZE: usize = 100;
const MAX_PAGES: usize = 50;
const SIGNED_HEADERS: &str = "(request-target) accept date";

type HmacSha256 = Hmac<Sha256>;

/// The `signature` value: base64 of HMAC-SHA256 over the three signed lines.
/// `target` is the path and query exactly as sent.
pub fn signature(secret: &str, method: &str, target: &str, accept: &str, date: &str) -> String {
    let text = format!(
        "(request-target): {} {target}\naccept: {accept}\ndate: {date}",
        method.to_ascii_lowercase()
    );
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("any key length is fine");
    mac.update(text.as_bytes());
    STANDARD.encode(mac.finalize().into_bytes())
}

/// The `Authorization` header value.
pub fn authorization(key_id: &str, signature: &str) -> String {
    format!(
        r#"Signature keyId="{key_id}",algorithm="hmac-sha256",headers="{SIGNED_HEADERS}",signature="{signature}""#
    )
}

fn http_date() -> String {
    chrono::Utc::now()
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string()
}

fn org(form: &ServiceForm) -> &str {
    let org = form.org_id.trim();
    if org.is_empty() {
        DEFAULT_ORG
    } else {
        org
    }
}

fn check(form: &ServiceForm, secret: &str) -> Result<BaseUrl> {
    crate::settings::check_secret(secret)?;
    let key = form.access_key_id.trim();
    if key.is_empty() || !key.bytes().all(|b| (0x21..=0x7e).contains(&b) && b != b'"') {
        return Err(ServiceError::Invalid(
            "Enter the access key id (create an access key under Profile, API key in JumpServer)."
                .into(),
        ));
    }
    let org = org(form);
    if !org.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(ServiceError::Invalid(
            "The organization id is not valid.".into(),
        ));
    }
    BaseUrl::parse(&form.url)
}

/// One page of assets: the `results` and JumpServer's `count`.
fn fetch_page(
    base: &BaseUrl,
    form: &ServiceForm,
    secret: &str,
    limit: usize,
    offset: usize,
) -> Result<(usize, bool, Vec<Value>)> {
    let url = base.join(
        "/api/v1/assets/hosts/",
        &[("limit", limit.to_string()), ("offset", offset.to_string())],
    );
    let parsed = url::Url::parse(&url).map_err(|_| ServiceError::Invalid("Bad address.".into()))?;
    let target = match parsed.query() {
        Some(query) => format!("{}?{query}", parsed.path()),
        None => parsed.path().to_owned(),
    };
    let date = http_date();
    let sig = signature(secret, "GET", &target, "application/json", &date);
    let auth = authorization(form.access_key_id.trim(), &sig);
    let reply = http::get(
        &url,
        &[
            ("Authorization", &auth),
            ("Date", &date),
            ("X-JMS-ORG", org(form)),
        ],
    )?;
    match reply.status {
        200 => {}
        401 => {
            return Err(ServiceError::Unauthorized(
                "JumpServer refused the access key: check the key id and secret, that the key is enabled, and this computer's clock (a signed request is only valid for a few minutes).".into(),
            ))
        }
        403 => {
            return Err(ServiceError::Forbidden(
                "The access key is not allowed to list assets. Use a key of a user that can see the assets in this organization.".into(),
            ))
        }
        404 => {
            return Err(ServiceError::Protocol(
                "This address does not answer /api/v1/assets/hosts/. Is it a JumpServer (version 3 or later)?".into(),
            ))
        }
        429 => {
            return Err(ServiceError::RateLimited {
                retry_after: reply.retry_after,
            })
        }
        status => {
            return Err(ServiceError::Status(
                status,
                http::error_detail(&reply.body, secret),
            ))
        }
    }
    let value: Value = serde_json::from_slice(&reply.body).map_err(|_| unexpected())?;
    match value {
        // An unpaginated answer.
        Value::Array(list) => Ok((list.len(), false, list)),
        Value::Object(map) => {
            let list = map
                .get("results")
                .and_then(Value::as_array)
                .ok_or_else(unexpected)?
                .clone();
            let count = map
                .get("count")
                .and_then(Value::as_u64)
                .map_or(list.len(), |count| count as usize);
            let more = map.get("next").is_some_and(|next| !next.is_null());
            Ok((count, more, list))
        }
        _ => Err(unexpected()),
    }
}

fn unexpected() -> ServiceError {
    ServiceError::Protocol(
        "The answer is not an asset list. Is this a JumpServer address?".to_owned(),
    )
}

/// One asset. Everything is optional: a missing field is the asset's
/// problem, not the whole import's.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Asset {
    pub id: Option<String>,
    pub name: Option<String>,
    pub address: Option<String>,
    pub is_active: Option<bool>,
    pub platform: Option<Value>,
    pub protocols: Option<Vec<Value>>,
    pub nodes_display: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct AssetList {
    pub assets: Vec<Asset>,
    pub total: usize,
    pub warnings: Vec<String>,
}

/// Reads every page of assets.
pub fn fetch_assets(form: &ServiceForm, secret: &str) -> Result<AssetList> {
    let secret = secret.trim();
    let base = check(form, secret)?;
    let mut out = AssetList::default();
    let mut offset = 0;
    for _ in 0..MAX_PAGES {
        let (count, more, list) = fetch_page(&base, form, secret, PAGE_SIZE, offset)?;
        out.total = count;
        let got = list.len();
        for item in list {
            match serde_json::from_value::<Asset>(item) {
                Ok(asset) => out.assets.push(asset),
                Err(_) => out
                    .warnings
                    .push("An asset in the list could not be read and was skipped.".to_owned()),
            }
        }
        offset += PAGE_SIZE;
        if !more || got == 0 || offset >= count {
            return Ok(out);
        }
    }
    out.warnings.push(format!(
        "JumpServer lists {} assets; only the first {} were read.",
        out.total,
        MAX_PAGES * PAGE_SIZE
    ));
    Ok(out)
}

/// Checks the address and the key with one small request. Returns the number
/// of assets JumpServer lists.
pub fn test(form: &ServiceForm, secret: &str) -> Result<usize> {
    let secret = secret.trim();
    let base = check(form, secret)?;
    let (count, _, _) = fetch_page(&base, form, secret, 1, 0)?;
    Ok(count)
}

/// The prefix of every source key of this JumpServer organization.
pub fn scope(form: &ServiceForm) -> Result<String> {
    Ok(format!(
        "jumpserver:{}#{}#",
        BaseUrl::parse(&form.url)?.origin(),
        org(form)
    ))
}

/// A choice field's label: JumpServer sends `{"value","label"}` or an object
/// with a `name`, or a plain string.
fn label(value: Option<&Value>) -> String {
    match value {
        Some(Value::Object(map)) => ["name", "label", "value"]
            .iter()
            .find_map(|key| map.get(*key).and_then(Value::as_str))
            .unwrap_or_default()
            .to_owned(),
        Some(Value::String(text)) => text.clone(),
        _ => String::new(),
    }
}

/// The SSH port of an asset: `Some(port)` for an `ssh` protocol, `None` when
/// the asset has protocols but not SSH (RDP, VNC, databases), and 22 when the
/// asset lists no protocols at all and is not Windows.
fn ssh_port(asset: &Asset, platform: &str) -> Option<u16> {
    match &asset.protocols {
        Some(protocols) if !protocols.is_empty() => protocols.iter().find_map(|protocol| {
            let name = protocol.get("name").and_then(Value::as_str)?;
            if !name.eq_ignore_ascii_case("ssh") {
                return None;
            }
            let port = protocol.get("port").and_then(Value::as_u64).unwrap_or(22);
            u16::try_from(port).ok().filter(|port| *port != 0)
        }),
        _ => (!platform.to_ascii_lowercase().contains("windows")).then_some(22),
    }
}

/// The group path below the JumpServer: the first node path of the asset
/// without its root node (`/Default/Prod/Web` is `Prod/Web`).
fn node_path(asset: &Asset) -> Vec<String> {
    let first = asset
        .nodes_display
        .as_ref()
        .and_then(|nodes| nodes.iter().find(|node| !node.trim().is_empty()));
    let parts: Vec<String> = first
        .map(|node| {
            node.split('/')
                .map(|part| part.trim().to_owned())
                .filter(|part| !part.is_empty())
                .collect()
        })
        .unwrap_or_default();
    parts.into_iter().skip(1).collect()
}

/// Where the host connects when the gateway is used: host, port and the SSH
/// user name for `asset_address`: `jms_user@account@asset_address`, or only
/// `jms_user` (menu mode) when the account is empty.
pub fn gateway_login(
    form: &ServiceForm,
    base: &BaseUrl,
    asset_address: &str,
) -> Result<(String, u16, String)> {
    let user = form.gateway_user.trim();
    if user.is_empty() {
        return Err(ServiceError::Invalid(
            "Enter your JumpServer user name: it is the SSH user name at the gateway.".into(),
        ));
    }
    let host = form.gateway_host.trim();
    let host = if host.is_empty() {
        base.host()
    } else {
        host.to_owned()
    };
    let port = if form.gateway_port == 0 {
        2222
    } else {
        form.gateway_port
    };
    let account = form.default_user.trim();
    if account.contains('@') || user.contains('@') {
        return Err(ServiceError::Invalid(
            "The JumpServer user name and the asset account must not contain @.".into(),
        ));
    }
    let login = if account.is_empty() {
        user.to_owned()
    } else {
        format!("{user}@{account}@{asset_address}")
    };
    Ok((host, port, login))
}

/// Maps the assets to import hosts. Inactive assets and assets without an SSH
/// protocol (Windows over RDP and the like) are counted in `skipped` with a
/// warning.
pub fn map_assets(form: &ServiceForm, list: AssetList) -> Result<Fetched> {
    let base = BaseUrl::parse(&form.url)?;
    let origin = base.origin();
    let top = base.host();
    let org = org(form).to_owned();
    // Fail early on a bad gateway setting, with no asset to name yet.
    if form.gateway {
        gateway_login(form, &base, "")?;
    }
    let user = Some(form.default_user.trim().to_owned()).filter(|user| !user.is_empty());
    let identity = form
        .identity_file
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty());
    let mut fetched = Fetched {
        scope: scope(form)?,
        warnings: list.warnings,
        ..Fetched::default()
    };
    let (mut inactive, mut no_ssh) = (0usize, Vec::new());
    for asset in list.assets {
        let address = asset.address.as_deref().map(str::trim).unwrap_or_default();
        let name = asset.name.as_deref().map(str::trim).unwrap_or_default();
        let shown = if name.is_empty() { address } else { name };
        let Some(id) = asset.id.as_deref().filter(|id| !id.is_empty()) else {
            fetched.skipped += 1;
            fetched
                .warnings
                .push(format!("{shown}: the asset has no id."));
            continue;
        };
        if asset.is_active == Some(false) {
            inactive += 1;
            fetched.skipped += 1;
            continue;
        }
        let platform = label(asset.platform.as_ref());
        let Some(asset_port) = ssh_port(&asset, &platform) else {
            fetched.skipped += 1;
            no_ssh.push(if platform.is_empty() {
                shown.to_owned()
            } else {
                format!("{shown} ({platform})")
            });
            continue;
        };
        if address.is_empty() {
            fetched.skipped += 1;
            fetched
                .warnings
                .push(format!("{shown}: the asset has no address."));
            continue;
        }
        let mut group_path = vec![top.clone()];
        group_path.extend(node_path(&asset));
        let mut host = ImportHost {
            label: shown.to_owned(),
            group_path,
            identity_file: identity.map(str::to_owned),
            source: Some(format!("jumpserver:{origin}#{org}#{id}")),
            ..ImportHost::default()
        };
        match form.gateway {
            true => {
                let (gateway_host, gateway_port, login) = gateway_login(form, &base, address)?;
                host.hostname = gateway_host;
                host.port = gateway_port;
                host.username = Some(login);
                host.notes = if form.default_user.trim().is_empty() {
                    format!(
                        "JumpServer asset {shown} ({address}:{asset_port}). Through the KoKo gateway: at the menu type /{shown} and press Enter."
                    )
                } else {
                    format!(
                        "JumpServer asset {shown} ({address}:{asset_port}), account {}. Direct login through the KoKo gateway.",
                        form.default_user.trim()
                    )
                };
            }
            false => {
                host.hostname = address.to_owned();
                host.port = asset_port;
                host.username = user.clone();
            }
        }
        fetched.hosts.push(host);
    }
    if inactive > 0 {
        fetched.warnings.push(format!(
            "{inactive} asset(s) are disabled in JumpServer and were skipped."
        ));
    }
    if !no_ssh.is_empty() {
        fetched.warnings.push(format!(
            "{} asset(s) have no SSH protocol (for example Windows over RDP) and were skipped: {}.",
            no_ssh.len(),
            no_ssh.join(", ")
        ));
    }
    Ok(fetched)
}

/// Fetches and maps in one go.
pub fn fetch(form: &ServiceForm, secret: &str) -> Result<Fetched> {
    map_assets(form, fetch_assets(form, secret)?)
}
