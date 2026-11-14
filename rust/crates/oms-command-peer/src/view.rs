use crate::{
    Context, Error, Output, Result, V, cli::Args, fail, json, now, obj, out, safety, storage, text,
};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
};

fn clean(value: &V, maximum: usize) -> Result<String> {
    let value = value
        .text()
        .ok_or_else(|| Error::new(2, "expected view text"))?;
    if value.chars().count() > maximum || value.contains('\0') {
        return fail("invalid or oversized view text");
    }
    if safety::redact(value.as_bytes())? != value.as_bytes() {
        return fail("view text refused by content scan");
    }
    Ok(value.into())
}
fn relative(value: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    storage::relative(&path)?;
    if path.components().any(|c| c.as_os_str() == ".git") {
        return fail("view paths must remain inside the selected repository");
    }
    Ok(path)
}
fn base64(raw: &[u8]) -> String {
    let mut s = crate::thread::encode(raw)
        .replace('-', "+")
        .replace('_', "/");
    while s.len() % 4 != 0 {
        s.push('=');
    }
    s
}
fn normalize(data: &V) -> Result<V> {
    if data.get("schema") != &1usize.into() || text(data, "kind") != "oms-study-results" {
        return fail("expected schema 1, kind oms-study-results");
    }
    let V::Arr(rows) = data.get("rows") else {
        return fail("rows must be an array");
    };
    if rows.len() > 10000 {
        return fail("view has more than 10000 rows");
    }
    let mut result = obj([
        ("schema", 1usize.into()),
        ("kind", "oms-study-results".into()),
    ]);
    let mut normalized = Vec::new();
    let mut seen = BTreeSet::new();
    let directions = data.default("metric_directions", obj([]));
    let V::Obj(fields) = &directions else {
        return fail("metric_directions must be an object");
    };
    for (key, value) in fields {
        if ![Some("min"), Some("max")].contains(&value.text()) || key.len() > 160 {
            return fail("invalid metric direction");
        }
    }
    for raw in rows {
        let mut row = obj([]);
        for key in ["run_id", "case_id", "variant"] {
            let value = clean(raw.get(key), 300)?;
            if value.is_empty() {
                return fail("run_id, case_id and variant are required");
            }
            row.set(key, value);
        }
        let repeat = raw.default("repeat", 0usize);
        if !repeat.integer(false) || crate::number(&repeat).is_none_or(|n| n > 9007199254740991.0) {
            return fail("repeat must be a nonnegative browser-safe integer");
        }
        row.set("repeat", repeat);
        row.set("status", clean(&raw.default("status", "complete"), 60)?);
        for field in ["metrics", "params"] {
            let values = raw.default(field, obj([]));
            let V::Obj(fields) = &values else {
                return fail("metrics/params must be objects");
            };
            if fields.len() > 128 {
                return fail("metrics/params exceed 128 entries");
            }
            for (name, value) in fields {
                if name.len() > 160 {
                    return fail("metric/parameter name too long");
                }
                if field == "metrics" && !matches!(value, V::Null) && crate::number(value).is_none()
                {
                    return fail("metrics must be finite numbers or null");
                }
                if matches!(value, V::Obj(_) | V::Arr(_))
                    || matches!(value, V::Num(_)) && crate::number(value).is_none()
                {
                    return fail("params must be finite scalars");
                }
                if let V::Str(_) = value {
                    clean(value, 1000)?;
                }
            }
            row.set(field, values);
        }
        let artifacts = raw.default("artifacts", V::Arr(Vec::new()));
        if !matches!(artifacts, V::Arr(_)) || artifacts.array().len() > 200 {
            return fail("artifacts must contain at most 200 records");
        }
        let mut normalized_artifacts = Vec::new();
        for item in artifacts.array() {
            let mut artifact = obj([]);
            for key in ["path", "type", "label"] {
                artifact.set(key, clean(&item.default(key, ""), 1000)?);
            }
            relative(text(&artifact, "path"))?;
            for key in ["sha256", "format", "frame", "alignment"] {
                if item.has(key) {
                    artifact.set(key, clean(item.get(key), 1000)?);
                }
            }
            if artifact.has("sha256") {
                crate::receipts::valid_hash(text(&artifact, "sha256"))?;
            }
            normalized_artifacts.push(artifact);
        }
        row.set("artifacts", V::Arr(normalized_artifacts));
        let identity = V::Arr(
            ["run_id", "case_id", "variant", "repeat"]
                .iter()
                .map(|k| row.get(k).clone())
                .collect(),
        );
        let key = format!("row-{}", &storage::hash(&identity.json(false)?)[..24]);
        if !seen.insert(key.clone()) {
            return fail("duplicate run/case/variant/repeat identity");
        }
        row.set("key", key);
        normalized.push(row);
    }
    result.set("rows", V::Arr(normalized));
    result.set("metric_directions", directions);
    for key in [
        "suite_id",
        "suite_sha256",
        "base_id",
        "base_sha256",
        "evaluator_sha256",
        "evaluation_pass_id",
        "source_sha256",
    ] {
        if data.has(key) {
            result.set(key, clean(data.get(key), 300)?);
        }
    }
    Ok(result)
}
fn embed(repo: &Path, data: &mut V) -> Result<()> {
    let V::Obj(fields) = data else {
        return fail("invalid view object");
    };
    let Some(V::Arr(rows)) = fields.get_mut(b"rows".as_slice()) else {
        return fail("invalid view rows");
    };
    let mut total = 0usize;
    for row in rows {
        let V::Obj(fields) = row else {
            continue;
        };
        let Some(V::Arr(artifacts)) = fields.get_mut(b"artifacts".as_slice()) else {
            continue;
        };
        for artifact in artifacts {
            let path = relative(text(artifact, "path"))?;
            let format = artifact
                .get("format")
                .text()
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    path.extension()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_lowercase()
                });
            if ![
                "png", "jpg", "jpeg", "gif", "webp", "txt", "json", "csv", "tsv", "md", "pdb",
                "sdf", "mol2", "xyz",
            ]
            .contains(&format.as_str())
            {
                artifact.set(
                    "availability",
                    "reference only: unsupported embedding format",
                );
                continue;
            }
            let raw = storage::read(repo, &path, false)?;
            total += raw.len();
            if raw.len() > 2 * 1024 * 1024 || total > 12 * 1024 * 1024 {
                return fail("embedded artifacts exceed per-file or total size limit");
            }
            let digest = storage::hash(&raw);
            if artifact.has("sha256") && text(artifact, "sha256") != digest {
                return fail("artifact bytes changed");
            }
            artifact.set("sha256", digest);
            artifact.set("format", format.clone());
            if ["png", "jpg", "jpeg", "gif", "webp"].contains(&format.as_str()) {
                let valid = match format.as_str() {
                    "png" => raw.starts_with(b"\x89PNG\r\n\x1a\n"),
                    "jpg" | "jpeg" => raw.starts_with(b"\xff\xd8\xff"),
                    "gif" => raw.starts_with(b"GIF87a") || raw.starts_with(b"GIF89a"),
                    "webp" => raw.starts_with(b"RIFF") && raw.get(8..12) == Some(b"WEBP"),
                    _ => false,
                };
                if !valid {
                    return fail("image signature does not match its extension");
                }
                artifact.set(
                    "data_url",
                    format!(
                        "data:image/{};base64,{}",
                        if format == "jpg" { "jpeg" } else { &format },
                        base64(&raw)
                    ),
                );
            } else {
                let value = std::str::from_utf8(&raw)
                    .map_err(|_| Error::new(2, "artifact must be UTF-8"))?;
                artifact.set("text", clean(&value.into(), 2 * 1024 * 1024)?);
            }
        }
    }
    Ok(())
}
fn html(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}
pub(crate) fn run(argv: &[OsString], context: &Context<'_>) -> Result<Output> {
    let args = Args::parse(
        argv,
        &[
            "repo",
            "output",
            "title",
            "molecule-js",
            "input",
            "host",
            "remote-root",
            "view",
        ],
        &["replace"],
    )?;
    if args.has("help") {
        return Ok(out(b"Usage: oms artifact-view [--repo PATH] build --input FILE | capsules ID... | remote --host HOST --remote-root PATH --input FILE | import-events FILE | events [--view ID] | list\nBuild: --output relative.html --title TEXT --molecule-js FILE --replace\n".to_vec()));
    }
    let repo = storage::repository(context, &args.path("repo", context.cwd))?;
    let directory = Path::new(".oms/artifact-views");
    if args.action() == "list" {
        let mut values = Vec::new();
        for name in storage::names(&repo, directory, ".json")?
            .into_iter()
            .filter(|s| s.starts_with("view-"))
        {
            values.push(V::parse(&storage::read(
                &repo,
                &directory.join(name),
                false,
            )?)?);
        }
        return Ok(out(json(&obj([("views", V::Arr(values))]))?));
    }
    if args.action() == "events" {
        let rows = storage::load(&repo, &directory.join("feedback.jsonl"))?
            .into_iter()
            .filter(|r| !args.has("view") || text(r, "view_id") == args.get("view"))
            .collect();
        return Ok(out(json(&obj([("feedback", V::Arr(rows))]))?));
    }
    safety::parent()?;
    if args.action() == "import-events" {
        let input = V::parse(&storage::file(&context.cwd.join(args.operand(1)))?)?;
        if input.get("schema") != &1usize.into() || text(&input, "kind") != "oms-artifact-feedback"
        {
            return fail("expected oms-artifact-feedback schema 1");
        }
        let ident = text(&input, "view_id");
        if !storage::identifier(ident) {
            return fail("invalid view id");
        }
        let meta = V::parse(&storage::read(
            &repo,
            &directory.join(format!("{ident}.json")),
            false,
        )?)?;
        if meta.get("data_sha256") != input.get("data_sha256") {
            return fail("feedback does not identify a registered source view");
        }
        if !matches!(input.get("events"), V::Arr(_)) || input.get("events").array().len() > 1000 {
            return fail("feedback must contain at most 1000 events");
        }
        let mut events = Vec::new();
        for raw in input.get("events").array() {
            if !meta
                .get("row_keys")
                .array()
                .iter()
                .any(|v| v == raw.get("row_key"))
                || !["select", "accept", "reject", "uncertain", "comment"]
                    .contains(&text(raw, "action"))
            {
                return fail("feedback names unknown row or action");
            }
            events.push(obj([
                ("row_key", raw.get("row_key").clone()),
                ("action", raw.get("action").clone()),
                ("comment", clean(&raw.default("comment", ""), 4000)?.into()),
            ]));
        }
        let mut report = obj([]);
        if let V::Obj(fields) = input.default("report", obj([])) {
            for (key, value) in fields {
                let key =
                    String::from_utf8(key).map_err(|_| Error::new(2, "invalid report field"))?;
                if !["title", "question", "decision", "next"].contains(&key.as_str()) {
                    return fail("invalid report field");
                }
                report.set(&key, clean(&value, 12000)?);
            }
        } else {
            return fail("report must be an object");
        }
        let mut batch = obj([
            ("view_id", ident.into()),
            ("data_sha256", input.get("data_sha256").clone()),
            ("events", V::Arr(events)),
            ("report", report),
        ]);
        let batch_id = storage::hash(&batch.json(false)?);
        let count = batch.get("events").array().len();
        batch.set("schema", 1usize);
        batch.set("batch_id", batch_id.clone());
        batch.set("created_at", now());
        let imported = storage::log(
            &repo,
            &directory.join("feedback.jsonl"),
            context.interrupted,
            |old, _| {
                if storage::rows(old)?
                    .iter()
                    .any(|r| text(r, "batch_id") == batch_id)
                {
                    return Ok((Vec::new(), false));
                }
                let mut bytes = batch.json(false)?;
                bytes.push(b'\n');
                Ok((bytes, true))
            },
        )?;
        return Ok(out(json(&obj([
            ("imported", imported.into()),
            ("duplicate", (!imported).into()),
            ("events", count.into()),
            ("batch_id", batch_id.into()),
        ]))?));
    }
    if args.action() == "remote" {
        return fail(
            "remote view requires a native bounded no-follow remote reader; no SSH call occurred",
        );
    }
    let mut data = match args.action() {
        "build" => normalize(&V::parse(&storage::file(
            &args.path("input", context.cwd),
        )?)?)?,
        "capsules" => {
            let mut rows = Vec::new();
            for id in args.positional.iter().skip(1) {
                let ident = id
                    .to_str()
                    .ok_or_else(|| Error::new(2, "invalid capsule id"))?;
                if !storage::identifier(ident) {
                    return fail("invalid capsule id");
                }
                let capsule = V::parse(&storage::read(
                    &repo,
                    &Path::new(".oms/runs").join(ident).join("capsule.json"),
                    false,
                )?)?;
                if text(&capsule, "id") != ident {
                    return fail("capsule identity differs");
                }
                let mut params = obj([]);
                for config in capsule.get("configs").array() {
                    if let V::Obj(fields) = config.get("params") {
                        for (key, value) in fields {
                            params.set(
                                &format!(
                                    "{}:{}",
                                    config.get("path").text().unwrap_or("config"),
                                    String::from_utf8_lossy(key)
                                ),
                                value.clone(),
                            );
                        }
                    }
                }
                rows.push(obj([
                    ("run_id", ident.into()),
                    ("case_id", "aggregate".into()),
                    ("variant", ident.into()),
                    ("repeat", 0usize.into()),
                    (
                        "status",
                        if capsule.get("result").get("exit") == &0usize.into() {
                            "complete"
                        } else {
                            "failed"
                        }
                        .into(),
                    ),
                    ("metrics", capsule.get("result").default("metrics", obj([]))),
                    ("params", params),
                    ("artifacts", V::Arr(Vec::new())),
                ]));
            }
            normalize(&obj([
                ("schema", 1usize.into()),
                ("kind", "oms-study-results".into()),
                ("rows", V::Arr(rows)),
            ]))?
        }
        _ => return fail("unknown artifact-view action"),
    };
    embed(&repo, &mut data)?;
    let body = data.json(false)?;
    let digest = storage::hash(&body);
    let ident = format!("view-{}", &digest[..24]);
    let title = args.value("title", "Experiment comparison");
    clean(&title.into(), 200)?;
    let payload = obj([
        ("view_id", ident.clone().into()),
        ("data_sha256", digest.clone().into()),
        ("title", title.into()),
        ("data", data.clone()),
    ]);
    let assets = Path::new("scripts/lib/artifact_view_assets");
    let read_asset = |name: &str| -> Result<String> {
        String::from_utf8(storage::read(context.source, &assets.join(name), false)?)
            .map_err(|_| Error::new(2, "invalid installed view asset"))
    };
    let js = format!("{}\n{}", read_asset("analysis.js")?, read_asset("view.js")?);
    let css = read_asset("view.css")?;
    let template = read_asset("view.html")?;
    let vendor = if args.has("molecule-js") {
        storage::file(&args.path("molecule-js", context.cwd))?
    } else {
        Vec::new()
    };
    if vendor.len() > 4 * 1024 * 1024 {
        return fail("molecule asset exceeds 4 MiB");
    }
    let page = template
        .replace("__TITLE__", &html(title))
        .replace("__CSS__", &css)
        .replace("__PAYLOAD__", &base64(&payload.json(false)?))
        .replace("__MOLECULE_JS__", &base64(&vendor))
        .replace("__JS__", &js);
    if page.len() > 24 * 1024 * 1024 {
        return fail("rendered view exceeds 24 MiB");
    }
    let output = if args.has("output") {
        relative(args.get("output"))?
    } else {
        directory.join(format!("{ident}.html"))
    };
    if output.extension().is_none_or(|s| s != "html") {
        return fail("view output must end in .html");
    }
    let meta = obj([
        ("schema", 1usize.into()),
        ("id", ident.clone().into()),
        ("data_sha256", digest.into()),
        ("title", title.into()),
        (
            "row_keys",
            V::Arr(
                data.get("rows")
                    .array()
                    .iter()
                    .map(|r| r.get("key").clone())
                    .collect(),
            ),
        ),
        ("created_at", now().into()),
        (
            "molecule_js_sha256",
            if vendor.is_empty() {
                V::Null
            } else {
                storage::hash(&vendor).into()
            },
        ),
        ("html_sha256", storage::hash(page.as_bytes()).into()),
        ("path", output.to_string_lossy().into_owned().into()),
    ]);
    storage::log(
        &repo,
        &directory.join("registry"),
        context.interrupted,
        |_, _| {
            let old = storage::read(&repo, &directory.join(format!("{ident}.json")), true)?;
            if !old.is_empty() && V::parse(&old)?.get("data_sha256") != meta.get("data_sha256") {
                return fail("view identity collision");
            }
            storage::write(
                &repo,
                &output,
                page.into_bytes(),
                args.has("replace"),
                context.interrupted,
            )?;
            storage::write(
                &repo,
                &directory.join(format!("{ident}.json")),
                json(&meta)?,
                true,
                context.interrupted,
            )?;
            Ok((Vec::new(), ()))
        },
    )?;
    Ok(out(json(&meta)?))
}
