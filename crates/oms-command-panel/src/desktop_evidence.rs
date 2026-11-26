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
    bound: usize,
}

#[derive(Default)]
struct Retained {
    answer: Option<Body>,
    raw_answer: Option<Body>,
    patch: Option<Body>,
    raw_patch: Option<Body>,
    raw_patch_withheld: bool,
    paths: Vec<String>,
    patch_digest: Option<String>,
}

/// Output without an explicit answer boundary can include prompts or tool logs.
pub(super) fn indexed_answer(store: &Store, row: &Value) -> Option<String> {
    let answer = results::unredacted_indexed_answer(store, row).ok()?;
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

fn raw_recorded_answer(
    store: &Store,
    id: &str,
    attempt: &Value,
    native: Option<&Value>,
    index: &[Value],
) -> Result<Option<Body>> {
    if let Some(report) = native {
        return Ok(report["summary"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(|text| Body {
                text: text.to_owned(),
                source: "native-report",
                digest: attempt["refs"]["native_result_sha256"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                bound: results::MAX_DRAFT,
            }));
    }
    let eligible = |r: &Value| {
        matches!(
            r["kind"].as_str(),
            Some("ask" | "call" | "consult" | "delegate" | "review")
        ) && !matches!(r["answer_quality"].as_str(), Some("empty" | "no_answer"))
    };
    if !index
        .iter()
        .any(|r| r["attempt_id"] == id && eligible(r) && r["artifact"].is_string())
    {
        return Ok(None);
    }
    let row = unique_row(index, id, "artifact_sha256", eligible)
        .ok_or_else(|| Error::new("ambiguous worker answer"))?;
    let text = results::unredacted_indexed_answer(store, row)?;
    if !matches!(text.lines().next(), Some("## Answer" | "## Findings"))
        || !text.lines().skip(1).any(|l| !l.trim().is_empty())
    {
        return Ok(None);
    }
    Ok(Some(Body {
        text,
        source: "indexed-answer",
        digest: row["artifact_sha256"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        bound: results::MAX_RESULT,
    }))
}

pub(super) struct WorkerPreview {
    pub text: String,
    pub source: &'static str,
    pub truncated: bool,
}

/// Read just the retained answer of an already authority-checked attempt, without opening its patch.
pub(super) fn worker_answer(
    repo: &Path,
    attempt: &Value,
) -> Result<Option<std::result::Result<WorkerPreview, oms_scrub::OutboundWithheld>>> {
    let Some(body) = worker_body(repo, attempt)? else {
        return Ok(None);
    };
    if body.text.len() > body.bound {
        return Err(Error::new("worker answer exceeds size bound"));
    }
    if oms_scrub::check_outbound(body.text.as_bytes()).is_err() {
        return Ok(Some(Err(oms_scrub::OutboundWithheld)));
    }
    let mut text = body.text;
    let length = text.len();
    text.truncate(text.floor_char_boundary(16 * 1024));
    let truncated = text.len() != length;
    Ok(Some(Ok(WorkerPreview {
        text,
        source: body.source,
        truncated,
    })))
}

fn worker_body(repo: &Path, attempt: &Value) -> Result<Option<Body>> {
    let store = Store::open(repo)?;
    let id = attempt["attempt_id"]
        .as_str()
        .ok_or_else(|| Error::new("missing attempt"))?;
    let native = if attempt["tool"] == "panel-native-assignment" {
        native_results::read(&store, attempt)?
    } else {
        None
    };
    let index = if native.is_none() {
        results::index_rows(repo)?
    } else {
        Vec::new()
    };
    raw_recorded_answer(&store, id, attempt, native.as_ref(), &index)
}

pub(super) fn worker_answer_page(
    repo: &Path,
    attempt: &Value,
    page: usize,
) -> Result<Option<std::result::Result<Value, oms_scrub::OutboundWithheld>>> {
    worker_body(repo, attempt)?
        .map(|body| body_page(&body, attempt, page))
        .transpose()
}

fn body_page(
    body: &Body,
    attempt: &Value,
    page: usize,
) -> Result<std::result::Result<Value, oms_scrub::OutboundWithheld>> {
    let part = match oms_scrub::outbound_page(
        &body.text,
        body.bound,
        oms_scrub::PageSelector::Number(page),
    ) {
        Ok(part) => part,
        Err(oms_scrub::PageError::OutboundWithheld) => {
            return Ok(Err(oms_scrub::OutboundWithheld));
        }
        Err(oms_scrub::PageError::InvalidRange) => {
            return Err(Error::new("Evidence page unavailable"));
        }
    };
    let revision = oms_panel_core::digest(
        json!([
            "desktop-answer-page-v1",
            body.source,
            body.digest,
            oms_panel_core::digest(body.text.as_bytes()),
            attempt["attempt_id"],
            attempt["refs"],
            attempt["provider"],
            attempt["tool"],
            attempt["task_id"]
        ])
        .to_string()
        .as_bytes(),
    );
    Ok(Ok(
        json!({"text":part.text,"page":page,"offset":part.offset,"bytes":part.end-part.offset,"totalBytes":part.total_bytes,
        "digest":body.digest,"revision":revision,"source":body.source,"nextPage":part.has_more.then_some(page+1),"hasMore":part.has_more}),
    ))
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
    result.raw_answer = raw_recorded_answer(&store, id, attempt, native.as_ref(), &index)
        .ok()
        .flatten();
    result.answer = result.raw_answer.as_ref().and_then(|raw| {
        Some(Body {
            text: oms_panel_core::redact(raw.text.as_bytes()).ok()?,
            source: raw.source,
            digest: raw.digest.clone(),
            bound: raw.bound,
        })
    });
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
        {
            let source = if native_patch.is_some() {
                "native-patch"
            } else {
                "indexed-patch"
            };
            let digest = row["patch_sha256"].as_str().unwrap_or_default().to_owned();
            let lossy = String::from_utf8_lossy(&raw).into_owned();
            let bound = if raw.len() <= results::PATCH_MAX {
                lossy.len().max(results::PATCH_MAX)
            } else {
                results::PATCH_MAX
            };
            if std::str::from_utf8(&raw).is_err() && oms_scrub::check_outbound(&raw).is_err() {
                result.raw_patch_withheld = true;
            } else {
                result.raw_patch = Some(Body {
                    text: lossy,
                    source,
                    digest: digest.clone(),
                    bound,
                });
            }
            if let Ok(text) = oms_panel_core::redact(&raw) {
                result.patch = Some(Body {
                    text,
                    source,
                    digest,
                    bound: results::PATCH_MAX,
                });
            }
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
    full_text: bool,
    _services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    let id = selected["attempt_id"]
        .as_str()
        .ok_or_else(|| Error::new("Missing recorded attempt"))?;
    if !matches!(section, "info" | "events" | "diff" | "files" | "transcript")
        || !(1..=10000).contains(&page)
        || (full_text && !matches!(section, "transcript" | "diff"))
        || stop.load(Ordering::Relaxed)
    {
        return Err(Error::new("Evidence page unavailable"));
    }
    let mut available = vec!["info", "events"];
    let mut proofs = json!({});
    let mut digest = Value::Null;
    let mut full = None;
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
                if full_text {
                    if section == "diff" && kept.raw_patch_withheld {
                        return Ok(
                            json!({"attempt":id,"section":section,"status":"outbound_withheld"}),
                        );
                    }
                    let raw_body = if section == "diff" {
                        kept.raw_patch
                    } else {
                        kept.raw_answer
                    }
                    .ok_or_else(|| Error::new("Verified retained evidence unavailable"))?;
                    source = raw_body.source;
                    digest = json!(raw_body.digest);
                    match body_page(&raw_body, selected, page)? {
                        Ok(part) => {
                            let text = part["text"]
                                .as_str()
                                .ok_or_else(|| Error::new("Evidence page unavailable"))?;
                            let rows = text.lines().map(str::to_owned).collect();
                            let next = part["nextPage"].as_u64().map(|n| n as usize);
                            full = Some(part);
                            Ok((rows, next, false))
                        }
                        Err(oms_scrub::OutboundWithheld) => {
                            return Ok(
                                json!({"attempt":id,"section":section,"status":"outbound_withheld"}),
                            );
                        }
                    }
                } else {
                    if section == "diff" && kept.raw_patch_withheld {
                        return Ok(
                            json!({"attempt":id,"section":section,"status":"outbound_withheld"}),
                        );
                    }
                    let raw_body = if section == "diff" {
                        kept.raw_patch
                    } else {
                        kept.raw_answer
                    }
                    .ok_or_else(|| Error::new("Verified retained evidence unavailable"))?;
                    match oms_scrub::outbound_page(
                        &raw_body.text,
                        raw_body.bound,
                        oms_scrub::PageSelector::Offset(0),
                    ) {
                        Ok(_) => {}
                        Err(oms_scrub::PageError::OutboundWithheld) => {
                            return Ok(
                                json!({"attempt":id,"section":section,"status":"outbound_withheld"}),
                            );
                        }
                        Err(oms_scrub::PageError::InvalidRange) => {
                            return Err(Error::new("Evidence page unavailable"));
                        }
                    }
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
            }
            _ => return Err(Error::new("Unsupported or unavailable evidence")),
        }
    }?;
    let mut result = json!({"attempt":id,"section":section,"page":page,"nextPage":next,"source":source,"sections":available,
        "proofs":proofs,"digest":digest,"lines":lines,"truncated":truncated});
    if let Some(full) = full {
        for key in [
            "text",
            "offset",
            "bytes",
            "totalBytes",
            "revision",
            "hasMore",
        ] {
            result[key] = full[key].clone();
        }
    }
    Ok(result)
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
        let fetch = |section, page| {
            read(
                root,
                &attempt,
                section,
                page,
                false,
                &svc,
                &AtomicBool::new(false),
            )
        };
        let fetch_full = |page| {
            read(
                root,
                &attempt,
                "transcript",
                page,
                true,
                &svc,
                &AtomicBool::new(false),
            )
        };
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
        let long = format!(
            "## Answer\n{}\n{}끝",
            "긴한줄🙂".repeat(1600),
            "후속 행\n".repeat(25)
        );
        let bytes = format!("## Output\nprivate noise\n{long}\n## Exit\n0\n");
        fs::write(root.join(".oms/artifacts/answer.md"), bytes.as_bytes()).unwrap();
        row["artifact_sha256"] = json!(oms_panel_core::digest(bytes.as_bytes()));
        save(&row);
        let legacy = fetch("transcript", 1).unwrap();
        assert_eq!(legacy["nextPage"], 2);
        assert_eq!(legacy["lines"].as_array().unwrap().len(), PAGE_SIZE);
        assert_eq!(
            legacy["lines"][1].as_str().unwrap().chars().count(),
            LINE_CHARS
        );
        assert_eq!(legacy["truncated"], true);
        assert!(legacy.get("text").is_none());
        let mut joined = String::new();
        let mut number = 1;
        let mut revision = None;
        loop {
            let part = fetch_full(number).unwrap();
            assert_eq!(part["offset"], joined.len());
            let text = part["text"].as_str().unwrap();
            assert!(text.len() <= oms_scrub::OUTBOUND_PAGE_BYTES);
            joined.push_str(text);
            assert_eq!(part["truncated"], false);
            let worker = worker_answer_page(root, &attempt, number)
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(worker["text"], part["text"]);
            if let Some(expected) = &revision {
                assert_eq!(&part["revision"], expected);
            } else {
                revision = Some(part["revision"].clone());
            }
            let Some(next) = part["nextPage"].as_u64() else {
                break;
            };
            number = next as usize;
        }
        assert_eq!(
            joined, long,
            "long lines and multibyte text must be readable without omitted bytes"
        );
        assert!(fetch_full(number + 1).is_err());

        // A secret or machine path straddling a page boundary withholds every page.
        let left = ["gh", "p_LeftHalf012345"].concat();
        let right = "RightHalf67890abcdef01";
        let answer_prefix = "## Answer\n";
        let straddling_answer = format!(
            "{answer_prefix}{} {left}{right} {}\n",
            "a".repeat(oms_scrub::OUTBOUND_PAGE_BYTES - answer_prefix.len() - 1 - left.len()),
            "z".repeat(400)
        );
        let straddling_bytes = format!("## Output\n{straddling_answer}## Exit\n0\n");
        fs::write(
            root.join(".oms/artifacts/answer.md"),
            straddling_bytes.as_bytes(),
        )
        .unwrap();
        row["artifact_sha256"] = json!(oms_panel_core::digest(straddling_bytes.as_bytes()));
        save(&row);
        for page_num in [1, 2] {
            let withheld = fetch_full(page_num).unwrap();
            assert_eq!(
                withheld,
                json!({"attempt":"bound","section":"transcript","status":"outbound_withheld"})
            );
            let wire = withheld.to_string();
            assert!(
                !wire.contains(&left) && !wire.contains(right) && !wire.contains(&"a".repeat(32))
            );
            assert_eq!(
                worker_answer_page(root, &attempt, page_num).unwrap(),
                Some(Err(oms_scrub::OutboundWithheld))
            );
        }

        let diff_header = "diff --git a/sample.txt b/sample.txt\n--- a/sample.txt\n+++ b/sample.txt\n@@ -1 +1 @@\n+";
        let straddling_patch = format!(
            "{diff_header}{} {left}{right} {}\n",
            "d".repeat(oms_scrub::OUTBOUND_PAGE_BYTES - diff_header.len() - 1 - left.len()),
            "e".repeat(400)
        );
        fs::write(
            root.join(".oms/artifacts/work.patch"),
            straddling_patch.as_bytes(),
        )
        .unwrap();
        row["patch_sha256"] = json!(oms_panel_core::digest(straddling_patch.as_bytes()));
        save(&row);
        for page_num in [1, 2] {
            let withheld = read(
                root,
                &attempt,
                "diff",
                page_num,
                true,
                &svc,
                &AtomicBool::new(false),
            )
            .unwrap();
            assert_eq!(
                withheld,
                json!({"attempt":"bound","section":"diff","status":"outbound_withheld"})
            );
            let wire = withheld.to_string();
            assert!(!wire.contains(&left) && !wire.contains(right));
        }
        fs::write(root.join(".oms/artifacts/work.patch"), patch).unwrap();
        row["patch_sha256"] = json!(oms_panel_core::digest(patch));
        save(&row);
        let changed = b"## Output\n## Answer\nnew retained answer\n## Exit\n0\n";
        fs::write(root.join(".oms/artifacts/answer.md"), changed).unwrap();
        assert!(
            fetch_full(1).is_err(),
            "changed evidence must still match the retained artifact digest"
        );
        row["artifact_sha256"] = json!(oms_panel_core::digest(changed));
        save(&row);
        assert_ne!(fetch_full(1).unwrap()["revision"], revision.unwrap());
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
