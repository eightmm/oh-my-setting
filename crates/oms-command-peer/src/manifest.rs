use crate::{
    Context, Error, Output, Result, V, cli::Args, fail, json, now, obj, out, safety, storage, text,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::Path,
};

/// CSV/TSV decoding includes quoted newlines and doubled quote escapes.
fn csv(input: &str, delimiter: char) -> Result<Vec<Vec<String>>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' if quoted => {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            }
            '"' if field.is_empty() => quoted = true,
            c if c == delimiter && !quoted => {
                row.push(std::mem::take(&mut field));
            }
            '\n' | '\r' if !quoted => {
                if ch == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                if !field.is_empty() || !row.is_empty() {
                    row.push(std::mem::take(&mut field));
                }
                rows.push(std::mem::take(&mut row));
            }
            c => field.push(c),
        }
    }
    if quoted {
        return fail("unterminated quoted CSV field");
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}
fn table(raw: &[u8]) -> Result<Vec<Vec<String>>> {
    let input = String::from_utf8_lossy(raw);
    let sample = input.chars().take(4096).collect::<String>();
    let tabs = sample.matches('\t').count();
    csv(
        &input,
        if tabs > 0 && tabs >= sample.matches(',').count() {
            '\t'
        } else {
            ','
        },
    )
}
fn set_hash(values: &BTreeSet<String>) -> String {
    storage::hash(
        values
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
            .as_bytes(),
    )
}
struct Split {
    row: V,
    ids: Vec<String>,
    keys: BTreeMap<String, BTreeSet<String>>,
    times: Vec<(f64, bool)>,
}
fn column(header: &[String], name: &str) -> Result<usize> {
    header
        .iter()
        .position(|h| h == name)
        .ok_or_else(|| Error::new(2, format!("column '{name}' not in split header")))
}
fn stamp(value: &str) -> Result<(f64, bool)> {
    if let Ok(n) = value.parse::<f64>() {
        if n.is_finite() {
            return Ok((n, false));
        }
        return fail("time values must be finite");
    }
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(value) {
        return Ok((
            t.timestamp() as f64 + t.timestamp_subsec_nanos() as f64 / 1e9,
            true,
        ));
    }
    if let Ok(t) = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S") {
        return Ok((t.and_utc().timestamp() as f64, false));
    }
    if let Ok(t) = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return Ok((
            t.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() as f64,
            false,
        ));
    }
    fail("time value must be numeric or ISO-8601")
}
fn fingerprint(cwd: &Path, label: &str, file: &str, manifest: &V) -> Result<Split> {
    let raw = storage::file(&cwd.join(file))?;
    let input = String::from_utf8_lossy(&raw);
    let named = text(manifest, "id_column");
    let indexed = text(manifest, "id_index");
    let columns = manifest
        .get("leakage_keys")
        .array()
        .iter()
        .filter_map(V::text)
        .collect::<Vec<_>>();
    let time = text(manifest, "time_column");
    let tabular =
        !named.is_empty() || !indexed.is_empty() || !columns.is_empty() || !time.is_empty();
    let rows = if tabular { table(&raw)? } else { Vec::new() };
    let header = rows.first().cloned().unwrap_or_default();
    let ci = if !named.is_empty() {
        Some(column(&header, named)?)
    } else if !indexed.is_empty() {
        Some(
            indexed
                .parse::<usize>()
                .map_err(|_| Error::new(2, "id-index must be a nonnegative integer"))?,
        )
    } else {
        None
    };
    let ids: Vec<String> = if let Some(ci) = ci {
        rows.iter()
            .skip(usize::from(!named.is_empty()))
            .filter_map(|r| r.get(ci))
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        input
            .lines()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect()
    };
    let unique = ids.iter().cloned().collect::<BTreeSet<_>>();
    let mut row = obj([
        ("label", label.into()),
        ("file", file.into()),
        ("sha256", storage::hash(&raw).into()),
        ("rows", V::Num(ids.len().to_string())),
        ("unique_ids", V::Num(unique.len().to_string())),
        ("id_sha256", set_hash(&unique).into()),
    ]);
    let mut key_records = obj([]);
    let mut keys = BTreeMap::new();
    for name in columns {
        let index = column(&header, name)?;
        let mut values = BTreeSet::new();
        let mut pairs = Vec::new();
        let mut empty = 0usize;
        for cells in rows.iter().skip(1) {
            let value = cells.get(index).map(|s| s.trim()).unwrap_or("");
            if value.is_empty() {
                empty += 1;
            } else {
                values.insert(value.to_owned());
            }
            if let Some(ci) = ci
                && let Some(id) = cells.get(ci).map(|s| s.trim()).filter(|s| !s.is_empty())
            {
                pairs.push(format!("{id}\t{value}"));
            }
        }
        let mut entry = obj([
            ("unique", V::Num(values.len().to_string())),
            ("sha256", set_hash(&values).into()),
            ("empty", V::Num(empty.to_string())),
        ]);
        if ci.is_some() {
            pairs.sort();
            entry.set("pair_sha256", storage::hash(pairs.join("\n").as_bytes()));
        }
        key_records.set(name, entry);
        keys.insert(name.to_owned(), values);
    }
    if !keys.is_empty() {
        row.set("keys", key_records);
    }
    let mut times = Vec::new();
    if !time.is_empty() {
        let index = column(&header, time)?;
        for cells in rows.iter().skip(1) {
            let value = cells
                .get(index)
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| Error::new(2, "time column has missing values"))?;
            times.push(stamp(value.trim())?);
        }
    }
    Ok(Split {
        row,
        ids,
        keys,
        times,
    })
}
fn pairs(values: Vec<String>) -> Result<Vec<(String, String)>> {
    values
        .into_iter()
        .map(|s| {
            s.split_once('=')
                .filter(|(a, b)| !a.is_empty() && !b.is_empty())
                .map(|(a, b)| (a.into(), b.into()))
                .ok_or_else(|| Error::new(2, "expected NAME=VALUE"))
        })
        .collect()
}
fn provenance(value: &str, maximum: usize) -> Result<()> {
    if value.trim().is_empty()
        || value.chars().count() > maximum
        || value.chars().any(char::is_control)
        || regex::Regex::new(r"[A-Za-z][A-Za-z0-9+.-]*://[^/\s]*@")
            .unwrap()
            .is_match(value)
    {
        return fail("invalid provenance text or URL userinfo");
    }
    safety::outbound(value.as_bytes())
}
pub(crate) fn run(argv: &[OsString], context: &Context<'_>) -> Result<Output> {
    let args = Args::parse(
        argv,
        &[
            "name",
            "split",
            "id-column",
            "id-index",
            "key-column",
            "policy",
            "time-column",
            "entity-column",
            "derive-cmd",
            "derive-script",
            "source",
            "source-digest",
            "split-rationale",
            "entity-kind",
            "note",
        ],
        &["json", "show-examples"],
    )?;
    if args.has("help") {
        return Ok(out(include_bytes!("help/data-manifest.txt").to_vec()));
    }
    args.exact_operands(1)?;
    let repo = storage::repository(context, context.cwd)?;
    let directory = Path::new(".oms/manifests");
    if args.action() == "list" {
        let mut values = Vec::new();
        for name in storage::names(&repo, directory, ".json")? {
            values.push(V::parse(&storage::read(
                &repo,
                &directory.join(name),
                false,
            )?)?);
        }
        return Ok(out(if args.has("json") {
            json(&V::Arr(values))?
        } else {
            values
                .iter()
                .map(|v| {
                    format!(
                        "{} {} {} split(s)\n",
                        text(v, "name"),
                        text(v, "ts"),
                        v.get("splits").array().len()
                    )
                })
                .collect::<String>()
                .into_bytes()
        }));
    }
    let name = args.required("name")?;
    if !storage::identifier(name) {
        return fail("invalid manifest name");
    }
    let path = directory.join(format!("{name}.json"));
    if args.action() == "show" {
        return Ok(out(storage::read(&repo, &path, false)?));
    }
    if args.action() == "create" {
        safety::parent()?;
        let files = pairs(args.all("split"))?;
        if files.is_empty() {
            return fail("create requires at least one --split LABEL=FILE");
        }
        let mut keys = args.all("key-column");
        let entities = args.all("entity-column");
        for e in &entities {
            if !keys.contains(e) {
                keys.push(e.clone());
            }
        }
        let policy = args.value("policy", "random");
        match policy {
            "random" => {}
            "group" | "scaffold" if !keys.is_empty() => {}
            "cold" if !entities.is_empty() => {}
            "time" if !args.get("time-column").is_empty() => {}
            _ => return fail("invalid split policy or missing policy columns"),
        }
        if args.has("time-column") && policy != "time" || !entities.is_empty() && policy != "cold" {
            return fail("time/entity columns need the matching policy");
        }
        if args.has("id-index") {
            args.get("id-index")
                .parse::<usize>()
                .map_err(|_| Error::new(2, "id-index must be nonnegative"))?;
        }
        let mut record = obj([
            ("schema", 1usize.into()),
            ("name", name.into()),
            ("ts", now().into()),
            ("note", args.get("note").into()),
            ("id_column", args.get("id-column").into()),
            ("id_index", args.get("id-index").into()),
            (
                "leakage_keys",
                V::Arr(keys.iter().map(|s| s.as_str().into()).collect()),
            ),
        ]);
        if args.has("policy") {
            record.set("policy", policy);
        }
        if args.has("time-column") {
            record.set("time_column", args.get("time-column"));
        }
        if !entities.is_empty() {
            record.set(
                "entity_columns",
                V::Arr(entities.iter().map(|s| s.as_str().into()).collect()),
            );
        }
        let mut sources = Vec::new();
        let mut seen = BTreeSet::new();
        let declared = pairs(args.all("source"))?;
        let digests = pairs(args.all("source-digest"))?;
        for (source, reference) in declared {
            if !storage::identifier(&source) || !seen.insert(source.clone()) {
                return fail("invalid or duplicate source name");
            }
            provenance(&reference, 200)?;
            let mut row = obj([
                ("name", source.clone().into()),
                (
                    "ref",
                    reference
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .into(),
                ),
            ]);
            for (key, hash) in &digests {
                if key == &source {
                    crate::receipts::valid_hash(&hash.to_lowercase())?;
                    row.set("sha256", hash.to_lowercase());
                }
            }
            sources.push(row);
        }
        if digests.iter().any(|(name, _)| !seen.contains(name)) {
            return fail("source-digest has no matching source");
        }
        if !sources.is_empty() {
            record.set("sources", V::Arr(sources));
        }
        if args.has("split-rationale") {
            provenance(args.get("split-rationale"), 240)?;
            record.set(
                "split_rationale",
                args.get("split-rationale")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        let mut kinds = obj([]);
        let mut seen = BTreeSet::new();
        for (column, kind) in pairs(args.all("entity-kind"))? {
            if (!keys.contains(&column) && args.get("id-column") != column)
                || !seen.insert(column.clone())
                || !["protein", "ligand", "complex", "pocket", "assay", "other"]
                    .contains(&kind.as_str())
            {
                return fail("invalid entity-kind or unknown column");
            }
            kinds.set(&column, kind);
        }
        if args.has("entity-kind") {
            record.set("entity_kinds", kinds);
        }
        let mut derive = obj([]);
        if args.has("derive-cmd") {
            derive.set(
                "cmd",
                args.get("derive-cmd")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(500)
                    .collect::<String>(),
            );
        }
        if args.has("derive-script") {
            derive.set("script", args.get("derive-script"));
            derive.set(
                "script_sha256",
                storage::hash(&storage::file(&args.path("derive-script", context.cwd))?),
            );
        }
        if args.has("derive-cmd") || args.has("derive-script") {
            record.set("derive", derive);
        }
        let mut splits = Vec::new();
        let mut labels = BTreeSet::new();
        for (label, file) in files {
            if !labels.insert(label.clone()) {
                return fail("duplicate split label");
            }
            splits.push(fingerprint(context.cwd, &label, &file, &record)?.row);
        }
        record.set("splits", V::Arr(splits));
        storage::write(&repo, &path, json(&record)?, true, context.interrupted)?;
        return Ok((
            0,
            format!("{}\n", repo.join(&path).display()).into_bytes(),
            format!("data-manifest: wrote {}\n", path.display()).into_bytes(),
        ));
    }
    let manifest = V::parse(&storage::read(&repo, &path, false)?)?;
    let mut splits = Vec::new();
    let mut output = String::new();
    let mut bad = false;
    for old in manifest.get("splits").array() {
        match fingerprint(
            context.cwd,
            text(old, "label"),
            text(old, "file"),
            &manifest,
        ) {
            Ok(s) => splits.push((old, s)),
            Err(e) if args.action() == "check" => {
                bad = true;
                output.push_str(&format!("DRIFT {}: {}\n", text(old, "label"), e.message));
            }
            Err(e) => return Err(e),
        }
    }
    match args.action() {
        "check" => {
            for (old, split) in splits {
                let label = text(old, "label");
                if old.get("sha256") == split.row.get("sha256") {
                    output.push_str(&format!("ok    {label}: unchanged\n"));
                    continue;
                }
                let mut reasons = Vec::new();
                if old.get("id_sha256") != split.row.get("id_sha256") {
                    reasons.push("ID set".into());
                }
                for key in manifest
                    .get("leakage_keys")
                    .array()
                    .iter()
                    .filter_map(V::text)
                {
                    let left = old.get("keys").get(key);
                    let right = split.row.get("keys").get(key);
                    if left.get("sha256") != right.get("sha256")
                        || left.has("pair_sha256")
                            && right.has("pair_sha256")
                            && left.get("pair_sha256") != right.get("pair_sha256")
                    {
                        reasons.push(format!("key '{key}'"));
                    }
                }
                bad |= !reasons.is_empty();
                output.push_str(&format!(
                    "{} {label}: {}\n",
                    if reasons.is_empty() { "WARN " } else { "DRIFT" },
                    if reasons.is_empty() {
                        "file bytes changed but ID/key sets identical".into()
                    } else {
                        format!("{} changed", reasons.join(", "))
                    }
                ));
            }
            let derive = manifest.get("derive");
            if derive.has("script") {
                let unchanged = storage::file(&context.cwd.join(text(derive, "script")))
                    .ok()
                    .is_some_and(|raw| storage::hash(&raw) == text(derive, "script_sha256"));
                output.push_str(if unchanged{"ok    derive: key script unchanged\n"}else{"WARN  derive: key script changed or is gone; recompute keys and recreate the manifest\n"});
            }
            for source in manifest.get("sources").array() {
                output.push_str(&format!(
                    "info  source {}: {}\n",
                    text(source, "name"),
                    text(source, "ref")
                ));
            }
            if manifest.has("split_rationale") {
                output.push_str(&format!(
                    "info  split rationale: {}\n",
                    text(&manifest, "split_rationale")
                ));
            }
        }
        "leakage" => {
            for (_, split) in &splits {
                let unique = split.ids.iter().collect::<BTreeSet<_>>().len();
                if unique < split.ids.len() {
                    output.push_str(&format!(
                        "WARN {}: {} repeated ID(s) inside split\n",
                        text(&split.row, "label"),
                        split.ids.len() - unique
                    ));
                }
            }
            for i in 0..splits.len() {
                for j in i + 1..splits.len() {
                    let a = &splits[i].1;
                    let b = &splits[j].1;
                    let left = a.ids.iter().cloned().collect::<BTreeSet<_>>();
                    let right = b.ids.iter().cloned().collect::<BTreeSet<_>>();
                    let overlap = left.intersection(&right).cloned().collect::<Vec<_>>();
                    bad |= !overlap.is_empty();
                    output.push_str(&format!(
                        "{} {} ∩ {}: {}\n",
                        if overlap.is_empty() {
                            "ok     "
                        } else {
                            "LEAKAGE"
                        },
                        text(&a.row, "label"),
                        text(&b.row, "label"),
                        if overlap.is_empty() {
                            "no overlap".into()
                        } else {
                            format!("{} shared ID(s)", overlap.len())
                        }
                    ));
                    if args.has("show-examples") {
                        for value in overlap.iter().take(5) {
                            output.push_str(&format!("    {value}\n"));
                        }
                    }
                    for (key, left) in &a.keys {
                        let right = b
                            .keys
                            .get(key)
                            .ok_or_else(|| Error::new(2, "recorded leakage key absent"))?;
                        let shared = left.intersection(right).collect::<Vec<_>>();
                        bad |= !shared.is_empty();
                        output.push_str(&format!(
                            "{} [key {key}] {} ∩ {}: {} shared value(s)\n",
                            if shared.is_empty() {
                                "ok     "
                            } else {
                                "LEAKAGE"
                            },
                            text(&a.row, "label"),
                            text(&b.row, "label"),
                            shared.len()
                        ));
                        if args.has("show-examples") {
                            for value in shared.iter().take(5) {
                                output.push_str(&format!("    {value}\n"));
                            }
                        }
                    }
                }
            }
            if text(&manifest, "policy") == "time" {
                let kinds = splits
                    .iter()
                    .flat_map(|(_, s)| s.times.iter().map(|(_, aware)| *aware))
                    .collect::<BTreeSet<_>>();
                if kinds.len() > 1 {
                    return fail("time values must consistently use naive or aware timestamps");
                }
                for pair in splits.windows(2) {
                    let a = &pair[0].1;
                    let b = &pair[1].1;
                    if a.times.is_empty() || b.times.is_empty() {
                        return fail("time split has no rows");
                    }
                    let last = a
                        .times
                        .iter()
                        .map(|(n, _)| *n)
                        .fold(f64::NEG_INFINITY, f64::max);
                    let first = b
                        .times
                        .iter()
                        .map(|(n, _)| *n)
                        .fold(f64::INFINITY, f64::min);
                    if last >= first {
                        bad = true;
                        output.push_str(&format!(
                            "LEAKAGE [time] {} is not strictly before {}\n",
                            text(&a.row, "label"),
                            text(&b.row, "label")
                        ));
                    }
                }
            }
        }
        _ => return fail("unknown data-manifest subcommand"),
    }
    Ok((i32::from(bad), output.into_bytes(), Vec::new()))
}
