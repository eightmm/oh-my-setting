//! Journal proposals are durable source records; review still owns eligibility.
use crate::{
    Error, Result, hash, records,
    store::{self, Store},
};
use std::{collections::BTreeSet, path::Path, sync::atomic::AtomicBool};

const MAX_PROPOSALS: usize = 5;
const MAX_BODY: usize = 16 * 1024;

#[derive(Clone, Copy)]
pub struct JournalProposal<'a> {
    pub key: &'a str,
    pub source_sha256: &'a str,
    pub body: &'a str,
    /// False permits reconciliation of an earlier publication, never a new append.
    pub source_current: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum JournalPublication {
    Appended(String),
    AlreadyPresent(String),
    Refused(String),
}

#[derive(Debug)]
pub struct JournalBatchReport {
    pub outcomes: Vec<JournalPublication>,
    pub warnings: Vec<u8>,
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Publish at most one journal pass into the selected project's existing log.
/// Keys and payload digests share the source publication boundary. Replaying a
/// batch reconciles source records, including retired/redacted ones, before any
/// new append. A source error may follow publication; retry the same batch.
pub fn append_journal_batch(
    repo: &Path,
    batch_id: &str,
    proposals: &[JournalProposal<'_>],
    interrupted: &AtomicBool,
) -> std::result::Result<JournalBatchReport, String> {
    publish(repo, batch_id, proposals, interrupted).map_err(|Error(_, message)| message)
}

fn publish(
    repo: &Path,
    batch_id: &str,
    proposals: &[JournalProposal<'_>],
    flag: &AtomicBool,
) -> Result<JournalBatchReport> {
    crate::check(flag)?;
    let mut keys = BTreeSet::new();
    if !digest(batch_id)
        || proposals.len() > MAX_PROPOSALS
        || proposals
            .iter()
            .any(|p| !digest(p.key) || !digest(p.source_sha256) || !keys.insert(p.key))
    {
        return crate::err("invalid or duplicate journal proposal identity; nothing appended");
    }
    let repo = std::fs::canonicalize(repo)?;
    let repo = store::repo_root(&repo, &repo, flag)?;
    let s = Store::new(repo, None, false);
    // Provenance acquisition can invoke Git. Keep it outside the source lock.
    let metadata: Vec<_> = proposals
        .iter()
        .map(|_| s.metadata("distill", flag))
        .collect();
    let _guard = store::lock(&s.shared, flag)?;
    let source = store::read(&s.shared)?;
    let entries = records::parse(&source, "shared");
    let mut outcomes = Vec::with_capacity(proposals.len());
    let mut records: Vec<u8> = Vec::new();
    for (proposal, mut metadata) in proposals.iter().zip(metadata) {
        crate::check(flag)?;
        let body = proposal.body.trim();
        let payload = hash(body.as_bytes());
        let matching: Vec<_> = entries
            .iter()
            .filter(|e| e.field("journal_candidate") == proposal.key)
            .collect();
        if !matching.is_empty() {
            if matching.len() == 1 {
                let e = matching[0];
                if e.kind == "distill"
                    && e.agent == "journal"
                    && e.field("journal_scope") == "project"
                    && e.field("journal_source_sha256") == proposal.source_sha256
                    && e.field("journal_payload_sha256") == payload
                    && e.field("event_id") == e.event_id
                    && (records::redacted(&e.body) || hash(e.body.as_bytes()) == payload)
                {
                    outcomes.push(JournalPublication::AlreadyPresent(e.event_id.clone()));
                    continue;
                }
            }
            outcomes.push(JournalPublication::Refused(
                "journal candidate conflicts with an earlier source publication".into(),
            ));
            continue;
        }
        if !proposal.source_current {
            outcomes.push(JournalPublication::Refused(
                "journal source changed before publication; retained for recomputation".into(),
            ));
            continue;
        }
        if body.is_empty()
            || body.len() > MAX_BODY
            || body.contains(records::START)
            || !store::clean(body, true)
        {
            outcomes.push(JournalPublication::Refused(
                "journal proposal is empty, oversized, reserved or sensitive; not appended".into(),
            ));
            continue;
        }
        for (key, value) in [
            ("journal_candidate", proposal.key),
            ("journal_scope", "project"),
            ("journal_source_sha256", proposal.source_sha256),
            ("journal_payload_sha256", payload.as_str()),
            ("journal_batch", batch_id),
        ] {
            metadata.insert(key.into(), value.into());
        }
        records.extend(store::metadata_text(&metadata).as_bytes());
        records.extend(format!("## {} journal\n\n{body}\n\n", crate::now()).as_bytes());
        outcomes.push(JournalPublication::Appended(metadata["event_id"].clone()));
    }
    if !records.is_empty() {
        s.ignore(flag)?;
        store::mutate(&s.shared, flag, |current| {
            if current != source {
                return crate::err(
                    "memory source changed during journal publication; retry the batch",
                );
            }
            let mut next = if source.is_empty() && !s.shared.exists() {
                s.initial().into_bytes()
            } else {
                source.clone()
            };
            next.extend(records);
            Ok(next)
        })?;
    }
    drop(_guard);
    if outcomes
        .iter()
        .any(|o| !matches!(o, JournalPublication::Refused(_)))
    {
        // An idempotent replay also repairs a refresh interrupted after publication.
        s.after_write(flag);
    }
    Ok(JournalBatchReport {
        outcomes,
        warnings: s.take_warnings(),
    })
}
