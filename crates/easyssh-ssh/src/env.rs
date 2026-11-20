//! What a new shell is told about the terminal: the pseudo-terminal modes
//! sent with `pty-req`, and the environment sent with `env` requests.
//!
//! The environment follows OpenSSH: `SendEnv LANG LC_*` passes the local
//! locale (only when it is UTF-8, since the terminal speaks UTF-8), and the
//! host's `SetEnv` wins over anything else. When the local process has no
//! UTF-8 locale (a GUI app on macOS or Windows, a phone), `LANG` comes from
//! the app's language instead, so `ls` and editors still print UTF-8.
//! Servers accept only what their `AcceptEnv` lists (usually `LANG LC_*`);
//! the rest is dropped silently, which is why no reply is asked for.

use russh::Pty;

/// The modes a typical local terminal has, as OpenSSH sends them from a
/// tty: Backspace sends DEL (`VERASE` 0x7f, matching xterm.js), input is
/// UTF-8 (`IUTF8`, so the line editor erases whole characters), and the
/// usual cooked-mode behaviour (signals, line editing, echo, CR/NL mapping).
pub fn default_terminal_modes() -> Vec<(Pty, u32)> {
    vec![
        (Pty::VINTR, 0x03),
        (Pty::VQUIT, 0x1c),
        (Pty::VERASE, 0x7f),
        (Pty::VKILL, 0x15),
        (Pty::VEOF, 0x04),
        (Pty::VSUSP, 0x1a),
        (Pty::VWERASE, 0x17),
        (Pty::VLNEXT, 0x16),
        (Pty::VREPRINT, 0x12),
        (Pty::ICRNL, 1),
        (Pty::IXON, 1),
        (Pty::IUTF8, 1),
        (Pty::ISIG, 1),
        (Pty::ICANON, 1),
        (Pty::IEXTEN, 1),
        (Pty::ECHO, 1),
        (Pty::ECHOE, 1),
        (Pty::ECHOK, 1),
        (Pty::ECHOCTL, 1),
        (Pty::ECHOKE, 1),
        (Pty::OPOST, 1),
        (Pty::ONLCR, 1),
        (Pty::CS8, 1),
    ]
}

fn is_locale_var(name: &str) -> bool {
    name == "LANG" || name.starts_with("LC_")
}

fn is_utf8(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("utf-8") || value.contains("utf8")
}

/// `LANG` for the app's language (`zh`, `zh-CN`, `en`, ...): Chinese gets
/// `zh_CN.UTF-8`, everything else `en_US.UTF-8`.
pub fn lang_for(language: Option<&str>) -> &'static str {
    match language {
        Some(language) if language.to_ascii_lowercase().starts_with("zh") => "zh_CN.UTF-8",
        _ => "en_US.UTF-8",
    }
}

/// The environment for a new shell, in the order it is sent.
///
/// - `COLORTERM=truecolor`: xterm.js draws 24-bit colour.
/// - The local `LANG` and `LC_*` that are UTF-8 (OpenSSH's `SendEnv`).
/// - Without a local UTF-8 `LANG`, `LC_ALL` or `LC_CTYPE`: `LANG` for
///   `app_language` (see [`lang_for`]).
/// - The host's `SetEnv` pairs, which replace any of the above.
pub fn shell_env(
    local: impl IntoIterator<Item = (String, String)>,
    app_language: Option<&str>,
    host_set_env: &[(String, String)],
) -> Vec<(String, String)> {
    let mut env = vec![("COLORTERM".to_owned(), "truecolor".to_owned())];
    let mut locale: Vec<(String, String)> = local
        .into_iter()
        .filter(|(name, value)| is_locale_var(name) && is_utf8(value))
        .collect();
    locale.sort();
    let has_charset = locale
        .iter()
        .any(|(name, _)| matches!(name.as_str(), "LANG" | "LC_ALL" | "LC_CTYPE"));
    env.extend(locale);
    if !has_charset {
        env.push(("LANG".to_owned(), lang_for(app_language).to_owned()));
    }
    for (name, value) in host_set_env {
        env.retain(|(existing, _)| existing != name);
        env.push((name.clone(), value.clone()));
    }
    env
}

/// [`shell_env`] for this process's environment.
pub fn local_shell_env(
    app_language: Option<&str>,
    host_set_env: &[(String, String)],
) -> Vec<(String, String)> {
    shell_env(std::env::vars(), app_language, host_set_env)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn modes_make_backspace_del_and_input_utf8() {
        let modes = default_terminal_modes();
        assert!(modes.contains(&(Pty::VERASE, 0x7f)));
        assert!(modes.contains(&(Pty::IUTF8, 1)));
        for flag in [Pty::ICRNL, Pty::ONLCR, Pty::ISIG, Pty::ICANON, Pty::ECHO] {
            assert!(modes.contains(&(flag, 1)), "{flag:?}");
        }
        assert!(!modes.iter().any(|(mode, _)| *mode == Pty::TTY_OP_END));
    }

    #[test]
    fn passes_a_utf8_local_locale_through() {
        let local = pairs(&[
            ("LANG", "de_DE.UTF-8"),
            ("LC_TIME", "en_GB.utf8"),
            ("LC_PAPER", "de_DE.ISO-8859-1"),
            ("HOME", "/home/me"),
        ]);
        assert_eq!(
            shell_env(local, Some("zh"), &[]),
            pairs(&[
                ("COLORTERM", "truecolor"),
                ("LANG", "de_DE.UTF-8"),
                ("LC_TIME", "en_GB.utf8"),
            ])
        );
    }

    #[test]
    fn falls_back_to_the_app_language_without_a_utf8_locale() {
        let local = pairs(&[("LANG", "C"), ("LC_TIME", "en_GB.UTF-8")]);
        assert_eq!(
            shell_env(local, Some("zh-CN"), &[]),
            pairs(&[
                ("COLORTERM", "truecolor"),
                ("LC_TIME", "en_GB.UTF-8"),
                ("LANG", "zh_CN.UTF-8"),
            ])
        );
        assert_eq!(
            shell_env(Vec::new(), None, &[]),
            pairs(&[("COLORTERM", "truecolor"), ("LANG", "en_US.UTF-8")])
        );
        assert_eq!(lang_for(Some("en")), "en_US.UTF-8");
        assert_eq!(lang_for(Some("zh_cn")), "zh_CN.UTF-8");
    }

    #[test]
    fn the_hosts_set_env_wins() {
        let env = shell_env(
            pairs(&[("LANG", "de_DE.UTF-8")]),
            Some("zh"),
            &pairs(&[("LANG", "ja_JP.UTF-8"), ("EDITOR", "vim")]),
        );
        assert_eq!(
            env,
            pairs(&[
                ("COLORTERM", "truecolor"),
                ("LANG", "ja_JP.UTF-8"),
                ("EDITOR", "vim"),
            ])
        );
        let env = shell_env(Vec::new(), Some("zh"), &pairs(&[("LANG", "C.UTF-8")]));
        assert_eq!(
            env,
            pairs(&[("COLORTERM", "truecolor"), ("LANG", "C.UTF-8")])
        );
    }
}
