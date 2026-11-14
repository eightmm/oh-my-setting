//! Canonical ledger metric contracts, captured provenance and seed aggregation.
use crate::{Error, Result};
use crate::{store, value::*};
use oms_context::Json;
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const FIELDS: [&str; 5] = ["unit", "direction", "split", "aggregation", "eval_command"];
pub const CONTRACT_NAME: &str = "metric-contract.json";
pub fn contract_path(ledger: &Path) -> PathBuf {
    ledger
        .parent()
        .unwrap_or(Path::new("."))
        .join(CONTRACT_NAME)
}
fn bounded(v: &Json, name: &str) -> Result<String> {
    let text = text(v).ok_or_else(|| Error::usage(format!("{name} must be text")))?;
    if text.chars().count() > 200 || text.chars().any(|c| c < ' ' || c == '\x7f') {
        return Err(Error::usage(format!(
            "{name} must be one line of at most 200 characters"
        )));
    }
    Ok(text.into())
}
pub fn load(ledger: &Path) -> Result<Json> {
    load_in(ledger, &std::env::current_dir()?)
}
pub(crate) fn load_in(ledger: &Path, root: &Path) -> Result<Json> {
    let Some(bytes) = store::read_optional_in(&contract_path(ledger), 65536, root)? else {
        return Ok(Json::Object(vec![]));
    };
    let doc = parse(&bytes)?;
    let Some(Json::Object(items)) = get(&doc, "metrics") else {
        return Err(Error::usage("expected schema 1 metric contract"));
    };
    if get(&doc, "schema").and_then(number) != Some(1.0) || items.len() > 50 {
        return Err(Error::usage("expected schema 1 and at most 50 metrics"));
    }
    let mut result = vec![];
    for (name, spec) in items {
        let name = bounded(&Json::String(name.clone()), "metric name")?;
        if !matches!(spec, Json::Object(_)) {
            return Err(Error::usage("metric spec must be an object"));
        }
        let mut fields = vec![];
        for key in FIELDS {
            fields.push((
                key.as_bytes().to_vec(),
                string(bounded(get(spec, key).unwrap_or(&string("")), key)?),
            ));
        }
        if !["min", "max"].contains(&s(spec, "direction").as_str()) {
            return Err(Error::usage("metric direction must be min or max"));
        }
        let mut code = BTreeSet::new();
        if let Some(paths) = get(spec, "eval_code") {
            let Json::Array(paths) = paths else {
                return Err(Error::usage("eval_code must be a list"));
            };
            if paths.len() > 20 {
                return Err(Error::usage("eval_code has more than 20 paths"));
            }
            for path in paths {
                let path = bounded(path, "eval_code path")?;
                if path.is_empty()
                    || Path::new(&path).is_absolute()
                    || path.replace('\\', "/").split('/').any(|x| x == "..")
                {
                    return Err(Error::usage(
                        "eval_code paths must be repository-relative without ..",
                    ));
                }
                code.insert(path);
            }
        }
        fields.push((
            b"eval_code".to_vec(),
            Json::Array(code.iter().map(string).collect()),
        ));
        result.push((name.into_bytes(), Json::Object(fields)));
    }
    Ok(Json::Object(result))
}
fn file_digest(root: &Path, relative: &str) -> String {
    let read = || -> Result<String> {
        let root = root.canonicalize()?;
        let path = root.join(relative).canonicalize()?;
        if !path.starts_with(&root) {
            return Err(Error::usage("eval_code outside root"));
        }
        let mut file =
            store::open(&path, false)?.ok_or_else(|| Error::usage("eval_code is missing"))?;
        let before = file.metadata()?;
        if !before.is_file() {
            return Err(Error::usage("eval_code is not regular"));
        }
        let mut h = Sha256::new();
        let sampled = before.len() > 8 * 1024 * 1024;
        if sampled {
            let mut bytes = vec![0; 65536];
            file.read_exact(&mut bytes)?;
            h.update(&bytes);
            file.seek(SeekFrom::End(-65536))?;
            file.read_exact(&mut bytes)?;
            h.update(&bytes);
        } else {
            let mut remaining = 8 * 1024 * 1024;
            let mut block = [0; 65536];
            while remaining > 0 {
                let n = file.read(&mut block[..remaining.min(65536)])?;
                if n == 0 {
                    break;
                }
                h.update(&block[..n]);
                remaining -= n;
            }
        }
        let after = file.metadata()?;
        file.verify_unchanged()?;
        if before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            return Err(Error::usage("eval_code changed during read"));
        }
        let sha = format!("{:x}", h.finalize());
        Ok(if sampled {
            format!("sample:{}:{sha}", before.len())
        } else {
            sha
        })
    };
    read().unwrap_or_else(|_| "missing".into())
}
pub fn record(spec: &Json, root: &Path) -> Result<Json> {
    let mut code = Json::Object(vec![]);
    if let Some(Json::Array(paths)) = get(spec, "eval_code") {
        for path in paths {
            if let Some(p) = text(path) {
                put(&mut code, p, string(file_digest(root, p)));
            }
        }
    }
    let mut body = Json::Object(vec![]);
    for key in FIELDS {
        put(&mut body, key, copy(get(spec, key).unwrap_or(&string(""))));
    }
    put(&mut body, "eval_code", copy(&code));
    let mut result = Json::Object(vec![]);
    for key in ["unit", "direction", "split", "aggregation"] {
        if get(spec, key).is_some_and(truth) {
            put(&mut result, key, copy(get(spec, key).unwrap()));
        }
    }
    if matches!(&code,Json::Object(x) if !x.is_empty()) {
        put(
            &mut result,
            "eval_sha256",
            string(hash(&encode(&code, true, true)?)),
        );
    }
    put(
        &mut result,
        "digest",
        string(hash(&encode(&body, false, true)?)),
    );
    Ok(result)
}
pub fn row_contract(ledger: &Path, root: &Path) -> Result<Option<Json>> {
    let doc = load_in(ledger, root)?;
    let Json::Object(items) = &doc else {
        unreachable!()
    };
    if items.is_empty() {
        return Ok(None);
    }
    Ok(Some(Json::Object(
        items
            .iter()
            .map(|(key, spec)| Ok((key.clone(), record(spec, root)?)))
            .collect::<Result<_>>()?,
    )))
}
fn signed(s: &str) -> bool {
    let v = s.strip_prefix('-').unwrap_or(s);
    !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())
}
fn canonical_integer(s: &str) -> String {
    let negative = s.starts_with('-');
    let n = s.trim_start_matches('-').trim_start_matches('0');
    if n.is_empty() {
        "0".into()
    } else {
        format!("{}{n}", if negative { "-" } else { "" })
    }
}
pub fn seed_and_group(
    command: &Json,
    sha: &str,
    diff: &str,
    explicit: &str,
    label: &str,
) -> Result<(Option<Json>, Option<String>)> {
    let Some(words) = (if let Json::Array(a) = command {
        Some(a)
    } else {
        None
    }) else {
        return Err(Error::usage("command must be an array"));
    };
    let re = Regex::new(r"^(?:--?seed|seed)=(-?\d+)$").unwrap();
    let mut found = None;
    let mut rest = vec![];
    let mut i = 0;
    while i < words.len() {
        let word = display(&words[i]);
        if let Some(cap) = re.captures(&word) {
            found = Some(canonical_integer(&cap[1]));
            i += 1;
            continue;
        }
        if ["--seed", "-seed"].contains(&word.as_str())
            && i + 1 < words.len()
            && signed(&display(&words[i + 1]))
        {
            found = Some(canonical_integer(&display(&words[i + 1])));
            i += 2;
            continue;
        }
        rest.push(copy(&words[i]));
        i += 1;
    }
    if !explicit.is_empty() {
        if !signed(explicit) {
            return Err(Error::usage("--seed requires an integer"));
        }
        found = Some(canonical_integer(explicit));
    }
    if found
        .as_ref()
        .is_some_and(|n| n.trim_start_matches('-').len() > 4300)
    {
        return Err(Error::usage("seed exceeds the integer digit limit"));
    }
    let group = if !label.is_empty() {
        Some(label.into())
    } else if found.is_some() {
        Some(
            hash(&encode(
                &Json::Array(vec![Json::Array(rest), string(sha), string(diff)]),
                true,
                true,
            )?)[..12]
                .into(),
        )
    } else {
        None
    };
    Ok((found.map(Json::Number), group))
}
fn hex(s: &str, lengths: &[usize]) -> bool {
    lengths.contains(&s.len())
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn keys(v: &Json, names: &[&str]) -> bool {
    if let Json::Object(o) = v {
        o.len() == names.len()
            && o.iter()
                .all(|(k, _)| names.iter().any(|n| k == n.as_bytes()))
    } else {
        false
    }
}
fn count(v: Option<&Json>, max: u64) -> Option<u64> {
    let n = integer(v?)?.parse::<u64>().ok()?;
    if n <= max { Some(n) } else { None }
}
/// Fingerprint captured source metadata without reopening configs or datasets.
pub fn make_input_identity(
    git: &Json,
    configs: &Json,
    data: &Json,
    expected_configs: &Json,
) -> Result<Json> {
    let commit = get(git, "commit_full")
        .and_then(text)
        .ok_or_else(|| Error::usage("input identity lacks a git record"))?;
    let dirty = count(get(git, "dirty"), 1_000_000_000)
        .ok_or_else(|| Error::usage("input identity has invalid git metadata"))?;
    let diff = get(git, "diff_sha256")
        .filter(|v| truth(v))
        .map(|v| text(v).ok_or_else(|| Error::usage("input identity has invalid git metadata")))
        .transpose()?
        .unwrap_or("");
    if !(hex(commit, &[40, 64]) || ["none", "no-commit"].contains(&commit))
        || (!diff.is_empty() && !hex(diff, &[16, 64]))
    {
        return Err(Error::usage("input identity has invalid git metadata"));
    }
    let Json::Array(expected) = expected_configs else {
        return Err(Error::usage(
            "input identity has invalid declared config paths",
        ));
    };
    if expected.len() > 128
        || expected
            .iter()
            .any(|v| !text(v).is_some_and(|s| !s.is_empty() && s.chars().count() <= 4096))
    {
        return Err(Error::usage(
            "input identity has invalid declared config paths",
        ));
    }
    fn hashes(
        rows: &Json,
        label: &str,
        hash_key: &str,
        lengths: &[usize],
    ) -> Result<BTreeMap<String, String>> {
        use unicode_general_category::{GeneralCategory, get_general_category};
        let Json::Array(rows) = rows else {
            return Err(Error::usage("input identity has an invalid source list"));
        };
        if rows.len() > 128 {
            return Err(Error::usage("input identity has an oversized source list"));
        }
        let mut found = BTreeMap::new();
        for row in rows {
            let name = get(row, label)
                .and_then(text)
                .ok_or_else(|| Error::usage("input identity lacks a bounded label"))?;
            let sha = get(row, hash_key)
                .and_then(text)
                .ok_or_else(|| Error::usage("input identity lacks a recorded digest"))?;
            if name.is_empty()
                || name.chars().count() > 4096
                || name.chars().any(|c| {
                    matches!(
                        get_general_category(c),
                        GeneralCategory::Control
                            | GeneralCategory::Format
                            | GeneralCategory::Surrogate
                            | GeneralCategory::PrivateUse
                            | GeneralCategory::Unassigned
                    )
                })
                || !hex(sha, lengths)
            {
                return Err(Error::usage(
                    "input identity lacks a bounded label or recorded digest",
                ));
            }
            if found.get(name).is_some_and(|old| old != sha) {
                return Err(Error::usage(
                    "input identity has conflicting duplicate sources",
                ));
            }
            found.insert(name.into(), sha.into());
        }
        Ok(found)
    }
    fn fingerprint(items: &BTreeMap<String, String>) -> Result<Json> {
        if items.is_empty() {
            Ok(Json::Null)
        } else {
            Ok(string(digest(&Json::Array(
                items
                    .iter()
                    .map(|(a, b)| Json::Array(vec![string(a), string(b)]))
                    .collect(),
            ))?))
        }
    }
    let cfg = hashes(configs, "path", "sha256", &[32, 64])?;
    let datasets = hashes(data, "name", "manifest_sha256", &[64])?;
    let expected = expected
        .iter()
        .map(|v| text(v).unwrap())
        .collect::<BTreeSet<_>>();
    let actual = cfg.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let complete = expected == actual;
    let mut body = object(vec![
        ("schema", num(1)),
        ("capture", string("pre-command")),
        (
            "git",
            object(vec![
                ("commit", string(commit)),
                ("dirty", num(dirty)),
                ("diff_sha256", string(diff)),
            ]),
        ),
        (
            "config_status",
            string(if !complete {
                "incomplete"
            } else if cfg.is_empty() {
                "unrecorded"
            } else {
                "recorded"
            }),
        ),
        (
            "data_status",
            string(if datasets.is_empty() {
                "unrecorded"
            } else {
                "recorded"
            }),
        ),
        (
            "config_sha256",
            if complete {
                fingerprint(&cfg)?
            } else {
                Json::Null
            },
        ),
        ("data_sha256", fingerprint(&datasets)?),
        ("recorded_configs", num(cfg.len())),
        ("recorded_data", num(datasets.len())),
    ]);
    let sha = digest(&body)?;
    put(&mut body, "digest", string(sha));
    Ok(body)
}
/// Allowlisted bridge for a native capsule producer; returns the canonical schema-1 envelope.
pub fn capture_for_ledger(
    git: &Json,
    configs: &Json,
    data: &Json,
    expected_configs: &Json,
    contract: &Json,
    seeds: &Json,
    capsule_id: &str,
) -> Result<Json> {
    captured_contract(contract)?;
    let value = object(vec![
        ("schema", num(1)),
        ("capsule_id", string(capsule_id)),
        (
            "inputs",
            make_input_identity(git, configs, data, expected_configs)?,
        ),
        (
            "metric_contract",
            if matches!(contract,Json::Object(o) if o.is_empty()) {
                Json::Null
            } else {
                copy(contract)
            },
        ),
        ("seeds", copy(seeds)),
    ]);
    decode_capture(&ordered(&value)?)
}
pub fn validate_identity(value: &Json) -> Result<()> {
    let names = [
        "schema",
        "capture",
        "git",
        "config_sha256",
        "data_sha256",
        "config_status",
        "data_status",
        "recorded_configs",
        "recorded_data",
        "digest",
    ];
    if !keys(value, &names)
        || get(value, "schema").and_then(integer) != Some("1")
        || s(value, "capture") != "pre-command"
    {
        return Err(Error::usage("invalid recorded input identity"));
    }
    let git = get(value, "git").unwrap();
    if !keys(git, &["commit", "dirty", "diff_sha256"]) {
        return Err(Error::usage("invalid recorded input identity git state"));
    }
    let commit = get(git, "commit")
        .and_then(text)
        .ok_or_else(|| Error::usage("invalid recorded input identity git state"))?;
    let diff = get(git, "diff_sha256")
        .and_then(text)
        .ok_or_else(|| Error::usage("invalid recorded input identity git state"))?;
    if !(hex(commit, &[40, 64]) || ["none", "no-commit"].contains(&commit))
        || count(get(git, "dirty"), 1_000_000_000).is_none()
        || (!diff.is_empty() && !hex(&diff, &[16, 64]))
    {
        return Err(Error::usage("invalid recorded input identity git state"));
    }
    for (prefix, ck) in [("config", "recorded_configs"), ("data", "recorded_data")] {
        let st = s(value, &format!("{prefix}_status"));
        let d = get(value, &format!("{prefix}_sha256")).unwrap();
        let n = count(get(value, ck), 128)
            .ok_or_else(|| Error::usage("invalid input identity coverage"))?;
        let allowed =
            st == "recorded" || st == "unrecorded" || (prefix == "config" && st == "incomplete");
        if !allowed
            || (st == "recorded" && (!text(d).is_some_and(|x| hex(x, &[64])) || n == 0))
            || (st != "recorded" && !matches!(d, Json::Null))
            || (st == "unrecorded" && n != 0)
        {
            return Err(Error::usage(
                "inconsistent recorded input identity coverage",
            ));
        }
    }
    let mut body = copy(value);
    remove(&mut body, "digest");
    if s(value, "digest") != digest(&body)? {
        return Err(Error::usage("recorded input identity digest mismatch"));
    }
    Ok(())
}
pub fn captured_contract(v: &Json) -> Result<()> {
    if matches!(v, Json::Null) {
        return Ok(());
    }
    let Json::Object(items) = v else {
        return Err(Error::usage("invalid captured metric contract"));
    };
    if items.len() > 50 {
        return Err(Error::usage("invalid captured metric contract"));
    }
    for (key, row) in items {
        bounded(&Json::String(key.clone()), "metric name")?;
        let Json::Object(fields) = row else {
            return Err(Error::usage("invalid captured metric record"));
        };
        if fields.iter().any(|(k, _)| {
            ![
                "unit",
                "direction",
                "split",
                "aggregation",
                "eval_sha256",
                "digest",
            ]
            .iter()
            .any(|n| k == n.as_bytes())
        }) || !hex(&s(row, "digest"), &[64])
            || !["min", "max"].contains(&s(row, "direction").as_str())
        {
            return Err(Error::usage("invalid captured metric record"));
        }
        for (_, value) in fields {
            bounded(value, "captured metric field")?;
        }
        if let Some(v) = get(row, "eval_sha256")
            && !text(v).is_some_and(|x| hex(x, &[64]))
        {
            return Err(Error::usage("invalid captured evaluation digest"));
        }
    }
    Ok(())
}
pub fn decode_capture(raw: &[u8]) -> Result<Json> {
    if raw.len() > 65536 {
        return Err(Error::usage("captured run metadata exceeds its limit"));
    }
    let v = strict(raw)?;
    if !keys(
        &v,
        &["schema", "capsule_id", "inputs", "metric_contract", "seeds"],
    ) || get(&v, "schema").and_then(integer) != Some("1")
    {
        return Err(Error::usage("invalid captured run envelope"));
    }
    let id = s(&v, "capsule_id");
    if !Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,159}$")
        .unwrap()
        .is_match(&id)
        || id.contains("..")
    {
        return Err(Error::usage("invalid captured run envelope"));
    }
    validate_identity(get(&v, "inputs").unwrap())?;
    captured_contract(get(&v, "metric_contract").unwrap())?;
    if !matches!(get(&v,"seeds"),Some(Json::Array(a)) if a.len()<=128 && a.iter().all(|v| text(v).is_some_and(|t| t.chars().count()<=100)))
    {
        return Err(Error::usage("invalid captured seed declarations"));
    }
    Ok(v)
}
pub fn attach_capture(row: &mut Json, capture: &Json) -> Result<()> {
    let checked = decode_capture(&ordered(capture)?)?;
    let capture = &checked;
    if !matches!(get(row,"cmd"),Some(Json::Array(words)) if words.iter().all(|v|matches!(v,Json::String(_))))
    {
        return Err(Error::usage("ledger row requires an exact command array"));
    }
    let identity = get(capture, "inputs").unwrap();
    let git = get(identity, "git").unwrap();
    for (dest, source) in [
        ("git_sha", "commit"),
        ("dirty", "dirty"),
        ("dirty_hash", "diff_sha256"),
    ] {
        put(row, dest, copy(get(git, source).unwrap()));
    }
    put(row, "capsule_id", copy(get(capture, "capsule_id").unwrap()));
    put(row, "input_identity", copy(identity));
    remove(row, "metric_contract");
    let contract = get(capture, "metric_contract").unwrap();
    if !matches!(contract, Json::Null) {
        put(row, "metric_contract", copy(contract));
    }
    let (mut seed, mut group) = seed_and_group(
        get(row, "cmd").unwrap(),
        &s(row, "git_sha"),
        &s(row, "dirty_hash"),
        "",
        "",
    )?;
    let mut status = if seed.is_some() {
        "command"
    } else {
        "unrecorded"
    };
    let mut seeds = vec![];
    if let Some(Json::Array(a)) = get(capture, "seeds") {
        for v in a {
            let text = display(v);
            if !seeds.contains(&text) {
                seeds.push(text);
            }
        }
    }
    if !seeds.is_empty() {
        if seeds.len() == 1 && Regex::new(r"^-?[0-9]{1,18}$").unwrap().is_match(&seeds[0]) {
            let explicit = canonical_integer(&seeds[0]);
            if seed.as_ref().is_some_and(|v| integer(v) != Some(&explicit)) {
                seed = None;
                group = None;
                status = "conflicting-declarations";
            } else {
                (seed, group) = seed_and_group(
                    get(row, "cmd").unwrap(),
                    &s(row, "git_sha"),
                    &s(row, "dirty_hash"),
                    &explicit,
                    "",
                )?;
                status = "declared";
            }
        } else {
            seed = None;
            group = None;
            status = "multiple-or-unrecognized";
        }
    }
    remove(row, "seed");
    remove(row, "group");
    put(row, "seed_status", string(status));
    if let Some(seed) = seed {
        put(row, "seed", seed);
        put(row, "group", string(group.unwrap()));
    }
    Ok(())
}
pub fn sanitize(raw: &[u8]) -> Result<Option<Json>> {
    let data = parse(raw)?;
    let Json::Object(items) = &data else {
        return Err(Error::usage("metrics file is not a JSON object; ignored"));
    };
    let mut out = Json::Object(vec![]);
    let mut count = 0;
    for (key, value) in items {
        if count >= 50 {
            break;
        }
        let key = clip(key, 200, false);
        let clean = match value {
            Json::Bool(_) => Some(copy(value)),
            Json::Number(n)
                if integer(value).is_some() || n.parse::<f64>().is_ok_and(f64::is_finite) =>
            {
                Some(copy(value))
            }
            Json::String(v) => Some(Json::String(clip(v, 200, true))),
            _ => None,
        };
        if let Some(v) = clean {
            let Json::Object(items) = &mut out else {
                unreachable!()
            };
            let exists = items.iter().any(|(k, _)| *k == key);
            if let Some((_, old)) = items.iter_mut().find(|(k, _)| *k == key) {
                *old = v;
            } else {
                items.push((key, v));
            }
            if !exists {
                count += 1;
            }
        }
    }
    if count == 0 { Ok(None) } else { Ok(Some(out)) }
}
pub fn row_digest(row: &Json, metric: &str) -> Option<String> {
    get(get(get(row, "metric_contract")?, metric)?, "digest")
        .and_then(text)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
pub type Ranked = Vec<(f64, Json)>;
pub fn comparable(
    rows: Ranked,
    metric: &str,
    current: Option<&Json>,
) -> (Ranked, BTreeMap<String, usize>) {
    let mut left = BTreeMap::new();
    let mut kept = vec![];
    if let Some(current) = current {
        for (v, row) in rows {
            if row_digest(&row, metric).as_deref() == Some(&s(current, "digest")) {
                kept.push((v, row));
                continue;
            }
            let entry = get(&row, "metric_contract").and_then(|v| get(v, metric));
            let reason = if let Some(entry) = entry.filter(|v| matches!(v, Json::Object(_))) {
                let fields = ["direction", "split", "unit", "aggregation", "eval_sha256"]
                    .into_iter()
                    .filter(|k| s(entry, k) != s(current, k))
                    .collect::<Vec<_>>();
                if fields.is_empty() {
                    "eval_command".into()
                } else {
                    fields.join(", ")
                }
            } else {
                "no contract recorded".into()
            };
            *left.entry(reason).or_default() += 1;
        }
    } else {
        for (_, row) in &rows {
            *left
                .entry(
                    row_digest(row, metric)
                        .map(|d| format!("contract {}", &d[..d.len().min(12)]))
                        .unwrap_or_else(|| "no contract recorded".into()),
                )
                .or_default() += 1;
        }
        if left.len() <= 1 {
            return (rows, BTreeMap::new());
        }
    }
    (kept, left)
}
pub fn data_comparable(rows: Ranked) -> (Ranked, BTreeMap<String, usize>) {
    let mut kept = vec![];
    let mut left = BTreeMap::new();
    let mut datasets = BTreeMap::new();
    for pair in rows {
        let identity = get(&pair.1, "input_identity");
        if identity.is_some_and(|v| validate_identity(v).is_err()) {
            *left.entry("invalid input identity".into()).or_default() += 1;
            continue;
        }
        let d = identity
            .map(|v| s(v, "data_sha256"))
            .filter(|v| v != "None");
        let key = d
            .filter(|v| !v.is_empty())
            .map(|d| format!("data {d}"))
            .unwrap_or_else(|| "data unrecorded".into());
        *datasets.entry(key).or_default() += 1;
        kept.push(pair);
    }
    if datasets.len() > 1 {
        left.extend(datasets);
        (vec![], left)
    } else {
        (kept, left)
    }
}
pub struct Group {
    pub label: String,
    pub n: usize,
    pub mean: f64,
    pub stdev: Option<f64>,
    pub ci: Option<[f64; 2]>,
    pub seeds: Vec<String>,
}
/// Python Random(int) uses MT19937 init_by_array and getrandbits rejection for choice.
struct Random {
    mt: [u32; 624],
    index: usize,
}
impl Random {
    fn new(seed: u64) -> Self {
        let mut r = Self {
            mt: [0; 624],
            index: 624,
        };
        r.mt[0] = 19650218;
        for i in 1..624 {
            r.mt[i] = 1812433253u32
                .wrapping_mul(r.mt[i - 1] ^ (r.mt[i - 1] >> 30))
                .wrapping_add(i as u32);
        }
        let keys = if seed >> 32 == 0 {
            vec![seed as u32]
        } else {
            vec![seed as u32, (seed >> 32) as u32]
        };
        let mut i = 1;
        let mut j = 0;
        for _ in 0..624 {
            r.mt[i] = (r.mt[i] ^ ((r.mt[i - 1] ^ (r.mt[i - 1] >> 30)).wrapping_mul(1664525)))
                .wrapping_add(keys[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= 624 {
                r.mt[0] = r.mt[623];
                i = 1;
            }
            if j >= keys.len() {
                j = 0;
            }
        }
        for _ in 0..623 {
            r.mt[i] = (r.mt[i] ^ ((r.mt[i - 1] ^ (r.mt[i - 1] >> 30)).wrapping_mul(1566083941)))
                .wrapping_sub(i as u32);
            i += 1;
            if i >= 624 {
                r.mt[0] = r.mt[623];
                i = 1;
            }
        }
        r.mt[0] = 0x80000000;
        r
    }
    fn next(&mut self) -> u32 {
        if self.index >= 624 {
            for i in 0..624 {
                let y = (self.mt[i] & 0x80000000) | (self.mt[(i + 1) % 624] & 0x7fffffff);
                self.mt[i] =
                    self.mt[(i + 397) % 624] ^ (y >> 1) ^ if y & 1 != 0 { 0x9908b0df } else { 0 };
            }
            self.index = 0;
        }
        let mut y = self.mt[self.index];
        self.index += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c5680;
        y ^= (y << 15) & 0xefc60000;
        y ^= y >> 18;
        y
    }
    fn choice(&mut self, n: usize) -> usize {
        let k = usize::BITS - n.leading_zeros();
        loop {
            let r = (self.next() >> (32 - k)) as usize;
            if r < n {
                return r;
            }
        }
    }
}
pub fn bootstrap(values: &[f64], seed_text: &str) -> Option<[f64; 2]> {
    if values.is_empty() {
        return None;
    }
    if values.len() == 1 {
        return Some([values[0], values[0]]);
    }
    let sha = hash(seed_text.as_bytes());
    let mut random = Random::new(u64::from_str_radix(&sha[..16], 16).unwrap());
    let mut estimates = Vec::with_capacity(2000);
    for _ in 0..2000 {
        let mut sum = 0.0;
        for _ in values {
            sum += values[random.choice(values.len())] / values.len() as f64;
        }
        estimates.push(sum);
    }
    estimates.sort_by(f64::total_cmp);
    Some([estimates[49], estimates[1949]])
}
fn integer_order(a: &str, b: &str) -> std::cmp::Ordering {
    let neg_a = a.starts_with('-');
    let neg_b = b.starts_with('-');
    if neg_a != neg_b {
        return if neg_a {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        };
    }
    let a = a.trim_start_matches('-');
    let b = b.trim_start_matches('-');
    let cmp = a.len().cmp(&b.len()).then(a.cmp(b));
    if neg_a { cmp.reverse() } else { cmp }
}
pub fn groups(rows: &[(f64, Json)], metric: &str) -> Result<(Vec<Group>, usize)> {
    let mut latest: BTreeMap<(String, String, String), f64> = BTreeMap::new();
    let mut scopes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut signatures = BTreeMap::new();
    let mut order = vec![];
    let mut loose = 0;
    for (value, row) in rows {
        let group = s(row, "group");
        let seed = get(row, "seed").and_then(integer);
        if !value.is_finite()
            || get(row, "group").and_then(text).is_none()
            || group.is_empty()
            || group.len() > 60
            || !group
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || seed.is_none()
        {
            loose += 1;
            continue;
        }
        let identity = get(row, "input_identity");
        if let Some(i) = identity {
            if validate_identity(i).is_err()
                || s(i, "config_status") == "incomplete"
                || ["none", "no-commit"].contains(&s(get(i, "git").unwrap(), "commit").as_str())
                || get(get(i, "git").unwrap(), "dirty").is_some_and(truth)
                    && s(get(i, "git").unwrap(), "diff_sha256").is_empty()
            {
                loose += 1;
                continue;
            }
        }
        let signature = Json::Array(vec![
            identity
                .map(|i| copy(get(i, "digest").unwrap()))
                .unwrap_or(Json::Null),
            row_digest(row, metric).map(string).unwrap_or(Json::Null),
        ]);
        let key = String::from_utf8(encode(&signature, false, false)?).unwrap();
        signatures.insert(key.clone(), signature);
        scopes.entry(group.clone()).or_default().insert(key.clone());
        if !order.contains(&(group.clone(), key.clone())) {
            order.push((group.clone(), key.clone()));
        }
        latest.insert((group, key, seed.unwrap().into()), *value);
    }
    let mut result = vec![];
    for (group, signature) in order {
        let mut values = latest
            .iter()
            .filter(|((g, s, _), _)| *g == group && *s == signature)
            .map(|((_, _, seed), v)| (seed.clone(), *v))
            .collect::<Vec<_>>();
        values.sort_by(|a, b| integer_order(&a.0, &b.0));
        let label = if scopes[&group].len() > 1 {
            format!(
                "{}@{}",
                group.chars().take(44).collect::<String>(),
                &digest(&signatures[&signature])?[..12]
            )
        } else {
            group
        };
        let seeds = values.iter().map(|(s, _)| s.clone()).collect::<Vec<_>>();
        let values = values.iter().map(|(_, v)| *v).collect::<Vec<_>>();
        let n = values.len();
        let mean = values.iter().map(|x| x / n as f64).sum::<f64>();
        let stdev = if n > 1 {
            Some(
                values
                    .iter()
                    .map(|v| (v - mean).powi(2) / (n - 1) as f64)
                    .sum::<f64>()
                    .sqrt(),
            )
        } else {
            None
        };
        result.push(Group {
            ci: if n > 1 {
                bootstrap(&values, &format!("{label}{metric}"))
            } else {
                None
            },
            label,
            n,
            mean,
            stdev,
            seeds,
        });
    }
    Ok((result, loose))
}
pub fn final_split_report(ledger: &Path) -> Result<Vec<String>> {
    final_split_report_in(ledger, &std::env::current_dir()?)
}
pub(crate) fn final_split_report_in(ledger: &Path, root: &Path) -> Result<Vec<String>> {
    let Some(bytes) = store::read_optional_in(&contract_path(ledger), 65536, root)? else {
        return Ok(vec![]);
    };
    let doc = parse(&bytes)?;
    let mut final_splits = BTreeSet::new();
    if let Some(splits) = get(&doc, "splits") {
        let Json::Object(items) = splits else {
            return Err(Error::usage("splits must be an object"));
        };
        if items.len() > 50 {
            return Err(Error::usage("more than 50 splits"));
        }
        for (key, spec) in items {
            let role = s(spec, "role");
            if !["selection", "final"].contains(&role.as_str()) {
                return Err(Error::usage("split role must be selection or final"));
            }
            if role == "final" {
                final_splits.insert(String::from_utf8_lossy(key).into_owned());
            }
        }
    }
    if final_splits.is_empty() {
        return Ok(vec![]);
    }
    let mut runs = 0;
    let mut variants = BTreeSet::new();
    let mut by_group: BTreeMap<String, usize> = BTreeMap::new();
    let mut names = BTreeSet::new();
    store::rows_in(ledger, true, root, |row| {
        let Some(Json::Object(m)) = get(&row, "metrics") else {
            return Ok(());
        };
        let Some(contract) = get(&row, "metric_contract") else {
            return Ok(());
        };
        let mut hit = false;
        for (key, v) in m {
            let key = String::from_utf8_lossy(key);
            let split = get(contract, &key)
                .map(|x| s(x, "split"))
                .unwrap_or_default();
            if number(v).is_some() && final_splits.contains(&split) {
                names.insert(split);
                hit = true;
            }
        }
        if hit {
            runs += 1;
            let group = s(&row, "group");
            if !group.is_empty() {
                *by_group.entry(group.clone()).or_default() += 1;
                variants.insert(group);
            } else {
                let signature = Json::Array(
                    ["cmd", "git_sha", "dirty_hash"]
                        .iter()
                        .map(|k| get(&row, k).map(copy).unwrap_or(Json::Null))
                        .collect(),
                );
                variants.insert(hash(&encode(&signature, true, true)?)[..12].into());
            }
        }
        Ok(())
    })?;
    if runs == 0 {
        return Ok(vec![]);
    }
    let label = names.iter().cloned().collect::<Vec<_>>().join(", ");
    let mut out = vec![format!(
        "advisory: {runs} recorded run(s) reported a metric on the final split {label}"
    )];
    for (group, n) in by_group.iter().take(10) {
        out.push(format!("  seed group {}: {n} run(s)", clean(group, 60)));
    }
    if by_group.len() > 10 {
        out.push(format!("  ... {} more seed group(s)", by_group.len() - 10));
    }
    let grouped: usize = by_group.values().sum();
    if grouped < runs {
        out.push(format!("  no seed group: {} run(s)", runs - grouped));
    }
    if variants.len() > 1 {
        out.push(format!(
            "advisory: {label} {} evaluated {runs} times across {} variants",
            if names.len() == 1 { "split" } else { "splits" },
            variants.len()
        ));
    }
    Ok(out)
}
