//! Project a complete, bounded lifecycle stream using the same transition and
//! identity rules as `project_attempts`. Only fixed display fields leave it.
use crate::{
    projection::{self, Projection},
    storage,
    value::V,
};
use sha2::{Digest, Sha256};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

pub const MAX_SOURCE_BYTES: usize = 512 * 1024;
const MAX_SOURCE_ROWS: usize = 2048;
const MAX_ROW_BYTES: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionIssue {
    Invalid,
    SourceLimit,
    BudgetLimit,
    Deadline,
    Interrupted,
}

#[derive(Debug, Eq, PartialEq)]
pub struct ObservedAttempt {
    pub id: String,
    pub source_id: String,
    pub provider: Option<&'static str>,
    pub tool: Option<&'static str>,
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
}

fn display(source: &str, value: V) -> Result<ObservedAttempt, ProjectionIssue> {
    let mut h = Sha256::new();
    h.update(b"oms-observed-lifecycle-attempt-v1\0");
    h.update(source.as_bytes());
    h.update([0]);
    h.update(projection::identity(value.get("attempt_id")));
    let timestamp = projection::stamp(value.get("updated_at"))
        .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
        .ok_or(ProjectionIssue::Invalid)?;
    let created = projection::stamp(value.get("created_at"))
        .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
        .unwrap_or(timestamp);
    let known = |field: &str, values: &'static [&'static str]| {
        let raw = value.get(field).text()?;
        values.iter().copied().find(|v| *v == raw)
    };
    Ok(ObservedAttempt {
        id: format!("{:x}", h.finalize()),
        source_id: source.to_owned(),
        provider: known(
            "provider",
            &["codex", "claude", "antigravity", "agy", "pi", "local"],
        ),
        tool: known(
            "tool",
            &[
                "peer-ask",
                "peer-review",
                "peer-delegate",
                "consult",
                "advise",
                "panel-main",
                "panel-native-assignment",
                "panel-council",
            ],
        ),
        state: value
            .get("state")
            .text()
            .filter(|state| projection::state(state))
            .ok_or(ProjectionIssue::Invalid)?
            .to_owned(),
        created_at: created.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        updated_at: timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    })
}

/// `remaining_rows` charges all admitted physical rows, including rows in a
/// source subsequently found invalid. An incomplete suffix never becomes a history.
pub fn observed_attempts(
    raw: &[u8],
    source: &str,
    remaining_rows: &mut usize,
    deadline: Instant,
    interrupted: &AtomicBool,
) -> Result<Vec<ObservedAttempt>, ProjectionIssue> {
    use ProjectionIssue::*;
    let check = || {
        if interrupted.load(Ordering::Acquire) {
            Err(Interrupted)
        } else if Instant::now() >= deadline {
            Err(Deadline)
        } else {
            Ok(())
        }
    };
    check()?;
    if source.len() != 64
        || !source
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Invalid);
    }
    if raw.len() > MAX_SOURCE_BYTES {
        return Err(SourceLimit);
    }
    if std::str::from_utf8(raw).is_err() {
        return Err(Invalid);
    }
    let input = storage::normalized(raw);
    if !input.is_empty() && !input.ends_with('\n') {
        return Err(Invalid);
    }
    let physical = input.bytes().filter(|b| *b == b'\n').count();
    if physical > MAX_SOURCE_ROWS {
        return Err(SourceLimit);
    }
    if physical > *remaining_rows {
        return Err(BudgetLimit);
    }
    *remaining_rows -= physical;
    let mut projection = Projection::default();
    for line in input.split_terminator('\n') {
        check()?;
        if line.len() > MAX_ROW_BYTES {
            return Err(SourceLimit);
        }
        if line
            .chars()
            .all(|c| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
        {
            continue;
        }
        let row = V::parse_with_depth(line.as_bytes(), 48).map_err(|_| Invalid)?;
        projection
            .extend_checked(std::slice::from_ref(&row), interrupted)
            .map_err(|_| check().err().unwrap_or(Invalid))?;
    }
    let mut out = Vec::new();
    for attempt in projection.into_ordered_attempts() {
        check()?;
        out.push(display(source, attempt)?);
    }
    check()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{new_event, value::obj};
    use std::time::Duration;

    #[test]
    fn native_typed_ids_and_whole_source_budgets() {
        let stop = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(3);
        let source = "e".repeat(64);
        let parse = |bytes: &[u8], budget: &mut usize| {
            observed_attempts(bytes, &source, budget, deadline, &stop)
        };
        let first = new_event(
            "1",
            1usize.into(),
            "attempt.created",
            obj([("to_state", "queued".into())]),
        )
        .unwrap();
        let mut second = new_event(
            "1",
            1usize.into(),
            "attempt.created",
            obj([("to_state", "queued".into())]),
        )
        .unwrap();
        second.set("attempt_id", 1usize);
        let mut raw = first.json(false).unwrap();
        raw.push(b'\n');
        raw.extend(second.json(false).unwrap());
        raw.push(b'\n');
        let mut budget = 2;
        let both = parse(&raw, &mut budget).unwrap();
        assert_eq!((both.len(), budget), (2, 0));
        assert_ne!(both[0].id, both[1].id);
        assert_eq!(parse(&raw, &mut 1), Err(ProjectionIssue::BudgetLimit));
        assert_eq!(
            parse(
                &vec![b'\n'; MAX_SOURCE_ROWS + 1],
                &mut (MAX_SOURCE_ROWS + 1)
            ),
            Err(ProjectionIssue::SourceLimit)
        );
        assert_eq!(
            parse(&raw[..raw.len() - 1], &mut 2),
            Err(ProjectionIssue::Invalid)
        );
        let mut invalid = raw.clone();
        invalid.extend_from_slice(&raw);
        assert_eq!(parse(&invalid, &mut 4), Err(ProjectionIssue::Invalid));
        stop.store(true, Ordering::Release);
        assert_eq!(parse(&raw, &mut 2), Err(ProjectionIssue::Interrupted));
    }
}
