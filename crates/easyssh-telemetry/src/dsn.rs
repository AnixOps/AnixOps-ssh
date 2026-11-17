//! A Sentry DSN, `scheme://public_key@host[:port][/prefix]/project`, and the
//! two things derived from it: the envelope URL and the auth header.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dsn {
    raw: String,
    scheme: String,
    key: String,
    authority: String,
    prefix: String,
    project: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct DsnError(pub &'static str);

impl fmt::Display for DsnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid DSN: {}", self.0)
    }
}

impl std::error::Error for DsnError {}

/// `https` always; plain `http` only for a loopback address (local tests and
/// a GlitchTip on the same machine). Nothing else is ever contacted unencrypted.
pub fn check_url(url: &str) -> Result<(), DsnError> {
    let (scheme, rest) = url.split_once("://").ok_or(DsnError("no scheme"))?;
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("");
    match scheme {
        "https" if !host.is_empty() => Ok(()),
        "http" if is_loopback(host) => Ok(()),
        "http" => Err(DsnError("http is only allowed for a loopback host")),
        _ => Err(DsnError("scheme must be https")),
    }
}

fn is_loopback(authority: &str) -> bool {
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    host == "localhost" || host == "127.0.0.1" || host == "::1"
}

impl Dsn {
    pub fn parse(raw: &str) -> Result<Dsn, DsnError> {
        let raw = raw.trim();
        check_url(raw)?;
        let (scheme, rest) = raw.split_once("://").ok_or(DsnError("no scheme"))?;
        let (userinfo, location) = rest.split_once('@').ok_or(DsnError("no public key"))?;
        let key = userinfo.split(':').next().unwrap_or("");
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(DsnError("bad public key"));
        }
        let (authority, path) = location.split_once('/').ok_or(DsnError("no project id"))?;
        if authority.is_empty() {
            return Err(DsnError("no host"));
        }
        let path = path.trim_end_matches('/');
        let (prefix, project) = match path.rsplit_once('/') {
            Some((prefix, project)) => (format!("/{prefix}"), project),
            None => (String::new(), path),
        };
        if project.is_empty() || !project.chars().all(|c| c.is_ascii_digit()) {
            return Err(DsnError("project id must be a number"));
        }
        Ok(Dsn {
            raw: raw.to_owned(),
            scheme: scheme.to_owned(),
            key: key.to_owned(),
            authority: authority.to_owned(),
            prefix,
            project: project.to_owned(),
        })
    }

    /// `POST` target of an envelope.
    pub fn envelope_url(&self) -> String {
        format!(
            "{}://{}{}/api/{}/envelope/",
            self.scheme, self.authority, self.prefix, self.project
        )
    }

    pub fn auth_header(&self) -> String {
        format!(
            "Sentry sentry_version=7, sentry_client=easyssh-telemetry/{}, sentry_key={}",
            env!("CARGO_PKG_VERSION"),
            self.key
        )
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_derives() {
        let d = Dsn::parse("https://abc123@glitchtip.example.org/7").unwrap();
        assert_eq!(
            d.envelope_url(),
            "https://glitchtip.example.org/api/7/envelope/"
        );
        assert!(d.auth_header().ends_with("sentry_key=abc123"));
        assert!(d.auth_header().starts_with("Sentry sentry_version=7"));
    }

    #[test]
    fn keeps_port_prefix_and_ignores_secret() {
        let d = Dsn::parse("https://k:secret@host.example:8443/gt/sub/12/").unwrap();
        assert_eq!(
            d.envelope_url(),
            "https://host.example:8443/gt/sub/api/12/envelope/"
        );
        assert!(!d.auth_header().contains("secret"));
    }

    #[test]
    fn loopback_http_only() {
        assert!(Dsn::parse("http://k@127.0.0.1:9000/1").is_ok());
        assert!(Dsn::parse("http://k@localhost/1").is_ok());
        assert!(Dsn::parse("http://k@[::1]:9000/1").is_ok());
        assert!(Dsn::parse("http://k@example.org/1").is_err());
        assert!(Dsn::parse("ftp://k@example.org/1").is_err());
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "",
            "not a dsn",
            "https://example.org/1",
            "https://@example.org/1",
            "https://k@example.org",
            "https://k@example.org/",
            "https://k@example.org/abc",
            "https://k@/1",
            "https://k y@example.org/1",
        ] {
            assert!(Dsn::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn check_url_for_usage_endpoint() {
        assert!(check_url("https://stats.example.org/ping").is_ok());
        assert!(check_url("http://127.0.0.1:1/p").is_ok());
        assert!(check_url("http://stats.example.org/ping").is_err());
        assert!(check_url("stats.example.org").is_err());
    }
}
