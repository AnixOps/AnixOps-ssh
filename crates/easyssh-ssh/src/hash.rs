//! SHA-256 of a remote file, computed by the server with `sha256sum` (or
//! `shasum -a 256`), so a transfer can be checked without downloading again.

use crate::channels::ExitStatus;
use crate::error::{Error, Result};
use crate::session::Session;

/// `path` as one shell word: single quotes around it, and each single quote
/// inside as `'\''`. Nothing in it is interpreted by the shell.
pub fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

/// The digest at the start of a `sha256sum` / `shasum` line. When the file
/// name needs escaping the line starts with a backslash, which is skipped.
pub fn parse_sha256(output: &str) -> Option<String> {
    let line = output.lines().next()?.trim_start_matches('\\');
    let digest: String = line.chars().take(64).collect();
    (digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| digest.to_ascii_lowercase())
}

impl Session {
    /// The SHA-256 of the remote file `path`, lower-case hex. `Ok(None)`
    /// when the server has neither `sha256sum` nor `shasum`.
    pub async fn remote_sha256(&self, path: &str) -> Result<Option<String>> {
        let quoted = shell_quote(path);
        for tool in ["sha256sum", "shasum -a 256"] {
            let output = self.exec(&format!("{tool} -- {quoted}")).await?;
            match output.exit {
                ExitStatus::Code(0) => {
                    return parse_sha256(&output.stdout_text())
                        .map(Some)
                        .ok_or_else(|| {
                            Error::Channel(format!(
                                "unexpected answer from {tool}: {}",
                                output.stdout_text().trim()
                            ))
                        });
                }
                // The shell could not find the tool: try the next one.
                ExitStatus::Code(127) => {}
                _ => {
                    let reason = output.stderr_text();
                    let reason = reason.trim();
                    if reason.contains("not found") || reason.contains("No such file or command") {
                        continue;
                    }
                    return Err(Error::Channel(format!(
                        "{tool} failed for {path}: {}",
                        if reason.is_empty() {
                            "no reason given"
                        } else {
                            reason
                        }
                    )));
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_cannot_leave_its_quotes() {
        assert_eq!(shell_quote("/srv/a b"), "'/srv/a b'");
        assert_eq!(shell_quote("/srv/it's"), r"'/srv/it'\''s'");
        assert_eq!(shell_quote("/x;rm -rf ~"), "'/x;rm -rf ~'");
        assert_eq!(shell_quote("/x$(id)`id`"), "'/x$(id)`id`'");
    }

    #[test]
    fn digests_are_read_from_either_tool_and_nothing_else() {
        let digest = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(
            parse_sha256(&format!("{digest}  /srv/empty\n")).as_deref(),
            Some(digest)
        );
        assert_eq!(
            parse_sha256(&format!("\\{digest}  /srv/a\\nb\n")).as_deref(),
            Some(digest)
        );
        assert_eq!(parse_sha256("sha256sum: not found"), None);
        assert_eq!(parse_sha256(""), None);
    }
}
