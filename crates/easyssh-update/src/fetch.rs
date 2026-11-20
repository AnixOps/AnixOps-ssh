//! Fetching the feed: one plain HTTPS `GET`, no cookies, no identifiers,
//! redirects followed only to the hosts GitHub serves release assets from.

use crate::feed::is_https;
use std::io::Read;
use std::time::Duration;

/// A feed is a few KiB; anything bigger is not one.
pub const MAX_BODY: u64 = 256 * 1024;
const MAX_REDIRECTS: usize = 5;
/// Hosts a feed URL may be served from or redirected to.
const HOSTS: [&str; 3] = [
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// The body of a `GET`, or a message. Faked in tests.
pub trait Fetch {
    fn get(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// The host of an HTTPS URL when it is one of [`HOSTS`].
pub fn allowed_host(url: &str) -> Option<&str> {
    if !is_https(url) {
        return None;
    }
    let authority = url[8..].split(['/', '?', '#']).next()?;
    // A port other than 443 is not a release host.
    let host = match authority.split_once(':') {
        Some((host, "443")) => host,
        Some(_) => return None,
        None => authority,
    };
    HOSTS.iter().copied().find(|h| h.eq_ignore_ascii_case(host))
}

/// The absolute URL a `Location` header names, resolved against `base`.
fn resolve(base: &str, location: &str) -> Option<String> {
    if location.starts_with('/') && !location.starts_with("//") {
        let origin_end = base[8..].find('/').map_or(base.len(), |i| i + 8);
        return Some(format!("{}{location}", &base[..origin_end]));
    }
    Some(location.to_owned())
}

/// `ureq` with short timeouts. Redirects are followed here, one hop at a
/// time, so that every hop is checked.
pub struct Http;

impl Fetch for Http {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .redirects(0)
            .user_agent(concat!("easyssh-update/", env!("CARGO_PKG_VERSION")))
            .build();
        let mut url = url.to_owned();
        for _ in 0..=MAX_REDIRECTS {
            if allowed_host(&url).is_none() {
                return Err("the feed URL is not on a GitHub release host".to_owned());
            }
            let response = match agent.get(&url).call() {
                Ok(response) => response,
                Err(ureq::Error::Status(_, response)) => response,
                Err(error) => return Err(error.kind().to_string()),
            };
            match response.status() {
                200 => {
                    let mut body = Vec::new();
                    response
                        .into_reader()
                        .take(MAX_BODY + 1)
                        .read_to_end(&mut body)
                        .map_err(|e| e.to_string())?;
                    return if body.len() as u64 > MAX_BODY {
                        Err("the feed is too large".to_owned())
                    } else {
                        Ok(body)
                    };
                }
                301 | 302 | 303 | 307 | 308 => {
                    let location = response.header("location").unwrap_or_default();
                    url = resolve(&url, location).ok_or("bad redirect")?;
                }
                status => return Err(format!("HTTP {status}")),
            }
        }
        Err("too many redirects".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_github_release_hosts_over_https() {
        for ok in [
            "https://github.com/AnixOps/EasySSH/releases/latest/download/x.json",
            "https://objects.githubusercontent.com/github-production-release-asset/1",
            "https://release-assets.githubusercontent.com/a?b=c",
            "https://GitHub.com:443/x",
        ] {
            assert!(allowed_host(ok).is_some(), "{ok}");
        }
        for bad in [
            "http://github.com/x",
            "https://evil.example/x",
            "https://github.com.evil.example/x",
            "https://evil.example/github.com",
            "https://github.com@evil.example/x",
            "https://github.com:8443/x",
            "https://notgithub.com/x",
            "https://raw.githubusercontent.com/x",
            "file:///etc/passwd",
        ] {
            assert!(allowed_host(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn relative_redirects_stay_on_the_origin() {
        assert_eq!(
            resolve("https://github.com/a/b", "/c/d").as_deref(),
            Some("https://github.com/c/d")
        );
        assert_eq!(
            resolve("https://github.com", "/c").as_deref(),
            Some("https://github.com/c")
        );
        // A protocol-relative location is passed on and then refused by the host check.
        assert_eq!(
            resolve("https://github.com/a", "//evil/x").as_deref(),
            Some("//evil/x")
        );
        assert!(allowed_host("//evil/x").is_none());
    }
}
