//! Static selector checks. Repository suite definitions are data, never code
//! imported or executed during planning, dry runs or admission preflight.
use crate::{Error, Result, require, value};
use oms_command_plan::storage::Store;
use regex::Regex;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

fn read(store: &Store, path: &str) -> Result<String> {
    String::from_utf8(store.read(path, 4 * 1024 * 1024)?)
        .map_err(|_| Error::new(2, "verify suite is not UTF-8"))
}
fn smoke_cases(source: &str) -> Vec<String> {
    Regex::new(r"(?m)^(test_[A-Za-z0-9_]+)\(\) *\{")
        .unwrap()
        .captures_iter(source)
        .map(|c| c[1].into())
        .collect()
}
fn operator_sections(source: &str) -> BTreeMap<String, Option<String>> {
    let line =
        Regex::new(r#"^    \("([a-z/-]+)", [0-9]+, (?:"([A-Za-z0-9_.-]+)"|(None))\),$"#).unwrap();
    let mut rows = BTreeMap::new();
    let mut inside = false;
    for raw in source.lines() {
        if raw == "SECTIONS = (" {
            inside = true;
            continue;
        }
        if inside && raw == ")" {
            break;
        }
        if inside && let Some(m) = line.captures(raw) {
            rows.insert(m[1].to_owned(), m.get(2).map(|s| s.as_str().to_owned()));
        }
    }
    rows
}
fn case_names(source: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(|line| line.strip_prefix("# BEGIN "))
        .map(|name| {
            name.split(" (")
                .next()
                .unwrap_or("")
                .split('.')
                .next()
                .unwrap_or("")
                .trim()
                .to_owned()
        })
        .collect()
}
pub fn validate(repo: &Path, command: &str) -> Result<()> {
    validate_with_continuation(repo, command, None)
}
/// The parent supplies raw continuation bytes and their reviewed digest. Only
/// added function definitions in the corresponding suite extend selector data.
pub fn validate_with_continuation(
    repo: &Path,
    command: &str,
    continuation: Option<(&[u8], &str)>,
) -> Result<()> {
    if command.trim().is_empty() {
        return Ok(());
    }
    let Ok(words) = oms_command_plan::floor::split(command, true) else {
        return Ok(());
    };
    let store = Store::open(repo)?;
    let mut added: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Some((patch, expected)) = continuation {
        require(
            value::digest(expected) && value::hash(patch) == expected,
            "continuation patch digest changed",
        )?;
        let text = std::str::from_utf8(patch)
            .map_err(|_| Error::new(2, "continuation patch is not UTF-8"))?;
        let mut current = None;
        let mut in_hunk = false;
        let definition = Regex::new(r"^\+(test_[A-Za-z0-9_]+)\(\) *\{").unwrap();
        for line in text.lines() {
            if line.starts_with("diff --git ") {
                current = None;
                in_hunk = false;
            } else if let Some(path) = line.strip_prefix("+++ b/") {
                current = crate::contract::path(path.split('\t').next().unwrap_or("")).ok();
            } else if line.starts_with("@@ ") {
                in_hunk = true;
            } else if in_hunk
                && let (Some(path), Some(found)) = (&current, definition.captures(line))
            {
                added.entry(path.clone()).or_default().push(found[1].into());
            }
        }
    }
    let suite_pattern = Regex::new(r"^(?:tests|scripts)/[A-Za-z0-9_.-]+\.sh$").unwrap();
    let section_re = Regex::new(r"(?m)^# OPERATOR_SECTION_BEGIN ([a-z_-]+)$").unwrap();
    let separators = ["&&", "||", ";", "|", "&", ";;", "(", ")"];
    for (index, word) in words.iter().enumerate().take(words.len().saturating_sub(1)) {
        if word != "bash" || !suite_pattern.is_match(&words[index + 1]) {
            continue;
        }
        let suite = &words[index + 1];
        let source = read(&store, suite)?;
        let end = (index + 2..words.len())
            .find(|i| separators.contains(&words[*i].as_str()))
            .unwrap_or(words.len());
        let sections = words[index + 2..end]
            .windows(2)
            .filter(|w| w[0] == "--section")
            .map(|w| w[1].clone())
            .collect::<Vec<_>>();
        let mut at = index + 2;
        let mut seen_section = false;
        while at + 1 < end {
            let option = words[at].as_str();
            let selected = &words[at + 1];
            let valid = if option == "--section" && suite == "tests/operator-tools-smoke.sh" {
                require(
                    !seen_section,
                    "--verify repeats --section; use one comma-separated list",
                )?;
                seen_section = true;
                let mut names = section_re
                    .captures_iter(&source)
                    .map(|c| c[1].replace('_', "-"))
                    .filter(|s| !["core", "shared-cache"].contains(&s.as_str()))
                    .collect::<Vec<_>>();
                for name in operator_sections(&read(&store, "tests/operator/runner.py")?).keys() {
                    if !names.contains(name) {
                        names.push(name.clone());
                    }
                }
                Some(names)
            } else if option == "--case" && suite == "tests/operator-tools-smoke.sh" {
                let manifest = operator_sections(&read(&store, "tests/operator/runner.py")?);
                if sections.len() != 1 {
                    None
                } else if let Some(file) = manifest.get(&sections[0]) {
                    let body = if let Some(file) = file {
                        if file.ends_with(".sh") {
                            String::new()
                        } else {
                            read(&store, &format!("tests/operator/{file}"))?
                        }
                    } else {
                        source
                            .split_once("# OPERATOR_SECTION_BEGIN dispatch_exit\n")
                            .map(|(_, body)| {
                                body.split("# OPERATOR_SECTION_END dispatch_exit\n")
                                    .next()
                                    .unwrap_or("")
                                    .to_owned()
                            })
                            .unwrap_or_default()
                    };
                    Some(case_names(&body))
                } else {
                    None
                }
            } else if option == "--only"
                && (suite == "tests/run-smoke-shard.sh" || source.contains("run-smoke-shard.sh"))
            {
                let file = if suite == "tests/run-smoke-shard.sh" {
                    "tests/scripts-smoke.sh"
                } else {
                    suite.as_str()
                };
                let mut names = smoke_cases(&read(&store, file)?);
                if let Some(more) = added.get(file) {
                    names.extend(more.clone());
                }
                Some(names)
            } else {
                None
            };
            if let Some(valid) = valid {
                let requested = if option == "--case" {
                    vec![selected.as_str()]
                } else if option == "--section" {
                    selected.split(',').collect::<Vec<_>>()
                } else if selected.starts_with("test_") {
                    selected
                        .split(' ')
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                } else {
                    vec![selected.as_str()]
                };
                let unknown = requested
                    .into_iter()
                    .filter(|s| !valid.iter().any(|name| name == s))
                    .collect::<Vec<_>>();
                require(
                    unknown.is_empty(),
                    format!(
                        "unknown {option} value(s): {}; valid names: {}",
                        unknown.join(", "),
                        valid.join(", ")
                    ),
                )?;
                at += 2;
            } else {
                at += 1;
            }
        }
    }
    Ok(())
}
pub fn candidate_only_targets(
    repo: &Path,
    command: &str,
    changed: &[String],
    interrupted: &AtomicBool,
) -> Result<Vec<String>> {
    let words = match oms_command_plan::floor::split(command, true) {
        Ok(words) => words,
        Err(_) => return Ok(vec![]),
    };
    if words.is_empty()
        || words
            .iter()
            .any(|s| s.chars().any(|c| ";&|()<>$`\n".contains(c)))
    {
        return Ok(vec![]);
    }
    let (runner, args) = (words[0].as_str(), &words[1..]);
    let args = if runner == "node" && args.first().is_some_and(|s| s == "--test") {
        &args[1..]
    } else if runner == "pytest" {
        args
    } else {
        return Ok(vec![]);
    };
    if args.is_empty() {
        return Ok(vec![]);
    }
    let git = oms_command_plan::repository::Git {
        repo,
        interrupted,
        group_resolver: None,
    };
    let store = Store::open(repo)?;
    let mut result = vec![];
    for arg in args {
        if runner == "pytest" && ["-q", "-v", "--quiet", "--verbose"].contains(&arg.as_str()) {
            continue;
        }
        let path = arg.split("::").next().unwrap_or("");
        if crate::contract::path(path).ok().as_deref() != Some(path)
            || path.contains(['*', '?', '[', ']', '='])
            || path.starts_with(['-', '~'])
            || !changed.iter().any(|p| p == path)
            || store.read(path, 4 * 1024 * 1024).is_err()
        {
            return Ok(vec![]);
        }
        let listed = git.run(&["ls-tree", "--name-only", "HEAD", "--", path])?;
        if !listed.is_empty() {
            return Ok(vec![]);
        }
        result.push(format!("{arg} (file added by patch)"));
    }
    Ok(result)
}
