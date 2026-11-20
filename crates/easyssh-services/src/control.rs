//! AnixOps Control: the panel's nodes are the servers ops log in to.
//!
//! `GET {panel}/api/v4/admin/nodes?page=N&page_size=100` with
//! `Authorization: Bearer anixadm_...` (an admin API token; scope `read`
//! is enough). The reply is `{"data":{"total":n,"list":[node...]}}`; a failure
//! is `{"error":{"code","message"}}`. The token is only ever sent in the
//! header: the panel refuses a token in a URL and audits it.
//!
//! Node fields used: `id`, `name`, `host` (the address clients use, IP or
//! domain), `server_ip` (nullable, the real IP), `tags` (nullable, a JSON
//! array in a string), `node_group_name`, `approval` (`approved`, `pending`
//! or `rejected`; empty means approved).

use crate::error::{Result, ServiceError};
use crate::http::{self, BaseUrl};
use crate::{Fetched, ServiceForm};
use easyssh_store::import::ImportHost;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeSet;

/// Nodes per request (the panel's maximum).
const PAGE_SIZE: usize = 100;
/// Pages read at most; with [`PAGE_SIZE`] this is the importable maximum.
const MAX_PAGES: usize = 50;
/// The group of nodes that belong to no node group.
pub const DEFAULT_GROUP: &str = "AnixOps Control";
const TOKEN_PREFIX: &str = "anixadm_";

/// One node as the panel lists it. Everything is optional: a missing field is
/// the node's problem, not the whole import's.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Node {
    pub id: u64,
    pub name: Option<String>,
    pub host: Option<String>,
    pub server_ip: Option<String>,
    pub tags: Option<Value>,
    pub node_group_name: Option<String>,
    pub approval: Option<String>,
}

/// What a fetch brought back.
#[derive(Debug, Clone, Default)]
pub struct NodeList {
    pub nodes: Vec<Node>,
    /// The panel's own count.
    pub total: usize,
    pub warnings: Vec<String>,
}

fn base(form: &ServiceForm) -> Result<BaseUrl> {
    BaseUrl::parse(&form.url)
}

fn check_token(token: &str) -> Result<()> {
    crate::settings::check_secret(token)?;
    if !token.starts_with(TOKEN_PREFIX) {
        return Err(ServiceError::Invalid(format!(
            "An admin API token starts with {TOKEN_PREFIX}. Create one in the panel under API tokens."
        )));
    }
    Ok(())
}

/// One page of the node list.
fn fetch_page(
    base: &BaseUrl,
    token: &str,
    page: usize,
    page_size: usize,
) -> Result<(usize, Vec<Value>)> {
    let url = base.join(
        "/api/v4/admin/nodes",
        &[
            ("page", page.to_string()),
            ("page_size", page_size.to_string()),
        ],
    );
    debug_assert!(!url.contains(TOKEN_PREFIX));
    let reply = http::get(&url, &[("Authorization", &format!("Bearer {token}"))])?;
    match reply.status {
        200 => {}
        401 => {
            let code = http::error_code(&reply.body).unwrap_or_default();
            return Err(ServiceError::Unauthorized(match code.as_str() {
                "api_token_invalid" => "The panel refused the token: it is wrong, revoked or expired, or its owner is no longer an administrator.".to_owned(),
                _ => "The panel refused the token (401). Check the token and the address.".to_owned(),
            }));
        }
        403 => {
            return Err(ServiceError::Forbidden(
                "The token is not allowed to read nodes. Use a token of an administrator with scope read or admin.".to_owned(),
            ))
        }
        429 => {
            return Err(ServiceError::RateLimited {
                retry_after: reply.retry_after,
            })
        }
        404 => {
            return Err(ServiceError::Protocol(
                "This address does not answer /api/v4/admin/nodes. Is it an AnixOps Control panel (version 5 or later)?".to_owned(),
            ))
        }
        status => {
            return Err(ServiceError::Status(
                status,
                http::error_detail(&reply.body, token),
            ))
        }
    }
    let value: Value = serde_json::from_slice(&reply.body).map_err(|_| unexpected())?;
    let data = value.get("data").ok_or_else(unexpected)?;
    let list = data
        .get("list")
        .and_then(Value::as_array)
        .ok_or_else(unexpected)?;
    let total = data
        .get("total")
        .and_then(Value::as_u64)
        .map_or(list.len(), |total| total as usize);
    Ok((total, list.clone()))
}

fn unexpected() -> ServiceError {
    ServiceError::Protocol(
        "The panel's answer is not a node list. Is this an AnixOps Control panel?".to_owned(),
    )
}

/// Reads every page of the node list.
pub fn fetch_nodes(form: &ServiceForm, token: &str) -> Result<NodeList> {
    let token = token.trim();
    check_token(token)?;
    let base = base(form)?;
    let mut out = NodeList::default();
    for page in 1..=MAX_PAGES {
        let (total, list) = fetch_page(&base, token, page, PAGE_SIZE)?;
        out.total = total;
        let empty = list.is_empty();
        for item in list {
            match serde_json::from_value::<Node>(item) {
                Ok(node) => out.nodes.push(node),
                Err(_) => out.warnings.push(
                    "A node in the panel's list could not be read and was skipped.".to_owned(),
                ),
            }
        }
        if empty || out.nodes.len() + out.warnings.len() >= total {
            return Ok(out);
        }
    }
    out.warnings.push(format!(
        "The panel lists {} nodes; only the first {} were read.",
        out.total,
        MAX_PAGES * PAGE_SIZE
    ));
    Ok(out)
}

/// Checks the address and the token with one small request. Returns the
/// number of nodes the panel has.
pub fn test(form: &ServiceForm, token: &str) -> Result<usize> {
    let token = token.trim();
    check_token(token)?;
    let (total, _) = fetch_page(&base(form)?, token, 1, 1)?;
    Ok(total)
}

/// The prefix of every source key of this panel.
pub fn scope(form: &ServiceForm) -> Result<String> {
    Ok(format!("control:{}#", base(form)?.origin()))
}

/// Reads `tags`: a JSON array in a string (what the panel stores), a real
/// array, or a comma separated string.
pub fn parse_tags(value: Option<&Value>) -> Vec<String> {
    let items: Vec<String> = match value {
        Some(Value::String(text)) => {
            let text = text.trim();
            match serde_json::from_str::<Value>(text) {
                Ok(Value::Array(items)) => items.iter().filter_map(scalar).collect(),
                _ => text.split(',').map(str::to_owned).collect(),
            }
        }
        Some(Value::Array(items)) => items.iter().filter_map(scalar).collect(),
        _ => Vec::new(),
    };
    let mut seen = BTreeSet::new();
    items
        .into_iter()
        .map(|tag| {
            tag.trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(48)
                .collect::<String>()
        })
        .filter(|tag| !tag.is_empty() && seen.insert(tag.clone()))
        .collect()
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// Maps the approved nodes to import hosts. Nodes that are not approved, or
/// have no address, are counted in `skipped` with a warning.
pub fn map_nodes(form: &ServiceForm, list: NodeList) -> Result<Fetched> {
    let base = base(form)?;
    let origin = base.origin();
    let panel = base.host();
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
    let mut unapproved = 0usize;
    for node in list.nodes {
        let name = node.name.as_deref().map(str::trim).unwrap_or_default();
        let approval = node.approval.as_deref().map(str::trim).unwrap_or_default();
        if !approval.is_empty() && approval != "approved" {
            unapproved += 1;
            fetched.skipped += 1;
            continue;
        }
        let address = [node.host.as_deref(), node.server_ip.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .find(|address| !address.is_empty());
        let Some(address) = address else {
            fetched.skipped += 1;
            fetched.warnings.push(format!(
                "{}: the node has no address (host and server_ip are empty).",
                label_of(name, node.id)
            ));
            continue;
        };
        let group = node
            .node_group_name
            .as_deref()
            .map(str::trim)
            .filter(|group| !group.is_empty())
            .unwrap_or(DEFAULT_GROUP);
        fetched.hosts.push(ImportHost {
            label: label_of(name, node.id),
            hostname: address.to_owned(),
            port: 22,
            username: user.clone(),
            group_path: vec![panel.clone(), group.to_owned()],
            identity_file: identity.map(str::to_owned),
            tags: parse_tags(node.tags.as_ref()),
            source: Some(format!("control:{origin}#{}", node.id)),
            ..ImportHost::default()
        });
    }
    if unapproved > 0 {
        fetched.warnings.push(format!(
            "{unapproved} node(s) are not approved in the panel (pending or rejected) and were skipped."
        ));
    }
    Ok(fetched)
}

fn label_of(name: &str, id: u64) -> String {
    if name.is_empty() {
        format!("node-{id}")
    } else {
        name.to_owned()
    }
}

/// Fetches and maps in one go.
pub fn fetch(form: &ServiceForm, token: &str) -> Result<Fetched> {
    map_nodes(form, fetch_nodes(form, token)?)
}
