//! OpenSSH client configuration (`~/.ssh/config`).
//!
//! Semantics follow `ssh_config(5)`: for each option the first value found
//! wins, in file order, across all `Host` blocks whose patterns match the
//! alias. A `Host *` block therefore supplies defaults, but only for options
//! an earlier matching block did not set. `IdentityFile` is cumulative, as
//! in OpenSSH.

use crate::model::{ImportReport, ImportWarning, ImportedHost};
use std::path::{Path, PathBuf};

/// `Include` nesting limit, counting the top-level file as depth 0.
pub const MAX_INCLUDE_DEPTH: usize = 8;
const MAX_FILES: usize = 512;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Options that make a host need the OpenSSH engine: `(lowercase key, name)`.
const OPENSSH_ONLY: &[(&str, &str)] = &[
    ("proxycommand", "ProxyCommand"),
    ("localcommand", "LocalCommand"),
    ("knownhostscommand", "KnownHostsCommand"),
    ("ciphers", "Ciphers"),
    ("macs", "MACs"),
    ("kexalgorithms", "KexAlgorithms"),
    ("hostkeyalgorithms", "HostKeyAlgorithms"),
    ("pubkeyacceptedalgorithms", "PubkeyAcceptedAlgorithms"),
    ("pubkeyacceptedkeytypes", "PubkeyAcceptedKeyTypes"),
    ("hostbasedacceptedalgorithms", "HostbasedAcceptedAlgorithms"),
    ("hostbasedkeytypes", "HostbasedKeyTypes"),
    ("casignaturealgorithms", "CASignatureAlgorithms"),
    ("pkcs11provider", "PKCS11Provider"),
];

/// Options copied into the notes because the neutral model has no field.
const NOTE_KEYS: &[(&str, &str)] = &[
    ("localforward", "LocalForward"),
    ("remoteforward", "RemoteForward"),
    ("dynamicforward", "DynamicForward"),
    ("remotecommand", "RemoteCommand"),
];

/// True when the bytes look like an OpenSSH client config.
pub fn detect(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    text.lines().take(2000).any(|line| {
        split_key(line).is_some_and(|(key, rest)| {
            matches!(
                key.to_ascii_lowercase().as_str(),
                "host" | "match" | "include" | "hostname" | "identityfile" | "proxyjump"
            ) && !rest.is_empty()
        })
    })
}

/// Parses a config without a base directory: `Include` lines are reported
/// and skipped.
pub fn parse(bytes: &[u8]) -> ImportReport {
    run(bytes, None)
}

/// Parses a config; relative `Include` paths resolve against `base_dir`
/// (normally `~/.ssh`).
///
/// Included files must live inside `base_dir` (after resolving symlinks);
/// anything else is skipped with a warning. Wildcards are supported in the
/// last path component only (`config.d/*.conf`). A leading `~/.ssh/` is read
/// as `base_dir`.
pub fn parse_with_base(bytes: &[u8], base_dir: &Path) -> ImportReport {
    run(bytes, Some(base_dir))
}

fn run(bytes: &[u8], base: Option<&Path>) -> ImportReport {
    let mut ctx = Ctx {
        base: base.map(Path::to_path_buf),
        canon_base: base.and_then(|b| b.canonicalize().ok()),
        blocks: vec![Block {
            patterns: vec!["*".into()],
            is_match: false,
            opts: Vec::new(),
        }],
        report: ImportReport::default(),
        files_read: 0,
        match_line: None,
    };
    let text = crate::util::decode_text(bytes);
    read_config(&mut ctx, &text, 0, None, 0);
    ctx.finish()
}

struct Opt {
    key: String,
    args: Vec<String>,
    line: usize,
    file: Option<String>,
}

struct Block {
    patterns: Vec<String>,
    is_match: bool,
    opts: Vec<Opt>,
}

struct Ctx {
    base: Option<PathBuf>,
    canon_base: Option<PathBuf>,
    blocks: Vec<Block>,
    report: ImportReport,
    files_read: usize,
    match_line: Option<usize>,
}

impl Ctx {
    fn warn(&mut self, file: Option<&str>, line: usize, message: impl Into<String>) {
        self.report.warn(ImportWarning {
            line: Some(line),
            record: file.map(str::to_owned),
            message: message.into(),
        });
    }
}

/// Splits `Key value`, `Key=value` and `Key = value`.
fn split_key(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}');
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let (key, rest) = line.split_at(end);
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest).trim_start();
    Some((key, rest.trim_end()))
}

/// Splits arguments on whitespace honouring double quotes; an unquoted `#`
/// at the start of a token begins a comment. The flag reports an
/// unterminated quote.
fn tokenize(text: &str) -> (Vec<String>, bool) {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_token = false;
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' => quoted = false,
                '\\' if chars.peek() == Some(&'"') => {
                    cur.push('"');
                    chars.next();
                }
                _ => cur.push(c),
            }
        } else if c == '"' {
            quoted = true;
            in_token = true;
        } else if c.is_whitespace() {
            if in_token {
                tokens.push(std::mem::take(&mut cur));
                in_token = false;
            }
        } else if c == '#' && !in_token {
            break;
        } else {
            cur.push(c);
            in_token = true;
        }
    }
    if in_token {
        tokens.push(cur);
    }
    (tokens, quoted)
}

fn read_config(ctx: &mut Ctx, text: &str, depth: usize, file: Option<&str>, start: usize) {
    let mut cur = start;
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let Some((key, rest)) = split_key(raw) else {
            continue;
        };
        let (args, unterminated) = tokenize(rest);
        if unterminated {
            ctx.warn(file, number, format!("unterminated quote in {key}"));
        }
        let key = key.to_ascii_lowercase();
        match key.as_str() {
            "host" => {
                ctx.blocks.push(Block {
                    patterns: args,
                    is_match: false,
                    opts: Vec::new(),
                });
                cur = ctx.blocks.len() - 1;
            }
            "match" => {
                ctx.match_line.get_or_insert(number);
                ctx.blocks.push(Block {
                    patterns: Vec::new(),
                    is_match: true,
                    opts: Vec::new(),
                });
                cur = ctx.blocks.len() - 1;
            }
            "include" => {
                for arg in &args {
                    include(ctx, arg, depth, cur, file, number);
                }
            }
            _ => {
                if args.is_empty() {
                    ctx.warn(file, number, format!("{key} has no value"));
                    continue;
                }
                ctx.blocks[cur].opts.push(Opt {
                    key,
                    args,
                    line: number,
                    file: file.map(str::to_owned),
                });
            }
        }
    }
}

fn include(ctx: &mut Ctx, arg: &str, depth: usize, cur: usize, file: Option<&str>, line: usize) {
    if depth >= MAX_INCLUDE_DEPTH {
        ctx.warn(
            file,
            line,
            format!("Include {arg} skipped: nesting deeper than {MAX_INCLUDE_DEPTH}"),
        );
        return;
    }
    let (Some(base), Some(canon_base)) = (ctx.base.clone(), ctx.canon_base.clone()) else {
        ctx.warn(
            file,
            line,
            format!("Include {arg} skipped: no base directory given"),
        );
        return;
    };
    let relative = arg
        .strip_prefix("~/.ssh/")
        .or_else(|| arg.strip_prefix("~\\.ssh\\"))
        .unwrap_or(arg);
    if relative.starts_with('~') {
        ctx.warn(
            file,
            line,
            format!("Include {arg} skipped: outside {base:?}"),
        );
        return;
    }
    let full = base.join(relative);
    let name = full
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let wildcard = name.contains(['*', '?']);
    let parent = full.parent().unwrap_or(&base).to_path_buf();
    if parent.to_string_lossy().contains(['*', '?']) {
        ctx.warn(
            file,
            line,
            format!("Include {arg} skipped: wildcards are supported in the file name only"),
        );
        return;
    }
    let mut candidates = Vec::new();
    if wildcard {
        if let Ok(entries) = std::fs::read_dir(&parent) {
            for entry in entries.flatten() {
                let entry_name = entry.file_name().to_string_lossy().into_owned();
                if glob_match(&name, &entry_name, false) {
                    candidates.push(entry.path());
                }
            }
        }
        candidates.sort();
    } else {
        candidates.push(full);
    }
    for path in candidates {
        let Ok(canon) = path.canonicalize() else {
            if !wildcard {
                ctx.warn(file, line, format!("Include {arg}: file not found"));
            }
            continue;
        };
        if !canon.starts_with(&canon_base) {
            ctx.warn(
                file,
                line,
                format!("Include {arg} skipped: resolves outside {base:?}"),
            );
            continue;
        }
        if !canon.is_file() {
            continue;
        }
        if ctx.files_read >= MAX_FILES {
            ctx.warn(file, line, "too many included files; the rest are skipped");
            return;
        }
        let label = canon
            .strip_prefix(&canon_base)
            .unwrap_or(&canon)
            .to_string_lossy()
            .into_owned();
        match std::fs::metadata(&canon) {
            Ok(meta) if meta.len() > MAX_FILE_BYTES => {
                ctx.warn(file, line, format!("{label} skipped: larger than 4 MiB"));
                continue;
            }
            Err(error) => {
                ctx.warn(file, line, format!("{label}: {error}"));
                continue;
            }
            _ => {}
        }
        match std::fs::read(&canon) {
            Ok(bytes) => {
                ctx.files_read += 1;
                let text = crate::util::decode_text(&bytes);
                read_config(ctx, &text, depth + 1, Some(&label), cur);
            }
            Err(error) => ctx.warn(file, line, format!("{label}: {error}")),
        }
    }
}

/// `*` and `?` glob match without recursion.
pub(crate) fn glob_match(pattern: &str, text: &str, ignore_case: bool) -> bool {
    let norm = |c: char| {
        if ignore_case {
            c.to_ascii_lowercase()
        } else {
            c
        }
    };
    let pattern: Vec<char> = pattern.chars().map(norm).collect();
    let text: Vec<char> = text.chars().map(norm).collect();
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((sp, st)) = star {
            p = sp + 1;
            t = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

fn block_matches(block: &Block, alias: &str) -> bool {
    if block.is_match {
        return false;
    }
    let mut positive = false;
    for pattern in &block.patterns {
        if let Some(negated) = pattern.strip_prefix('!') {
            if glob_match(negated, alias, true) {
                return false;
            }
        } else if glob_match(pattern, alias, true) {
            positive = true;
        }
    }
    positive
}

fn is_wildcard(pattern: &str) -> bool {
    pattern.contains(['*', '?'])
}

impl Ctx {
    /// First matching option, per ssh_config first-match-wins.
    fn first<'a>(&'a self, alias: &str, key: &str) -> Option<&'a Opt> {
        self.blocks
            .iter()
            .filter(|block| block_matches(block, alias))
            .flat_map(|block| block.opts.iter())
            .find(|opt| opt.key == key)
    }

    fn all<'a>(&'a self, alias: &str, key: &str) -> Vec<&'a Opt> {
        self.blocks
            .iter()
            .filter(|block| block_matches(block, alias))
            .flat_map(|block| block.opts.iter())
            .filter(|opt| opt.key == key)
            .collect()
    }

    fn finish(mut self) -> ImportReport {
        if let Some(line) = self.match_line {
            self.report.warn(ImportWarning::at_line(
                line,
                "Match blocks are not evaluated; their options are ignored and every host is \
                 marked as needing the OpenSSH engine",
            ));
        }
        let mut aliases: Vec<String> = Vec::new();
        for block in self.blocks.iter().filter(|block| !block.is_match) {
            for pattern in &block.patterns {
                if pattern.starts_with('!') || is_wildcard(pattern) || pattern.is_empty() {
                    continue;
                }
                if !aliases.iter().any(|a| a.eq_ignore_ascii_case(pattern)) {
                    aliases.push(pattern.clone());
                }
            }
        }
        for alias in aliases {
            let host = self.build_host(&alias);
            self.report.push_host(host);
        }
        self.report.finish()
    }

    fn build_host(&mut self, alias: &str) -> ImportedHost {
        let mut warnings: Vec<ImportWarning> = Vec::new();
        let mut host = ImportedHost::new(alias, alias);

        if let Some(opt) = self.first(alias, "hostname") {
            host.hostname = opt.args[0].replace("%h", alias).replace("%%", "%");
        }
        if let Some(opt) = self.first(alias, "port") {
            match opt.args[0].parse::<u16>() {
                Ok(port) => host.port = port,
                Err(_) => warnings.push(ImportWarning {
                    line: Some(opt.line),
                    record: opt.file.clone().or_else(|| Some(alias.to_owned())),
                    message: format!("invalid Port {:?}; using 22", opt.args[0]),
                }),
            }
        }
        if let Some(opt) = self.first(alias, "user") {
            host.username = Some(opt.args[0].clone());
        }

        let identities: Vec<&str> = self
            .all(alias, "identityfile")
            .iter()
            .map(|opt| opt.args[0].as_str())
            .filter(|path| !path.eq_ignore_ascii_case("none"))
            .collect();
        if let Some(first) = identities.first() {
            host.identity_file = Some(PathBuf::from(first));
        }
        let mut notes: Vec<String> = identities
            .iter()
            .skip(1)
            .map(|path| format!("Additional IdentityFile: {path}"))
            .collect();

        if let Some(opt) = self.first(alias, "proxyjump") {
            let value = opt.args.join(" ");
            if !value.eq_ignore_ascii_case("none") {
                host.jump = value
                    .split(',')
                    .map(str::trim)
                    .filter(|hop| !hop.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
        }

        for (key, name) in OPENSSH_ONLY {
            if let Some(opt) = self.first(alias, key) {
                if opt.args[0].eq_ignore_ascii_case("none") {
                    continue;
                }
                host.openssh_only.push((*name).to_owned());
            }
        }
        if self
            .first(alias, "permitlocalcommand")
            .is_some_and(|opt| opt.args[0].eq_ignore_ascii_case("yes"))
        {
            host.openssh_only.push("PermitLocalCommand".to_owned());
        }
        if self.match_line.is_some() {
            host.openssh_only.push("Match".to_owned());
        }

        for (key, name) in NOTE_KEYS {
            for opt in self.all(alias, key) {
                notes.push(format!("{name} {}", opt.args.join(" ")));
            }
        }
        host.notes = notes.join("\n");
        self.report.warnings.extend(warnings);
        host
    }
}
