//! Bounded resume recall is selected before rendering, so long pins or unrelated
//! recent summaries cannot crowd the current objective out of the hook budget.
use crate::{
    Error, Output, Result, byte_clip, check, db, flat, records, review,
    store::{self, Store},
};
use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

const RESUME_BYTES: usize = 1200;
const RESUME_RECORDS: usize = 3;
const MAX_SOURCE_RECORDS: usize = 8192;

struct Source {
    store: Store,
    notes: Vec<records::Entry>,
    pins: Vec<records::Entry>,
    reviews: Vec<serde_json::Value>,
    shared_clean: bool,
    pins_clean: bool,
}

impl Source {
    fn read(store: Store) -> Result<Self> {
        let shared = store::read_bounded(&store.shared, review::VIEW_SOURCE_MAX)?;
        let pinned = store::read_bounded(&store.pins, review::VIEW_SOURCE_MAX)?;
        let notes = records::parse(&shared, "shared");
        let pins = records::parse(&pinned, "pins");
        if notes.len() + pins.len() > MAX_SOURCE_RECORDS {
            return crate::err("memory source exceeds the resume-view limit");
        }
        let reviews = review::bounded_rows(&store, "reviews.jsonl")?;
        Ok(Self {
            store,
            notes,
            pins,
            reviews,
            shared_clean: store::clean_bytes(&shared, true),
            pins_clean: store::clean_bytes(&pinned, true),
        })
    }

    fn select(
        &self,
        entries: Vec<records::Entry>,
        query: &str,
        targets: &str,
        flag: &AtomicBool,
    ) -> Result<Vec<records::Entry>> {
        let eligible = review::context_filter_rows(&self.reviews);
        Ok(db::select(
            &self.store,
            &db::Query {
                query,
                agent: "",
                limit: RESUME_RECORDS,
                recall: true,
                json: false,
                include_stale: false,
                include_superseded: false,
                targets,
                drop_scoped: true,
                context_only: true,
                handoffs: None,
                github: false,
            },
            entries,
            (&self.notes, &self.pins),
            Some(&eligible),
            flag,
        )?
        .entries)
    }
}

/// Read at most three eligible pins and three notes ranked for the selected
/// objective. Uses a transient index, leaves access receipts untouched, and
/// returns at most 1200 UTF-8 bytes. Automatic notes retain review eligibility.
pub fn resume_context(repo: &Path, query: &str, flag: &AtomicBool) -> Output {
    match resume(repo, query, flag) {
        Ok(body) => (0, body.into_bytes(), Vec::new()),
        Err(Error(code, message)) => (code, Vec::new(), format!("error: {message}\n").into_bytes()),
    }
}

fn resume(repo: &Path, query: &str, flag: &AtomicBool) -> Result<String> {
    let started = Instant::now();
    let within_budget = || -> Result<()> {
        check(flag)?;
        if started.elapsed() > Duration::from_secs(2) {
            return crate::err("memory resume lookup exceeded its time budget");
        }
        Ok(())
    };
    within_budget()?;
    let repo = std::fs::canonicalize(repo)?;
    let project = Source::read(Store::new(repo.clone(), None, false))?;
    let global = Source::read(Store::new(repo, None, true))?;
    let query = flat(byte_clip(query, 300));
    let targets = std::env::var("OMS_MEMORY_TARGET_PATHS").unwrap_or_default();
    let mut pins = Vec::new();
    for source in [&project, &global] {
        within_budget()?;
        if !source.pins_clean {
            continue;
        }
        let mut newest = source.pins.clone();
        newest.sort_by(|a, b| {
            b.occurred_at
                .cmp(&a.occurred_at)
                .then(b.ordinal.cmp(&a.ordinal))
        });
        pins.extend(
            source
                .select(newest, "", &targets, flag)?
                .into_iter()
                .map(|e| (source.store.scope.as_str(), e)),
        );
    }
    pins.sort_by(|a, b| {
        b.1.occurred_at
            .cmp(&a.1.occurred_at)
            .then(b.1.ordinal.cmp(&a.1.ordinal))
    });
    pins.truncate(RESUME_RECORDS);
    let mut recalled = Vec::new();
    if !query.is_empty() {
        for source in [&project, &global] {
            within_budget()?;
            if !source.shared_clean || crate::cli::child() && source.store.scope == "global" {
                continue;
            }
            let ranked = db::transient_ranked(&source.notes, &query, flag)?;
            let selected = source.select(ranked, &query, &targets, flag)?;
            if !selected.is_empty() {
                recalled.extend(
                    selected
                        .into_iter()
                        .map(|e| (source.store.scope.as_str(), e)),
                );
                break;
            }
        }
    }
    within_budget()?;
    let count = pins.len() + recalled.len();
    if count == 0 {
        return Ok(String::new());
    }
    let line_budget = RESUME_BYTES / count;
    let mut body = String::new();
    for (kind, rows) in [("pin", pins), ("recall", recalled)] {
        for (scope, entry) in rows {
            let prefix = format!("- {kind} {scope} [{}]: ", entry.event_id);
            let text = flat(&entry.body);
            body.push_str(&prefix);
            body.push_str(byte_clip(
                &text,
                line_budget.saturating_sub(prefix.len() + 1),
            ));
            body.push('\n');
        }
    }
    if !store::clean(&body, true) {
        return Ok(String::new());
    }
    within_budget()?;
    Ok(body)
}
