//! Pure replay of artifact-resolution relationships in ledger order.
//!
//! Readers retain their own parsing, byte limits and schema compatibility. The
//! view deliberately keeps every event ID, including ineligible rows, so an
//! invalid duplicate cannot make an otherwise ambiguous relationship unique.
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    Success,
    Failure,
    Invalid,
}

/// A reader's typed interpretation of one artifact-index row. `schema_one`
/// includes any additional row validity required by that reader. Lineage value
/// equality stays with the adapter so legacy telemetry values remain readable.
#[derive(Clone, Copy, Debug)]
pub struct Row<'a> {
    pub event_id: Option<&'a [u8]>,
    pub schema_one: bool,
    pub is_resolution: bool,
    pub exit: Exit,
    pub resolves_event_id: Option<&'a [u8]>,
    pub parent_event_id: Option<&'a [u8]>,
    pub resolved: bool,
}

#[derive(Debug, Default)]
pub struct Projection<'a> {
    pub resolved_event_ids: BTreeSet<&'a [u8]>,
    /// Original row positions, for readers that count invalid resolution rows.
    pub accepted_resolution_rows: BTreeSet<usize>,
}

/// Resolve a unique earlier failure only from a uniquely identified resolution.
/// `same_lineage(target, resolver)` compares operation and artifact identity
/// using the caller's existing value semantics. This projection grants no
/// authority to mutate the ledger or accept a task's result.
pub fn project<'a>(
    rows: &[Row<'a>],
    same_lineage: impl Fn(usize, usize) -> bool,
) -> Projection<'a> {
    let mut entries = BTreeMap::<&[u8], Option<usize>>::new();
    for (pos, row) in rows.iter().enumerate() {
        if let Some(id) = row.event_id {
            entries
                .entry(id)
                .and_modify(|entry| *entry = None)
                .or_insert(Some(pos));
        }
    }
    let mut result = Projection::default();
    for (pos, row) in rows.iter().enumerate() {
        if !row.is_resolution || !row.schema_one || row.exit != Exit::Success || !row.resolved {
            continue;
        }
        let (Some(id), Some(target_id)) = (row.event_id, row.resolves_event_id) else {
            continue;
        };
        let Some(Some(target_pos)) = entries.get(target_id) else {
            continue;
        };
        let target = &rows[*target_pos];
        if entries.get(id) == Some(&Some(pos))
            && *target_pos < pos
            && target.schema_one
            && target.exit == Exit::Failure
            && !target.is_resolution
            && row.parent_event_id == Some(target_id)
            && same_lineage(*target_pos, pos)
        {
            result.resolved_event_ids.insert(target_id);
            result.accepted_resolution_rows.insert(pos);
        }
    }
    result
}
