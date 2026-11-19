//! The one HTTP client of the service importers: GET only, HTTPS required
//! (plain HTTP only to the local machine, for testing), no redirects (a
//! redirect could carry a credential somewhere the user did not name), bounded
//! time and size.

use crate::error::{Result, ServiceError};
use std::io::Read;
use std::time::Duration;
use url::Url;

/// The largest reply read.
const MAX_BODY: u64 = 8 * 1024 * 1024;

/// A validated service address: scheme, host, optional port and an optional
/// path prefix (a service behind a reverse proxy), without a trailing slash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseUrl {
    url: Url,
}

impl BaseUrl {
    /// Parses what the user typed. `https` is required; `http` is accepted for
    /// `localhost`, `127.0.0.1` and `::1` only. Credentials, a query and a
    /// fragment in the address are refused.
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        if text.is_empty() {
            return Err(ServiceError::Invalid(
                "Enter the address of the service.".into(),
            ));
        }
        let with_scheme = if text.contains("://") {
            text.to_owned()
        } else {
            format!("https://{text}")
        };
        let mut url = Url::parse(&with_scheme)
            .map_err(|_| ServiceError::Invalid("The address is not valid.".into()))?;
        let host = url
            .host_str()
            .filter(|host| !host.is_empty())
            .ok_or_else(|| ServiceError::Invalid("The address has no host name.".into()))?
            .to_owned();
        match url.scheme() {
            "https" => {}
            "http" if is_local(&host) => {}
            "http" => return Err(ServiceError::Invalid(
                "The address must start with https:// (http:// is accepted for localhost only)."
                    .into(),
            )),
            _ => {
                return Err(ServiceError::Invalid(
                    "The address must start with https://.".into(),
                ))
            }
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(ServiceError::Invalid(
                "Leave the user name and password out of the address.".into(),
            ));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(ServiceError::Invalid(
                "The address must not have a query or a fragment.".into(),
            ));
        }
        let path = url.path().trim_end_matches('/').to_owned();
        url.set_path(&path);
        Ok(Self { url })
    }

    /// `scheme://host[:port]` with a lower-case host: identifies the service
    /// in source keys.
    pub fn origin(&self) -> String {
        self.url.origin().ascii_serialization()
    }

    /// The host name or address, without brackets.
    pub fn host(&self) -> String {
        self.url
            .host_str()
            .unwrap_or_default()
            .trim_matches(['[', ']'])
            .to_owned()
    }

    /// `base()` + `path` (starting with `/`) + the query pairs, encoded.
    pub fn join(&self, path: &str, query: &[(&str, String)]) -> String {
        let mut url = self.url.clone();
        url.set_path(&format!(
            "{}{}",
            self.url.path().trim_end_matches('/'),
            path
        ));
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
        }
        url.to_string()
    }
}

fn is_local(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}

/// What came back, for the importer to interpret.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub retry_after: Option<u64>,
    pub body: Vec<u8>,
}

/// GETs `url` with `headers`. Any HTTP status is a reply, not an error.
pub fn get(url: &str, headers: &[(&str, &str)]) -> Result<Reply> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .timeout_write(Duration::from_secs(10))
        .redirects(0)
        .user_agent(concat!("easyssh-services/", env!("CARGO_PKG_VERSION")))
        .build();
    let mut call = agent.get(url).set("Accept", "application/json");
    for (name, value) in headers {
        call = call.set(name, value);
    }
    let response = match call.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(ureq::Error::Transport(transport)) => return Err(network(&transport)),
    };
    let status = response.status();
    let retry_after = response
        .header("Retry-After")
        .and_then(|value| value.trim().parse::<u64>().ok());
    let mut body = Vec::new();
    response
        .into_reader()
        .take(MAX_BODY + 1)
        .read_to_end(&mut body)
        .map_err(|_| {
            ServiceError::Network("The connection was lost while reading the reply.".into())
        })?;
    if body.len() as u64 > MAX_BODY {
        return Err(ServiceError::Protocol("The reply is too large.".into()));
    }
    Ok(Reply {
        status,
        retry_after,
        body,
    })
}

fn network(error: &ureq::Transport) -> ServiceError {
    use ureq::ErrorKind::*;
    ServiceError::Network(
        match error.kind() {
            Dns => "Cannot find the service: check the address and your network.",
            ConnectionFailed => {
                "Cannot connect to the service: is the address right and the service running?"
            }
            InsecureRequestHttpsOnly | InvalidUrl | UnknownScheme => "The address is not valid.",
            TooManyRedirects => {
                "The service redirected the request. Redirects are not followed; use the address it redirects to."
            }
            Io => "The connection was lost or timed out.",
            _ => "The request could not be sent. If the address is https, check the service's certificate.",
        }
        .into(),
    )
}

/// The `error.message` (or similar) of a failed reply, cleaned and with
/// `secret` removed, for an error message.
pub fn error_detail(body: &[u8], secret: &str) -> String {
    let value: Option<serde_json::Value> = serde_json::from_slice(body).ok();
    let text = value
        .as_ref()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("detail"))
                .or_else(|| v.get("error"))
                .or_else(|| v.get("msg"))
                .or_else(|| v.get("message"))
        })
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let mut text: String = text.chars().filter(|c| !c.is_control()).take(200).collect();
    if !secret.is_empty() {
        text = text.replace(secret, "…");
    }
    text
}

/// The `error.code` of a failed reply, when it has one.
pub fn error_code(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value
        .pointer("/error/code")
        .and_then(|v| v.as_str())
        .map(|code| code.chars().take(64).collect())
}
