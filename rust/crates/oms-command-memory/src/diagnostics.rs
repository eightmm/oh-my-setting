pub(crate) use crate::sources::run as sources;
use crate::{db, store::Store, *};
use serde_json::json;
use std::collections::{HashMap, HashSet};
fn push(v: &mut Value, s: &str) {
    v.as_array_mut().unwrap().push(s.into());
}
pub(crate) fn health(s: &Store, as_json: bool) -> Result<Output> {
    let images = db::snapshots(s)?;
    let mut report = json!({"schema":1,"action":"health","assessment":"index-health-not-memory-quality","status":"empty","sources":{},"failures":{},"index":{"present":s.database.exists(),"readable":false,"current":false,"schema_version":null,"expected_schema":3,"integrity":"not-run","fts":false,"fts_entries":null,"fts_current":null,"entries":0,"by_source":{"shared":0,"pins":0,"failures":0},"provenance":{"total":0,"modern":0,"legacy":0,"task_id":0,"session_hash":0,"git_sha":0,"git_dirty":0,"git_state":0},"problems":[]},"problems":[]});
    let mut total = 0;
    let (mut valid, mut invalid) = (0, 0);
    let mut states = HashMap::new();
    for (source, image, entries) in &images {
        report["sources"][*source] =
            json!({"present":image.size>=0,"bytes":image.size.max(0),"entries":entries.len()});
        total += entries.len();
        if *source == "failures" {
            for l in String::from_utf8_lossy(&image.bytes)
                .lines()
                .filter(|l| !l.trim().is_empty())
            {
                let Some(row) = parse_json(l.as_bytes()).filter(Value::is_object) else {
                    invalid += 1;
                    continue;
                };
                valid += 1;
                let fp = text(&row, "fingerprint");
                let event = text(&row, "event");
                if !fp.is_empty() && ["fail", "resolved"].contains(&event.as_str()) {
                    states.insert(fp, event == "resolved");
                }
            }
        }
    }
    report["failures"] = json!({"valid_rows":valid,"invalid_rows":invalid,"open_fingerprints":states.values().filter(|v|!**v).count(),"resolved_fingerprints":states.values().filter(|v|**v).count()});
    if invalid > 0 {
        push(&mut report["problems"], "failure-ledger-invalid");
    }
    let mut code = 0;
    if !s.database.exists() {
        if invalid > 0 {
            report["status"] = "degraded".into();
            code = 1;
        } else if total > 0 || valid > 0 {
            report["status"] = "missing".into();
            push(&mut report["index"]["problems"], "index-missing");
            code = 1;
        }
    } else {
        let result = (|| -> Result<()> {
            let db = db::connect(&s.database, true)?;
            let ix = &mut report["index"];
            ix["readable"] = true.into();
            let integrity: String = db.query_row("pragma integrity_check", [], |r| r.get(0))?;
            ix["integrity"] = integrity.clone().into();
            let version: i64 = db.query_row("pragma user_version", [], |r| r.get(0))?;
            ix["schema_version"] = version.into();
            let mut q =
                db.prepare("select name from sqlite_master where type in ('table','view')")?;
            let tables = q
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<HashSet<_>>>()?;
            if !["memory_sources", "memory_entries"]
                .iter()
                .all(|t| tables.contains(*t))
            {
                push(&mut ix["problems"], "required-tables-missing");
            }
            let mut columns = HashSet::new();
            if tables.contains("memory_entries") {
                let mut q = db.prepare("pragma table_info(memory_entries)")?;
                columns = q
                    .query_map([], |r| r.get::<_, String>(1))?
                    .collect::<rusqlite::Result<_>>()?;
                if !db::FIELDS.split(", ").all(|c| columns.contains(c)) {
                    push(&mut ix["problems"], "required-columns-missing");
                }
            }
            let mut current = true;
            for (source, image, _) in &images {
                let row = if tables.contains("memory_sources") {
                    db.query_row(
                        "select path,size,mtime_ns,digest from memory_sources where source=?",
                        [source],
                        |r| {
                            Ok((
                                r.get_ref(0)?.as_bytes()?.to_vec(),
                                r.get::<_, i64>(1)?,
                                r.get::<_, i64>(2)?,
                                r.get::<_, String>(3)?,
                            ))
                        },
                    )
                    .ok()
                } else {
                    None
                };
                current &= row
                    == Some((
                        image.path.as_os_str().as_encoded_bytes().to_vec(),
                        image.size,
                        image.mtime,
                        image.digest.clone(),
                    ));
            }
            ix["current"] = current.into();
            if !current {
                push(&mut ix["problems"], "source-index-mismatch");
            }
            if db::FIELDS.split(", ").all(|c| columns.contains(c)) {
                let entries = db::all(&db, "")?;
                ix["entries"] = entries.len().into();
                for source in ["shared", "pins", "failures"] {
                    ix["by_source"][source] =
                        entries.iter().filter(|e| e.source == source).count().into();
                }
                let legacy = entries
                    .iter()
                    .filter(|e| e.event_id.starts_with("legacy:"))
                    .count();
                ix["provenance"] = json!({"total":entries.len(),"modern":entries.len()-legacy,"legacy":legacy,"task_id":entries.iter().filter(|e|!e.task_id.is_empty()).count(),"session_hash":entries.iter().filter(|e|!e.session_hash.is_empty()).count(),"git_sha":entries.iter().filter(|e|!e.git_sha.is_empty()).count(),"git_dirty":entries.iter().filter(|e|e.git_dirty.is_some()).count(),"git_state":entries.iter().filter(|e|!e.git_state.is_empty()).count()});
            }
            let fts = tables.contains("memory_fts");
            ix["fts"] = fts.into();
            if fts {
                let count: i64 =
                    db.query_row("select count(*) from memory_fts", [], |r| r.get(0))?;
                ix["fts_entries"] = count.into();
                ix["fts_current"] = (ix["entries"] == count).into();
                if ix["fts_current"] != true {
                    push(&mut ix["problems"], "fts-entry-mismatch");
                }
            }
            if version != 3 {
                push(&mut ix["problems"], "schema-version-mismatch");
            }
            if integrity != "ok" {
                push(&mut ix["problems"], "integrity-check-failed");
            }
            Ok(())
        })();
        if result.is_err() {
            push(&mut report["index"]["problems"], "database-unreadable");
            report["status"] = "degraded".into();
            code = 1;
        } else {
            let degraded = invalid > 0
                || report["index"]["problems"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|s| {
                        [
                            "required-tables-missing",
                            "required-columns-missing",
                            "fts-entry-mismatch",
                            "schema-version-mismatch",
                            "integrity-check-failed",
                        ]
                        .contains(&s.as_str().unwrap_or(""))
                    });
            report["status"] = if degraded {
                "degraded"
            } else if report["index"]["current"] != true {
                "stale"
            } else {
                "healthy"
            }
            .into();
            if report["status"] != "healthy" {
                code = 1;
            }
        }
    }
    if as_json {
        return Ok((code, json_line(&report)?, Vec::new()));
    }
    let ix = &report["index"];
    let p = &ix["provenance"];
    let mut out = format!(
        "memory health: {}\nsources: shared={} pins={} failures={} invalid-ledger-rows={}\n",
        text(&report, "status"),
        report["sources"]["shared"]["entries"],
        report["sources"]["pins"]["entries"],
        report["sources"]["failures"]["entries"],
        invalid
    );
    if ix["present"] == true {
        out.push_str(&format!("index: current={} schema={}/3 integrity={} fts={} entries={}\nprovenance: modern={} legacy={} task={} session={} git-sha={}\n",if ix["current"]==true{"yes"}else{"no"},if ix["schema_version"].is_null(){"None".into()}else{ix["schema_version"].to_string()},text(ix,"integrity"),if ix["fts"]==true{"yes"}else{"fallback"},ix["entries"],p["modern"],p["legacy"],p["task_id"],p["session_hash"],p["git_sha"]));
    } else {
        out.push_str("index: absent\n");
    }
    let problems: Vec<_> = report["problems"]
        .as_array()
        .unwrap()
        .iter()
        .chain(ix["problems"].as_array().unwrap())
        .filter_map(Value::as_str)
        .collect();
    if !problems.is_empty() {
        out.push_str(&format!("problems: {}\n", problems.join(", ")));
    }
    out.push_str("assessment: index health only; memory quality not evaluated\n");
    Ok((code, out.into_bytes(), Vec::new()))
}
