//! Shared admission-floor lint and frozen verifier shell grammar.
use crate::{Result, error, require};
use std::collections::BTreeSet;

pub fn split(command: &str, punctuation: bool) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        if escaped {
            current.push(c);
            escaped = false;
            started = true;
            continue;
        }
        if quote == Some('\'') {
            if c == '\'' {
                quote = None;
            } else {
                current.push(c);
            }
            continue;
        }
        if quote == Some('"') {
            match c {
                '"' => quote = None,
                '\\' => match chars.peek().copied() {
                    Some('"' | '\\' | '$' | '`') => escaped = true,
                    _ => current.push('\\'),
                },
                _ => current.push(c),
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                started = true;
            }
            '\\' => {
                escaped = true;
                started = true;
            }
            c if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            ';' | '&' | '|' if punctuation => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
                let mut run = c.to_string();
                while chars.peek().is_some_and(|c| ";&|".contains(*c)) {
                    run.push(chars.next().unwrap());
                }
                words.push(run);
            }
            _ => {
                current.push(c);
                started = true;
            }
        }
    }
    require(
        quote.is_none() && !escaped,
        "unclosed shell quote or escape",
    )?;
    if started {
        words.push(current);
    }
    Ok(words)
}
fn spelling(mut value: &str) -> &str {
    while let Some(s) = value.strip_prefix("./") {
        value = s;
    }
    value
}
pub fn incompatible_reads(command: &str, allowed: &[&str]) -> Vec<(String, String)> {
    lint(command, allowed, 0)
}
fn lint(command: &str, allowed: &[&str], depth: usize) -> Vec<(String, String)> {
    if depth > 64 {
        return vec![];
    }
    let Ok(words) = split(command, false) else {
        return vec![];
    };
    let mut tokens = Vec::new();
    for word in words {
        if word.chars().any(char::is_whitespace) {
            tokens.push(word);
            continue;
        }
        let mut at = 0;
        let chars = word.char_indices().collect::<Vec<_>>();
        let mut kind = None;
        for (i, c) in chars {
            let now = ";&|".contains(c);
            if kind.is_some_and(|old| old != now) {
                tokens.push(word[at..i].to_owned());
                at = i;
            }
            kind = Some(now);
        }
        if at < word.len() {
            tokens.push(word[at..].to_owned());
        }
    }
    let boundaries = ["&&", "||", ";", "|", "&", ";;"];
    let mut hits = Vec::new();
    let readers = [
        "grep",
        "egrep",
        "fgrep",
        "rg",
        "sed",
        "awk",
        "cat",
        "head",
        "tail",
        "diff",
        "cmp",
        "sort",
        "wc",
        "od",
        "strings",
        "cut",
        "tr",
        "uniq",
        "jq",
        "sha256sum",
        "md5sum",
        "shasum",
        "cksum",
        "comm",
        "join",
        "paste",
        "nl",
        "tac",
        "rev",
        "xxd",
        "hexdump",
        "base64",
        "sha1sum",
        "sha224sum",
        "sha384sum",
        "sha512sum",
        "b2sum",
        "sum",
        "base32",
        "basenc",
    ];
    for command in tokens.split(|w| boundaries.contains(&w.as_str())) {
        let mut words = command;
        while words
            .first()
            .is_some_and(|s| s.split_once('=').is_some_and(|(a, _)| !a.contains('/')))
        {
            words = &words[1..];
        }
        while words.first().is_some_and(|s| {
            [
                "do", "then", "else", "elif", "if", "while", "until", "time", "!", "{",
            ]
            .contains(&s.as_str())
        }) {
            words = &words[1..];
        }
        if words.is_empty() {
            continue;
        }
        if words[0] == "command" && words.len() >= 2 && !matches!(words[1].as_str(), "-v" | "-V") {
            let at = if matches!(words[1].as_str(), "-p" | "--") {
                2
            } else {
                1
            };
            if words.get(at).is_some_and(|s| !s.starts_with('-')) {
                words = &words[at..];
            }
        } else if ["env", "/bin/env", "/usr/bin/env"].contains(&words[0].as_str()) {
            let mut at = 1;
            if words.get(at).is_some_and(|s| s == "--") {
                at += 1;
            } else if words.get(at).is_some_and(|s| s.starts_with('-')) {
                continue;
            }
            while words
                .get(at)
                .is_some_and(|s| s.split_once('=').is_some_and(|(n, _)| identifier(n)))
            {
                at += 1;
            }
            if words.get(at).is_some_and(|s| !s.starts_with('-')) {
                words = &words[at..];
            }
        }
        let head = words[0].rsplit('/').next().unwrap();
        if ["bash", "sh", "zsh"].contains(&head) && words.iter().take(3).any(|s| s == "-c") {
            let at = words.iter().position(|s| s == "-c").unwrap();
            if let Some(inner) = words.get(at + 1) {
                hits.extend(lint(inner, allowed, depth + 1));
            }
            continue;
        }
        for (i, word) in words.iter().enumerate() {
            let target = if word == "<" {
                words.get(i + 1).map(String::as_str)
            } else {
                word.strip_prefix('<').filter(|s| !s.is_empty())
            };
            if let Some(target) = target {
                let t = spelling(target);
                if allowed.iter().any(|a| spelling(a) == t) {
                    hits.push((t.into(), "stdin redirect".into()));
                }
            }
        }
        if readers.contains(&head) {
            for word in &words[1..] {
                let t = spelling(word);
                if allowed.iter().any(|a| spelling(a) == t) {
                    hits.push((t.into(), head.into()));
                }
            }
        }
    }
    hits
}
fn identifier(value: &str) -> bool {
    let mut b = value.bytes();
    b.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}
pub fn verifier_argv(command: &str, files: &[&str]) -> Result<Vec<Vec<String>>> {
    if command == "git diff --check" {
        return Ok(vec![vec!["git".into(), "diff".into(), "--check".into()]]);
    }
    let words = split(command, true)?;
    let mut commands = Vec::new();
    let mut used = BTreeSet::new();
    let mut connectors = BTreeSet::new();
    let mut at = 0;
    while at < words.len() {
        require(commands.len() < 16, "too many verifier invocations")?;
        let start = at;
        if words[at] == "if" {
            let tail = words
                .get(at..at + 10)
                .ok_or_else(|| error("unsupported conditional verifier"))?;
            require(
                tail[..3] == ["if", "test", "-f"]
                    && tail[4..7] == [";", "then", "bash"]
                    && tail[8..] == [";", "fi"]
                    && tail[3] == tail[7]
                    && files.contains(&tail[3].as_str())
                    && !tail[3].starts_with('-'),
                "unsupported conditional verifier",
            )?;
            used.insert(tail[3].clone());
            at += 10;
        } else if words[at] == "for" {
            require(
                words.get(at + 1).is_some_and(|s| identifier(s))
                    && words.get(at + 2).is_some_and(|s| s == "in"),
                "unsupported verifier loop",
            )?;
            let end = (at + 3..words.len())
                .find(|i| words[*i] == ";")
                .ok_or_else(|| error("unsupported verifier loop"))?;
            let paths = &words[at + 3..end];
            let var = format!("${}", words[at + 1]);
            let tail = [
                ";",
                "do",
                "if",
                "test",
                "-f",
                var.as_str(),
                ";",
                "then",
                "bash",
                var.as_str(),
                "||",
                "exit",
                ";",
                "fi",
                ";",
                "done",
            ];
            require(
                !paths.is_empty()
                    && paths.len() <= 64
                    && paths
                        .iter()
                        .all(|p| files.contains(&p.as_str()) && !p.starts_with('-'))
                    && words
                        .get(end..end + tail.len())
                        .is_some_and(|v| v.iter().map(String::as_str).eq(tail)),
                "unsupported verifier loop body",
            )?;
            used.extend(paths.iter().cloned());
            at = end + tail.len();
        } else {
            let end = (at..words.len())
                .find(|i| matches!(words[*i].as_str(), ";" | "&&"))
                .unwrap_or(words.len());
            let argv = &words[at..end];
            require(
                !argv.is_empty()
                    && argv.iter().all(|s| {
                        !s.is_empty()
                            && s.bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"_./:=,+@%-".contains(&b))
                    }),
                "unsupported verifier shell grammar",
            )?;
            require(
                argv.len() >= 2
                    && ["bash", "sh", "python3"].contains(&argv[0].as_str())
                    && files.contains(&argv[1].as_str())
                    && !argv[1].starts_with('-'),
                "verifier must execute a frozen repository-relative script, not an interpreter option",
            )?;
            used.insert(argv[1].clone());
            at = end;
        }
        commands.push(words[start..at].to_vec());
        if at < words.len() {
            require(
                matches!(words[at].as_str(), ";" | "&&"),
                "unsupported verifier connector",
            )?;
            connectors.insert(words[at].as_str());
            require(
                connectors.len() <= 1,
                "mixed verifier connectors can conceal a skipped verification",
            )?;
            at += 1;
            require(at < words.len(), "empty verifier invocation")?;
        }
    }
    require(
        !used.is_empty(),
        "verifier must execute at least one frozen script",
    )?;
    Ok(commands)
}
