//! Read-only advice from the recorded CI/PR/review facts and the source rule
//! table. Missing findings or comments cannot produce a readiness suggestion.
use crate::{Context, Error, Output, Result, V, cli::Options, git::Git, obj, storage, string};
use std::path::{Path, PathBuf};
pub(crate) fn execute(c: &Context<'_>, o: &Options, repo: &Path, branch: &str) -> Result<Output> {
    let rules = V::parse(&storage::read(
        c.source,
        Path::new("config/ci-suggestions.json"),
        false,
    )?)?;
    let ledger = std::env::var_os("OMS_CI_LEDGER")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo.join(".oms/ci.jsonl"));
    let rows = storage::parse_rows(&storage::read_external(&ledger, true)?)?;
    let rows: Vec<_> = rows
        .iter()
        .filter(|r| string(r, "branch") == branch)
        .collect();
    let tip = Git::new(repo, c.interrupted)
        .commit(branch)
        .unwrap_or_else(|_| rows.last().map(|r| string(r, "sha")).unwrap_or_default());
    let mine: Vec<_> = rows
        .iter()
        .copied()
        .filter(|r| string(r, "sha") == tip)
        .collect();
    let latest = mine.last().copied();
    let completed = latest.is_some_and(|r| string(r, "status") == "completed");
    let failed = completed
        && latest.is_some_and(|r| {
            ["failure", "timed_out", "startup_failure"].contains(&string(r, "conclusion").as_str())
        });
    let mut facts = V::Obj(Default::default());
    facts.set(
        "ci",
        latest
            .map(|r| {
                if completed {
                    format!("{} (recorded {})", string(r, "conclusion"), string(r, "ts"))
                } else {
                    format!(
                        "pending ({}, recorded {})",
                        string(r, "status"),
                        string(r, "ts")
                    )
                }
            })
            .unwrap_or_else(|| {
                if rows.is_empty() {
                    "unknown (nothing recorded)".into()
                } else {
                    "unknown (no row for the tip)".into()
                }
            }),
    );
    let mut class = String::from("unknown");
    let mut class_note = String::new();
    if failed {
        if o.has("refresh") {
            match c.services.ci_comparison(repo, branch, &tip, c.interrupted) {
                Ok(value) => {
                    class = string(&value, "class");
                    class_note = string(&value, "note");
                }
                Err(_) => class_note = "triage could not run".into(),
            }
            facts.set("base comparison", format!("{class} ({class_note})"));
        } else {
            facts.set(
                "base comparison",
                "unknown (not recorded; pass --refresh or run ci-status triage)",
            );
        }
    }
    let mut red = None;
    let mut flaky = None;
    for row in &mine {
        if ["failure", "timed_out", "startup_failure"].contains(&string(row, "conclusion").as_str())
        {
            red = Some(*row);
        } else if string(row, "conclusion") == "success" {
            if let Some(bad) = red {
                flaky = Some((bad, *row));
            }
        }
    }
    let pr = rows.iter().rev().find_map(|r| {
        r.get("pr_number")
            .integer(true)
            .then(|| r.get("pr_number").py())
    });
    let mut packet: Option<(PathBuf, V)> = None;
    if let Some(pr) = &pr {
        let directory = repo.join(".oms/artifacts/pr-comments");
        if let Ok(files) = std::fs::read_dir(&directory) {
            for file in files.flatten() {
                let name = file.file_name().to_string_lossy().into_owned();
                if !name.starts_with(&format!("pr{pr}-")) || !name.ends_with(".json") {
                    continue;
                }
                if let Ok(raw) = storage::read(repo, &file.path(), false) {
                    if let Ok(row) = V::parse(&raw) {
                        if matches!(row.get("comments"), V::Arr(_))
                            && packet.as_ref().is_none_or(|(_, p)| {
                                string(&row, "read_at") >= string(p, "read_at")
                            })
                        {
                            packet = Some((file.path(), row));
                        }
                    }
                }
            }
        }
    }
    let current = packet.as_ref().map_or(0, |(_, p)| {
        p.get("comments")
            .array()
            .iter()
            .filter(|r| string(r, "state") == "current")
            .count()
    });
    facts.set(
        "review comments",
        if let Some((_, p)) = &packet {
            format!(
                "{current} current of {} (packet read {})",
                p.get("comments").array().len(),
                string(p, "read_at")
            )
        } else if let Some(pr) = &pr {
            format!("unknown (no packet for PR #{pr}; draft-pr comments stores one)")
        } else {
            "unknown (no PR number in the CI record)".into()
        },
    );
    let findings = c.services.review_findings(repo).ok();
    let total = findings.as_ref().map_or(0, |f| f.array().len());
    let blocking: Vec<_> = findings
        .as_ref()
        .map(|f| {
            f.array()
                .iter()
                .filter(|r| string(r, "status") == "open" && string(r, "severity") == "blocking")
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    facts.set(
        "blocking findings",
        if findings.is_some() {
            format!("{} open of {total} recorded", blocking.len())
        } else {
            "unknown (could not read the findings)".into()
        },
    );
    let merge_rows = storage::rows(
        repo,
        Path::new(".oms/artifacts/pr-merged/observations.jsonl"),
    )?;
    let merged = merge_rows
        .iter()
        .rev()
        .find(|r| string(r, "branch") == branch);
    facts.set(
        "merge",
        merged
            .map(|m| {
                format!(
                    "merged, tree {} (observed {})",
                    string(m, "tree"),
                    string(m, "observed_at")
                )
            })
            .unwrap_or_else(|| "not observed".into()),
    );
    let mut suggestions = vec![];
    for rule in rules.get("rules").array() {
        let id = string(rule, "id");
        let mut stamp = String::new();
        let mut packet_name = String::from("?");
        let evidence=match id.as_str(){
        "ci-not-recorded" if latest.is_none()=>Some(format!(".oms/ci.jsonl: {}",rows.last().map(|r|format!("the newest row is for {}",string(r,"sha").chars().take(12).collect::<String>())).unwrap_or_else(||"no row for this branch".into()))),
        "ci-pending" if latest.is_some() && !completed=>{let row=latest.unwrap();stamp=string(row,"ts");Some(format!(".oms/ci.jsonl {stamp}: {} on {}",string(row,"status"),tip.chars().take(12).collect::<String>()))},
        "ci-red"|"ci-red-regression"|"ci-red-preexisting" if failed=>{let wanted=match id.as_str(){"ci-red-regression"=>"regression","ci-red-preexisting"=>"preexisting",_=>"unknown"};if class!=wanted{None}else{let row=latest.unwrap();stamp=string(row,"ts");Some(format!(".oms/ci.jsonl {stamp}: {} on {}; failed jobs: {}{}",string(row,"conclusion"),tip.chars().take(12).collect::<String>(),if row.get("failed_jobs").array().is_empty(){"not recorded".into()}else{row.get("failed_jobs").array().iter().take(6).map(V::py).collect::<Vec<_>>().join(", ")},if class_note.is_empty(){String::new()}else{format!("; {class_note}")}))}},
        "ci-flaky"=>flaky.map(|(a,b)|format!(".oms/ci.jsonl: red {}, green {} on {}",string(a,"ts"),string(b,"ts"),tip.chars().take(12).collect::<String>())),
        "review-comments-current" if current>0=>{let(path,p)=packet.as_ref().unwrap();packet_name=path.strip_prefix(repo).unwrap_or(path).to_string_lossy().into_owned();stamp=string(p,"read_at");Some(format!("{packet_name} read {stamp}: {current} comment(s) on the PR head"))},
        "blocking-findings-open" if !blocking.is_empty()=>Some(format!("review findings: {} open blocking, e.g. {} {}",blocking.len(),string(&blocking[0],"id"),oms_contract_markdown::bounded_line(&string(&blocking[0],"summary"),80))),
        "ready-to-land" if completed && latest.is_some_and(|r|string(r,"conclusion")=="success") && findings.is_some() && blocking.is_empty() && current==0 && merged.is_none() && (pr.is_none() || packet.is_some())=>{stamp=string(latest.unwrap(),"ts");Some(format!(".oms/ci.jsonl {stamp}: success on {}; no open blocking finding, no current comment",tip.chars().take(12).collect::<String>()))},
        "merged-tree-changed"=>merged.filter(|m|string(m,"tree")=="changed-after-review").map(|m|format!("pr-merged observation {}: tree changed-after-review (merged {}, reviewed {})",string(m,"observed_at"),string(m,"merged_tree").chars().take(12).collect::<String>(),string(m,"reviewed_tree").chars().take(12).collect::<String>())),
        _=>None,
    };
        let Some(evidence) = evidence else { continue };
        let mut action = string(rule, "action");
        for (key, value) in [
            ("branch", branch),
            ("sha", tip.as_str()),
            ("packet", packet_name.as_str()),
            (
                "reviewed_tree",
                merged
                    .and_then(|m| m.get("reviewed_tree").text())
                    .unwrap_or("?"),
            ),
            (
                "merged_tree",
                merged
                    .and_then(|m| m.get("merged_tree").text())
                    .unwrap_or("?"),
            ),
        ] {
            action = action.replace(&format!("{{{key}}}"), value);
        }
        let mut item = obj([
            ("rule", id.into()),
            ("event", rule.get("event").clone()),
            ("say", rule.get("say").clone()),
            ("evidence", evidence.into()),
            ("action", action.into()),
        ]);
        if rule.get("escalate_after_s").truth() {
            let threshold = rule
                .get("escalate_after_s")
                .py()
                .parse::<u64>()
                .map_err(|_| Error::new(2, "invalid CI suggestion escalation interval"))?;
            let age = chrono::DateTime::parse_from_rfc3339(&stamp)
                .ok()
                .map(|time| crate::timestamp().saturating_sub(time.timestamp().max(0) as u64));
            item.set(
                "waiting_s",
                age.map(|n| V::Num(n.to_string())).unwrap_or(V::Null),
            );
            item.set("escalate_after_s", rule.get("escalate_after_s").clone());
            item.set(
                "over",
                age.map(|n| V::Bool(n > threshold)).unwrap_or(V::Null),
            );
            item.set("escalate_to", rule.get("escalate_to").clone());
        }
        suggestions.push(item);
    }
    let recorded = !rows.is_empty() || packet.is_some() || merged.is_some() || total > 0;
    let mut output = if o.has("json") {
        Output::json(&obj([
            ("schema", 1usize.into()),
            ("kind", "oms-ci-suggest".into()),
            ("branch", branch.into()),
            ("sha", tip.into()),
            ("refreshed", o.has("refresh").into()),
            ("recorded", recorded.into()),
            ("facts", facts.clone()),
            ("suggestions", V::Arr(suggestions.clone())),
        ]))?
    } else {
        let mut text = format!(
            "ci-status suggest [{branch}]: {} suggestion(s) from recorded facts{}\n",
            suggestions.len(),
            if recorded {
                ""
            } else {
                "; nothing is recorded to go on"
            }
        );
        if let V::Obj(fields) = facts {
            for (k, v) in fields {
                text += &format!("  {}: {}\n", String::from_utf8_lossy(&k), v.py());
            }
        }
        for item in suggestions {
            text += &format!(
                "- {}: {}\n    evidence: {}\n    why: {}\n    run: {}\n",
                string(&item, "rule"),
                string(&item, "event"),
                string(&item, "evidence"),
                string(&item, "say"),
                string(&item, "action")
            );
            if item.has("waiting_s") {
                text += &format!(
                    "    waiting: {}s; line: {}s; over: {}\n",
                    item.get("waiting_s").py(),
                    item.get("escalate_after_s").py(),
                    item.get("over").py()
                );
            }
        }
        text += "suggestions only: nothing was sent to an agent, retried, merged or recorded; a person decides\n";
        Output::text(text)
    };
    output.code = i32::from(!recorded);
    Ok(output)
}
