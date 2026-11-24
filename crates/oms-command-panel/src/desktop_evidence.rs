//! Display-only evidence from an attempt already authorized by the Room reader.
use crate::{Error, Result, Services};
use oms_panel_core::{Store, Value, files, native_results, results};
use serde_json::json;
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

const PAGE_SIZE: usize = 20;
const LINE_CHARS: usize = 200;

pub(super) fn sections(_attempt: &Value) -> Value {
    json!(["info", "events"])
}

struct Body {
    text: String,
    source: &'static str,
    digest: String,
}

#[derive(Default)]
struct Retained {
    answer: Option<Body>,
    patch: Option<Body>,
    paths: Vec<String>,
    patch_digest: Option<String>,
}

/// Output without an explicit answer boundary can include prompts or tool logs.
pub(super) fn indexed_answer(store: &Store, row: &Value) -> Option<String> {
    let answer = results::indexed_answer(store, row).ok()?;
    matches!(answer.lines().next(), Some("## Answer" | "## Findings")).then_some(answer)
}

fn unique_row<'a>(
    rows: &'a [Value],
    id: &str,
    digest_key: &str,
    eligible: impl Fn(&Value) -> bool,
) -> Option<&'a Value> {
    let mut matching = rows
        .iter()
        .filter(|r| r["attempt_id"] == id && r[digest_key].is_string() && eligible(r));
    let first = matching.next()?;
    matching
        .all(|r| r[digest_key] == first[digest_key])
        .then_some(first)
}

fn retained(repo: &Path, attempt: &Value) -> Retained {
    let mut result = Retained::default();
    let (Ok(store), Some(id)) = (Store::open(repo), attempt["attempt_id"].as_str()) else {
        return result;
    };
    let native = if attempt["tool"] == "panel-native-assignment" {
        native_results::read(&store, attempt).ok().flatten()
    } else {
        None
    };
    let index = results::index_rows(repo).unwrap_or_default();
    if let Some(report) = native.as_ref() {
        if let Some(text) = report["summary"].as_str().filter(|s| !s.is_empty())
            && let Ok(text) = oms_panel_core::redact(text.as_bytes())
        {
            result.answer = Some(Body {
                text,
                source: "native-report",
                digest: attempt["refs"]["native_result_sha256"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            });
        }
    } else if let Some(row) = unique_row(&index, id, "artifact_sha256", |r| {
        matches!(
            r["kind"].as_str(),
            Some("ask" | "call" | "consult" | "delegate" | "review")
        ) && !matches!(r["answer_quality"].as_str(), Some("empty" | "no_answer"))
    }) && let Some(text) = indexed_answer(&store, row)
    {
        result.answer = Some(Body {
            text,
            source: "indexed-answer",
            digest: row["artifact_sha256"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        });
    }
    let native_patch = native.as_ref().and_then(|r| {
        Some(json!({"patch":r["patch"]["path"].as_str()?,"patch_sha256":r["patch"]["sha256"].as_str()?}))
    });
    let patch = native_patch
        .as_ref()
        .or_else(|| unique_row(&index, id, "patch_sha256", |r| r["patch"].is_string()));
    if let Some(row) = patch
        && let Ok(raw) = results::verified_patch(&store, row)
        && let Ok(paths) = files::patch_paths(&raw)
        && !paths.is_empty()
    {
        // A binary patch contains encoded payloads, never useful as text in chat.
        result.paths = paths;
        result.patch_digest = row["patch_sha256"].as_str().map(str::to_owned);
        if !raw
            .split(|b| *b == b'\n')
            .any(|s| s.strip_suffix(b"\r").unwrap_or(s) == b"GIT binary patch")
            && let Ok(text) = oms_panel_core::redact(&raw)
        {
            result.patch = Some(Body {
                text,
                source: if native_patch.is_some() {
                    "native-patch"
                } else {
                    "indexed-patch"
                },
                digest: row["patch_sha256"].as_str().unwrap_or_default().to_owned(),
            });
        }
    }
    result
}

fn page_rows<'a>(
    items: impl Iterator<Item = &'a str>,
    page: usize,
) -> Result<(Vec<String>, Option<usize>, bool)> {
    let mut items = items.skip((page - 1) * PAGE_SIZE);
    let mut truncated = false;
    let rows = items
        .by_ref()
        .take(PAGE_SIZE)
        .map(|s| {
            let safe = oms_panel_core::redact(s.as_bytes())?;
            let mut chars = safe.chars();
            let line = chars.by_ref().take(LINE_CHARS).collect();
            truncated |= chars.next().is_some();
            Ok(line)
        })
        .collect::<Result<_>>()?;
    Ok((rows, items.next().map(|_| page + 1), truncated))
}

pub(super) fn read(
    repo: &Path,
    selected: &Value,
    section: &str,
    page: usize,
    _services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    let id = selected["attempt_id"]
        .as_str()
        .ok_or_else(|| Error::new("Missing recorded attempt"))?;
    if !matches!(section, "info" | "events" | "diff" | "files" | "transcript")
        || !(1..=10000).contains(&page)
        || stop.load(Ordering::Relaxed)
    {
        return Err(Error::new("Evidence page unavailable"));
    }
    let mut available = vec!["info", "events"];
    let mut proofs = json!({});
    let mut digest = Value::Null;
    let source;
    let (lines, next, truncated) = if section == "events" {
        source = "lifecycle";
        let rows = oms_command_operations::read_rows(&oms_command_operations::event_path(repo))
            .map_err(|_| Error::new("Lifecycle evidence unavailable"))?;
        let events: Vec<_> = rows
            .iter()
            .filter(|r| r.get("attempt_id").text() == Some(id))
            .map(|r| {
                ["ts", "event_type", "from_state", "to_state"]
                    .iter()
                    .filter_map(|k| r.get(k).text())
                    .collect::<Vec<_>>()
                    .join(" · ")
            })
            .collect();
        page_rows(events.iter().map(String::as_str), page)
    } else {
        let kept = retained(repo, selected);
        if let Some(a) = &kept.answer {
            available.push("transcript");
            proofs["transcript"] = json!(a.digest);
        }
        if !kept.paths.is_empty() {
            available.push("files");
            proofs["files"] = json!(kept.patch_digest);
        }
        if let Some(p) = &kept.patch {
            available.push("diff");
            proofs["diff"] = json!(p.digest);
        }
        match section {
            "info" => {
                source = "lifecycle";
                let info: Vec<_> = [
                    ("공급자", "provider"),
                    ("도구", "tool"),
                    ("작업", "task_id"),
                    ("상태", "state"),
                    ("생성", "created_at"),
                    ("갱신", "updated_at"),
                ]
                .iter()
                .filter_map(|(name, k)| selected.get(*k)?.as_str().map(|v| format!("{name} · {v}")))
                .collect();
                page_rows(info.iter().map(String::as_str), page)
            }
            "files" if !kept.paths.is_empty() => {
                source = "verified-patch-paths";
                digest = proofs[section].clone();
                page_rows(kept.paths.iter().map(String::as_str), page)
            }
            "diff" | "transcript" => {
                let body = if section == "diff" {
                    kept.patch
                } else {
                    kept.answer
                }
                .ok_or_else(|| Error::new("Verified retained evidence unavailable"))?;
                source = body.source;
                digest = json!(body.digest);
                page_rows(body.text.lines(), page)
            }
            _ => return Err(Error::new("Unsupported or unavailable evidence")),
        }
    }?;
    Ok(
        json!({"attempt":id,"section":section,"page":page,"nextPage":next,"source":source,"sections":available,
        "proofs":proofs,"digest":digest,"lines":lines,"truncated":truncated}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn retained_drilldown_verifies_origin_digest_answer_boundary_and_event_projection() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = format!("oms-evidence-{}-{nonce}", std::process::id());
        let repo = Fixture(std::env::temp_dir().join(dir));
        let root = &repo.0;
        let index = root.join(".oms/artifacts/index.jsonl");
        fs::create_dir_all(index.parent().unwrap()).unwrap();
        let event_file = oms_command_operations::event_path(root);
        fs::create_dir_all(event_file.parent().unwrap()).unwrap();
        let attempt = json!({"attempt_id":"bound","tool":"peer-ask","state":"done"});
        let answer = b"## Output\nprovider log\n## Answer\nRetained answer\n## Exit\n0\n";
        let patch = b"diff --git a/sample.txt b/sample.txt\n--- a/sample.txt\n+++ b/sample.txt\n@@ -1 +1 @@\n-old\n+new\n";
        let mut row = json!({"kind":"ask","attempt_id":"bound","artifact":".oms/artifacts/answer.md","artifact_sha256":oms_panel_core::digest(answer),"patch":".oms/artifacts/work.patch","patch_sha256":oms_panel_core::digest(patch)});
        let save = |row: &Value| fs::write(&index, format!("{row}\n")).unwrap();
        fs::write(root.join(row["artifact"].as_str().unwrap()), answer).unwrap();
        fs::write(root.join(row["patch"].as_str().unwrap()), patch).unwrap();
        save(&row);
        let events: Vec<_>=(0..21).map(|_| json!({"attempt_id":"bound","event_type":"transition","to_state":"done","argv":"unprojected arguments"}).to_string()).chain([json!({"attempt_id":"other","event_type":"unrelated transition"}).to_string()]).collect();
        fs::write(event_file, format!("{}\n", events.join("\n"))).unwrap();
        let svc = oms_panel_core::SubcommandServices::sibling().unwrap();
        let fetch =
            |section, page| read(root, &attempt, section, page, &svc, &AtomicBool::new(false));
        let info = fetch("info", 1).unwrap();
        assert_eq!(info["proofs"]["files"], row["patch_sha256"]);
        assert_eq!(info["proofs"]["diff"], row["patch_sha256"]);
        assert_eq!(fetch("files", 1).unwrap()["lines"], json!(["sample.txt"]));
        assert!(
            fetch("diff", 1).unwrap()["lines"]
                .to_string()
                .contains("+new")
        );
        assert_eq!(
            fetch("transcript", 1).unwrap()["lines"],
            json!(["## Answer", "Retained answer"])
        );
        let first = fetch("events", 1).unwrap();
        assert_eq!(first["nextPage"], 2);
        assert_eq!(first["lines"].as_array().unwrap().len(), 20);
        assert_eq!(
            fetch("events", 2).unwrap()["lines"],
            json!(["transition · done"])
        );
        assert!(!first.to_string().contains("unprojected"));
        fs::write(root.join(".oms/artifacts/work.patch"), b"replacement").unwrap();
        assert!(fetch("files", 1).is_err());
        assert!(fetch("diff", 1).is_err());
        let unmarked = b"## Output\nraw provider output\n## Exit\n0\n";
        fs::write(root.join(".oms/artifacts/answer.md"), unmarked).unwrap();
        assert!(fetch("transcript", 1).is_err());
        row["artifact_sha256"] = json!(oms_panel_core::digest(unmarked));
        save(&row);
        assert!(fetch("transcript", 1).is_err());
        row["attempt_id"] = json!("other");
        save(&row);
        assert!(
            fetch("info", 1).unwrap()["proofs"]
                .as_object()
                .unwrap()
                .is_empty()
        );
    }
}
