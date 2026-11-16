use crate::Value;
use crate::value::{array, text};
use std::collections::BTreeSet;
#[derive(Clone, Debug)]
pub struct Counts {
    pub finished: usize,
    pub failed: usize,
    pub needs: usize,
    pub marked_at: u64,
}
pub fn since_counts(report: &Value, ranked: &[Value]) -> Option<Counts> {
    let marked_at = report["attention_read"]["marked_at"].as_u64()?;
    if report["collection"]["ok"] != true {
        return None;
    }
    let cutoff =
        chrono::DateTime::<chrono::Utc>::from_timestamp(i64::try_from(marked_at).ok()?, 0)?
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string();
    let later = |v: &Value| {
        v.as_str()
            .is_some_and(|s| s.get(..19).is_some_and(|s| s > cutoff.as_str()))
    };
    let finished = ["recent", "active_recent"]
        .iter()
        .flat_map(|k| array(&report["attempts"][k]))
        .filter(|a| a["state"] == "done" && later(&a["updated_at"]))
        .filter_map(|a| a["attempt_id"].as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let failed = ranked
        .iter()
        .filter(|v| {
            [
                "failed",
                "guard_stop",
                "timed_out",
                "verify_failed",
                "verify_timed_out",
                "provider_error",
            ]
            .contains(&text(v, "cls").as_str())
                && later(&v["stamp"])
        })
        .count();
    let needs = ranked
        .iter()
        .filter(|v| v["cls"] == "needs" && later(&v["stamp"]))
        .count();
    if finished + failed + needs == 0 {
        None
    } else {
        Some(Counts {
            finished,
            failed,
            needs,
            marked_at,
        })
    }
}
pub fn since_line(report: &Value, ranked: &[Value], maximum: usize, now: u64) -> String {
    let Some(c) = since_counts(report, ranked) else {
        return String::new();
    };
    let age = now.saturating_sub(c.marked_at);
    let age = if age >= 3600 {
        format!("{}h", age / 3600)
    } else if age >= 60 {
        format!("{}m", age / 60)
    } else {
        format!("{age}s")
    };
    let parts: Vec<_> = [
        (c.finished, "finished"),
        (c.failed, "failed"),
        (c.needs, "needs you"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, t)| format!("{n} {t}"))
    .collect();
    crate::eventlog::clean(
        &format!("Since you looked {age} ago: {}", parts.join(" · ")),
        maximum,
    )
}
