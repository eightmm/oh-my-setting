use crate::{
    Context, Error, Output, Result, V, cli::Args, fail, json, now, obj, out, safety, storage, text,
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

pub(crate) fn run(tool: &str, argv: &[OsString], context: &Context<'_>) -> Result<Output> {
    let args = Args::parse(
        argv,
        &[
            "kind", "name", "version", "sha256", "file", "license", "env", "egress", "approver",
            "note", "id", "title", "year", "claim", "locator",
        ],
        &["json", "online", "bibtex"],
    )?;
    if args.has("help") {
        return Ok(out(if tool == "cite" {
            include_bytes!("help/cite.txt").as_slice()
        } else {
            include_bytes!("help/external.txt").as_slice()
        }
        .to_vec()));
    }
    args.exact_operands(if tool == "external" { 2 } else { 1 })?;
    let repo = storage::repository(context, context.cwd)?;
    let variable = if tool == "external" {
        "OMS_EXTERNAL_FILE"
    } else {
        "OMS_LITERATURE_FILE"
    };
    let default = format!(
        ".oms/{}.jsonl",
        if tool == "external" {
            "external"
        } else {
            "literature"
        }
    );
    let selected = std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default));
    let (root, path) = if selected.is_absolute() {
        (
            selected
                .parent()
                .ok_or_else(|| Error::new(2, "store needs a parent"))?
                .to_owned(),
            PathBuf::from(
                selected
                    .file_name()
                    .ok_or_else(|| Error::new(2, "store needs a file name"))?,
            ),
        )
    } else {
        (repo.clone(), selected)
    };
    storage::relative(&path)?;
    if tool == "external" {
        external(&args, context, &repo, &root, &path)
    } else {
        cite(&args, context, &root, &path)
    }
}
fn external(
    args: &Args,
    context: &Context<'_>,
    repo: &Path,
    root: &Path,
    path: &Path,
) -> Result<Output> {
    let values = storage::latest(&storage::load(root, path)?)?;
    match args.action() {
        "add" => {
            safety::parent()?;
            let kind = args.required("kind")?;
            let name = args.required("name")?;
            if !["skill", "mcp", "model", "dataset", "tool"].contains(&kind) {
                return fail("invalid external kind");
            }
            if !regex::Regex::new(r"\A[A-Za-z0-9][A-Za-z0-9._/@:+-]{0,127}\z")
                .unwrap()
                .is_match(name)
            {
                return fail("invalid receipt name");
            }
            let egress = args.value("egress", "none");
            if !["none", "public-query", "uploads-input"].contains(&egress) {
                return fail("invalid egress kind");
            }
            if egress != "none" && args.get("approver").is_empty() {
                return fail(
                    "egress needs --approver (who accepted that data may leave); not recorded as verified",
                );
            }
            let ident = format!(
                "ext-{}",
                &storage::hash(format!("{kind}\0{name}\0{}", args.get("version")).as_bytes())[..10]
            );
            let mut row = obj([
                ("id", ident.clone().into()),
                ("kind", kind.into()),
                ("name", name.into()),
                ("version", args.get("version").into()),
                ("license", args.value("license", "unknown").into()),
                ("egress", egress.into()),
                ("approver", args.get("approver").into()),
                ("added_at", now().into()),
            ]);
            if args.has("sha256") {
                valid_hash(args.get("sha256"))?;
                row.set("sha256", args.get("sha256"));
            }
            for (option, field, digest_field) in [
                ("file", "file", "sha256"),
                ("env", "env_lock", "env_lock_sha256"),
            ] {
                if args.has(option) {
                    let location = args.path(option, context.cwd);
                    let bytes = storage::file(&location)?;
                    let hash = storage::hash(&bytes);
                    if option == "file" && args.has("sha256") && args.get("sha256") != hash {
                        return fail("--sha256 does not match file");
                    }
                    let canonical = location.canonicalize()?;
                    row.set(
                        field,
                        canonical
                            .strip_prefix(repo)
                            .unwrap_or(&canonical)
                            .to_string_lossy()
                            .into_owned(),
                    );
                    row.set(digest_field, hash);
                }
            }
            if args.has("note") {
                row.set("note", args.get("note"));
            }
            storage::append(root, path, &row, context.interrupted)?;
            let mut output = format!(
                "{ident}: {kind} {name}{} recorded\nlicense: {} (recorded as declared; OMS does not judge licenses)\n{}\n",
                if args.get("version").is_empty() {
                    String::new()
                } else {
                    format!("@{}", args.get("version"))
                },
                text(&row, "license"),
                describe(&row)
            );
            if !args.has("version") && !row.has("sha256") {
                output.push_str(
                    "note: no version, commit or sha256; this receipt cannot pin what was used\n",
                );
            }
            if !row.has("env_lock") {
                output.push_str("note: no environment lock recorded (--env PATH)\n");
            }
            Ok(out(output.into_bytes()))
        }
        "list" if args.has("json") => Ok(out(json(&V::Arr(values))?)),
        "list" => {
            let mut output = String::new();
            for row in &values {
                output.push_str(&format!(
                    "{}  {:7} {}{} | license {} | egress {}\n",
                    text(row, "id"),
                    text(row, "kind"),
                    text(row, "name"),
                    if text(row, "version").is_empty() {
                        String::new()
                    } else {
                        format!("@{}", text(row, "version"))
                    },
                    text(row, "license"),
                    text(row, "egress")
                ));
            }
            if values.is_empty() {
                output = format!("no external receipts ({})\n", root.join(path).display());
            }
            Ok(out(output.into_bytes()))
        }
        "show" => {
            let row = find(&values, args.operand(1))?;
            let mut bytes = json(row)?;
            bytes.extend_from_slice(format!("{}\n", describe(row)).as_bytes());
            Ok(out(bytes))
        }
        "check" => {
            let selected: Vec<_> = if args.operand(1).is_empty() {
                values.iter().collect()
            } else {
                vec![find(&values, args.operand(1))?]
            };
            let mut bad = false;
            let mut output = String::new();
            for row in selected {
                let mut drift = Vec::new();
                for (field, hash_field) in [("file", "sha256"), ("env_lock", "env_lock_sha256")] {
                    if row.has(field) && row.has(hash_field) {
                        match storage::file(&repo.join(text(row, field))) {
                            Ok(bytes) if storage::hash(&bytes) == text(row, hash_field) => {}
                            Ok(_) => drift.push(format!("{field} changed: {}", text(row, field))),
                            Err(_) => drift.push(format!("{field} missing: {}", text(row, field))),
                        }
                    }
                }
                let changed = !drift.is_empty();
                bad |= changed;
                if text(row, "egress") != "none" {
                    drift.push(format!("note: {}", describe(row)));
                }
                output.push_str(&format!(
                    "{} {}: {}\n",
                    if changed { "drift" } else { "ok   " },
                    text(row, "id"),
                    if drift.is_empty() {
                        "hashes match".into()
                    } else {
                        drift.join("; ")
                    }
                ));
            }
            Ok((i32::from(bad), output.into_bytes(), Vec::new()))
        }
        _ => fail("external expects add, list, show or check"),
    }
}
fn describe(row: &V) -> String {
    let detail = match text(row, "egress") {
        "none" => "declared as not sending data out (a declaration, not observed)",
        "public-query" => {
            "queries go to an external provider; unpublished structures or sequences in a query leave this machine"
        }
        "uploads-input" => {
            "input files are uploaded to an external provider; unpublished data leaves this machine"
        }
        _ => "unknown value",
    };
    format!("egress {}: {detail}", text(row, "egress"))
}
fn find<'a>(values: &'a [V], ident: &str) -> Result<&'a V> {
    values
        .iter()
        .find(|r| text(r, "id") == ident)
        .ok_or_else(|| Error::new(2, format!("no such id: {ident}")))
}
pub(crate) fn valid_hash(value: &str) -> Result<()> {
    if value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        fail("sha256 must be 64 lowercase hex characters")
    }
}
pub(crate) fn normalize_id(raw: &str) -> std::result::Result<(String, String), &'static str> {
    let mut value = raw.trim();
    let mut low = value.to_lowercase();
    for prefix in ["https://doi.org/", "http://doi.org/", "doi:"] {
        if low.starts_with(prefix) {
            value = &value[prefix.len()..];
            break;
        }
    }
    low = value.to_lowercase();
    if low.starts_with("10.") {
        if regex::Regex::new(r"\A10\.\d{4,9}/[^\s]+\z")
            .unwrap()
            .is_match(value)
        {
            return Ok(("doi".into(), low));
        }
        return Err("not a DOI (expected 10.<registrant>/<suffix>)");
    }
    for prefix in ["arxiv:", "https://arxiv.org/abs/"] {
        if low.starts_with(prefix) {
            value = &value[prefix.len()..];
            break;
        }
    }
    if regex::Regex::new(r"\A(\d{4}\.\d{4,5}|[a-z\-]+(\.[A-Z]{2})?/\d{7})(v\d+)?\z")
        .unwrap()
        .is_match(value)
    {
        Ok(("arxiv".into(), value.into()))
    } else {
        Err("not an arXiv id (expected 2101.00001 or hep-th/9901001)")
    }
}
fn cite(args: &Args, context: &Context<'_>, root: &Path, path: &Path) -> Result<Output> {
    let values = storage::latest(&storage::load(root, path)?)?;
    match args.action() {
        "add" => {
            safety::parent()?;
            let raw = args.required("id")?;
            let normalized = normalize_id(raw);
            let ident = normalized
                .as_ref()
                .map(|(k, v)| format!("{k}:{v}"))
                .unwrap_or_else(|_| raw.trim().into());
            let mut row = obj([
                ("id", ident.clone().into()),
                ("title", args.get("title").into()),
                ("claim", args.get("claim").into()),
                ("locator", args.get("locator").into()),
                (
                    "status",
                    if normalized.is_ok() {
                        "unverified"
                    } else {
                        "invalid-id"
                    }
                    .into(),
                ),
                ("added_at", now().into()),
                ("checked_at", V::Null),
            ]);
            if args.has("year") {
                let year = args
                    .get("year")
                    .parse::<i64>()
                    .map_err(|_| Error::new(2, "--year must be an integer"))?;
                row.set("year", year);
            }
            if let Err(reason) = normalized {
                row.set("detail", reason);
            }
            if let Ok(old) = find(&values, &ident) {
                for key in ["title", "claim", "locator", "year"] {
                    if !row.get(key).truth() && old.get(key).truth() {
                        row.set(key, old.get(key).clone());
                    }
                }
            }
            storage::append(root, path, &row, context.interrupted)?;
            Ok((
                i32::from(normalized.is_err()),
                format!(
                    "{ident}: {} ({})\n",
                    text(&row, "status"),
                    row.get("detail").text().unwrap_or("recorded")
                )
                .into_bytes(),
                Vec::new(),
            ))
        }
        "list" if args.has("json") => Ok(out(json(&V::Arr(values))?)),
        "list" => Ok(out(values
            .iter()
            .map(|r| {
                format!(
                    "{:12} {} | {} | {}\n",
                    text(r, "status"),
                    text(r, "id"),
                    r.get("year")
                        .text()
                        .map(String::from)
                        .unwrap_or_else(|| if r.has("year") {
                            r.get("year").py()
                        } else {
                            "-".into()
                        }),
                    crate::safety::truncate(text(r, "title"), 70)
                )
            })
            .collect::<String>()
            .into_bytes())),
        "verify" => {
            if args.has("online") {
                safety::parent()?;
            }
            let wanted = if args.has("id") {
                normalize_id(args.get("id"))
                    .map(|(k, v)| format!("{k}:{v}"))
                    .unwrap_or_else(|_| args.get("id").into())
            } else {
                String::new()
            };
            if !wanted.is_empty() {
                find(&values, &wanted)?;
            }
            let mut bad = false;
            let mut output = String::new();
            for original in values
                .iter()
                .filter(|r| wanted.is_empty() || text(r, "id") == wanted)
            {
                let mut row = original.clone();
                if let Err(reason) = normalize_id(text(&row, "id")) {
                    row.set("status", "invalid-id");
                    row.set("detail", reason);
                } else if args.has("online") {
                    row.set("checked_at", now());
                    let (kind, canonical) = normalize_id(text(&row, "id")).unwrap();
                    match lookup(&kind, &canonical, context) {
                        Ok(Some((title, year))) => {
                            let mut problems = Vec::new();
                            if !text(&row, "title").is_empty()
                                && !titles_match(text(&row, "title"), &title)
                            {
                                problems.push(format!(
                                    "title differs (registry: {})",
                                    title.chars().take(100).collect::<String>()
                                ));
                            }
                            if row.has("year")
                                && year.is_some_and(|n| {
                                    crate::number(row.get("year")) != Some(n as f64)
                                })
                            {
                                problems
                                    .push(format!("year differs (registry: {})", year.unwrap()));
                            }
                            if !problems.is_empty() {
                                row.set("status", "mismatch");
                                row.set("detail", problems.join("; "));
                            } else if text(&row, "title").is_empty() {
                                row.set("status", "unverified");
                                row.set("detail", "id exists but no title recorded to compare");
                            } else {
                                row.set("status", "verified");
                                row.set("detail", "id exists; title and year agree");
                            }
                        }
                        Ok(None) => {
                            row.set("status", "not-found");
                            row.set("detail", "registry has no record for this id");
                        }
                        Err(_) => {
                            row.set("status", "unverified");
                            row.set(
                                "detail",
                                "lookup failed: bounded registry request unavailable or malformed",
                            );
                        }
                    }
                } else {
                    row.set("detail", "syntax ok; not looked up (use --online)");
                }
                if args.has("online") {
                    safety::parent()?;
                    storage::append(root, path, &row, context.interrupted)?;
                }
                bad |= text(&row, "status") != "verified";
                output.push_str(&format!(
                    "{:10} {}: {}\n",
                    text(&row, "status"),
                    text(&row, "id"),
                    text(&row, "detail")
                ));
            }
            if !args.has("online") {
                output.push_str("offline: only identifier syntax was checked; nothing is verified without --online\n");
            }
            Ok((i32::from(bad), output.into_bytes(), Vec::new()))
        }
        "export" => {
            if !args.has("bibtex") {
                return fail("export requires --bibtex");
            }
            let mut output = String::new();
            for row in &values {
                let key = regex::Regex::new("[^A-Za-z0-9]+")
                    .unwrap()
                    .replace_all(text(row, "id"), "_")
                    .trim_matches('_')
                    .to_string();
                output.push_str(&format!("@misc{{{key},\n"));
                let mut fields = vec![
                    ("title", text(row, "title").to_owned()),
                    (
                        "year",
                        if row.has("year") {
                            row.get("year").py()
                        } else {
                            String::new()
                        },
                    ),
                ];
                if let Ok((kind, canon)) = normalize_id(text(row, "id")) {
                    if kind == "doi" {
                        fields.push(("doi", canon));
                    } else {
                        fields.push(("eprint", canon));
                        fields.push(("archiveprefix", "arXiv".into()));
                    }
                }
                fields.push(("note", format!("OMS status: {}", text(row, "status"))));
                for (field, value) in fields {
                    if !value.is_empty() {
                        let escaped = value
                            .chars()
                            .map(|c| {
                                if "{}%&$#_\\".contains(c) {
                                    format!("\\{c}")
                                } else {
                                    c.to_string()
                                }
                            })
                            .collect::<String>();
                        output.push_str(&format!("  {field} = {{{escaped}}},\n"));
                    }
                }
                output.push_str("}\n\n");
            }
            Ok(out(output.into_bytes()))
        }
        _ => fail("cite expects add, list, verify or export"),
    }
}

fn percent(raw: &str, slash: bool) -> String {
    raw.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) || slash && b == b'/' {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn lookup(kind: &str, ident: &str, context: &Context<'_>) -> Result<Option<(String, Option<i64>)>> {
    let url = if kind == "doi" {
        let base = std::env::var("OMS_CITE_CROSSREF_URL")
            .unwrap_or_else(|_| "https://api.crossref.org/works/".into());
        format!("{base}{}", percent(ident, true))
    } else {
        let base = std::env::var("OMS_CITE_ARXIV_URL")
            .unwrap_or_else(|_| "https://export.arxiv.org/api/query".into());
        format!("{base}?id_list={}", percent(ident, false))
    };
    if !url.starts_with("https://") && !url.starts_with("http://")
        || url.chars().any(char::is_control)
    {
        return fail("invalid citation registry endpoint");
    }
    let args = [
        "curl",
        "--silent",
        "--show-error",
        "--location",
        "--max-time",
        "15",
        "--max-filesize",
        "2000000",
        "--proto",
        "=http,https",
        "--user-agent",
        "oms-cite/1",
        "--write-out",
        "\n%{http_code}",
        &url,
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    let response = crate::process::invoke(context, context.cwd, args, None, 16, false)?;
    if response.code != 0 || !response.complete || response.stdout.len() > 2_000_004 {
        return fail("citation request did not complete");
    }
    let raw = std::str::from_utf8(&response.stdout)
        .map_err(|_| Error::new(2, "invalid registry text"))?;
    let (body, status) = raw
        .rsplit_once('\n')
        .ok_or_else(|| Error::new(2, "registry status absent"))?;
    if status == "404" {
        return Ok(None);
    }
    if status != "200" {
        return fail("registry request failed");
    }
    if kind == "doi" {
        let value = V::parse(body.as_bytes())?;
        let message = value.get("message");
        let title = message
            .get("title")
            .array()
            .first()
            .and_then(V::text)
            .unwrap_or("")
            .to_owned();
        let year = message
            .get("issued")
            .get("date-parts")
            .array()
            .first()
            .and_then(|v| v.array().first())
            .and_then(crate::number)
            .map(|n| n as i64);
        return Ok(Some((title, year)));
    }
    if body.contains("<!DOCTYPE") || body.contains("<!ENTITY") {
        return fail("unsupported registry XML declaration");
    }
    let entry = xml(body, "entry");
    let Some(entry) = entry else {
        return Ok(None);
    };
    let title = xml(&entry, "title").unwrap_or_default();
    if title.trim().is_empty() || title.trim().eq_ignore_ascii_case("error") {
        return Ok(None);
    }
    let published = xml(&entry, "published").unwrap_or_default();
    let year = published.get(..4).and_then(|s| s.parse().ok());
    Ok(Some((
        unescape_xml(&title)?
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        year,
    )))
}
fn xml(raw: &str, name: &str) -> Option<String> {
    let expression = format!(
        r"(?s)<(?:[A-Za-z_][A-Za-z0-9_.-]*:)?{name}(?:\s[^>]*)?>(.*?)</(?:[A-Za-z_][A-Za-z0-9_.-]*:)?{name}\s*>"
    );
    regex::Regex::new(&expression)
        .ok()?
        .captures(raw)
        .map(|c| c[1].to_owned())
}
fn unescape_xml(raw: &str) -> Result<String> {
    let mut result = String::new();
    let mut remaining = raw;
    while let Some(at) = remaining.find('&') {
        result.push_str(&remaining[..at]);
        let rest = &remaining[at + 1..];
        let end = rest
            .find(';')
            .ok_or_else(|| Error::new(2, "invalid XML entity"))?;
        let entity = &rest[..end];
        let ch = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            s if s.starts_with("#x") => char::from_u32(
                u32::from_str_radix(&s[2..], 16)
                    .map_err(|_| Error::new(2, "invalid XML entity"))?,
            )
            .ok_or_else(|| Error::new(2, "invalid XML scalar"))?,
            s if s.starts_with('#') => char::from_u32(
                s[1..]
                    .parse::<u32>()
                    .map_err(|_| Error::new(2, "invalid XML entity"))?,
            )
            .ok_or_else(|| Error::new(2, "invalid XML scalar"))?,
            _ => return fail("unsupported XML entity"),
        };
        result.push(ch);
        remaining = &rest[end + 1..];
    }
    result.push_str(remaining);
    Ok(result)
}
fn titles_match(left: &str, right: &str) -> bool {
    let normalize = |raw: &str| {
        regex::Regex::new("[^a-z0-9]+")
            .unwrap()
            .replace_all(&raw.to_lowercase(), " ")
            .trim()
            .chars()
            .collect::<Vec<_>>()
    };
    let a = normalize(left);
    let b = normalize(right);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let mut positions: std::collections::BTreeMap<char, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, c) in b.iter().enumerate() {
        positions.entry(*c).or_default().push(i);
    }
    if b.len() >= 200 {
        positions.retain(|_, p| p.len() <= b.len() / 100 + 1);
    }
    let mut ranges = vec![(0, a.len(), 0, b.len())];
    let mut matched = 0usize;
    while let Some((alo, ahi, blo, bhi)) = ranges.pop() {
        let (mut ai, mut bj, mut size) = (alo, blo, 0);
        let mut lengths = std::collections::BTreeMap::<usize, usize>::new();
        for (i, c) in a.iter().enumerate().take(ahi).skip(alo) {
            let mut next = std::collections::BTreeMap::new();
            if let Some(indices) = positions.get(c) {
                for j in indices.iter().copied().filter(|j| *j >= blo && *j < bhi) {
                    let n = lengths.get(&j.wrapping_sub(1)).copied().unwrap_or(0) + 1;
                    next.insert(j, n);
                    if n > size {
                        ai = i + 1 - n;
                        bj = j + 1 - n;
                        size = n;
                    }
                }
            }
            lengths = next;
        }
        while ai > alo && bj > blo && a[ai - 1] == b[bj - 1] {
            ai -= 1;
            bj -= 1;
            size += 1;
        }
        while ai + size < ahi && bj + size < bhi && a[ai + size] == b[bj + size] {
            size += 1;
        }
        if size > 0 {
            matched += size;
            if alo < ai && blo < bj {
                ranges.push((alo, ai, blo, bj));
            }
            if ai + size < ahi && bj + size < bhi {
                ranges.push((ai + size, ahi, bj + size, bhi));
            }
        }
    }
    2.0 * matched as f64 / (a.len() + b.len()) as f64 >= 0.92
}
