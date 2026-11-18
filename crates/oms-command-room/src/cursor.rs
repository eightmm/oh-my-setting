//! Opaque v1 cursors bind a stream offset to device, inode and boundary hashes.
use crate::model::{member, project, unread_messages, visible_to};
use crate::value::{Value, encode, obj, parse};
use crate::{Error, Result, Room, Snapshot, checkpoint, digest};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
fn b64(bytes: &[u8]) -> String {
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let v = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        out.push(ALPHABET[(v >> 18) as usize] as char);
        out.push(ALPHABET[((v >> 12) & 63) as usize] as char);
        if c.len() > 1 {
            out.push(ALPHABET[((v >> 6) & 63) as usize] as char);
        }
        if c.len() > 2 {
            out.push(ALPHABET[(v & 63) as usize] as char);
        }
    }
    out
}
fn unb64(text: &str) -> Result<Vec<u8>> {
    if text.is_empty() || text.len() > 1024 || text.len() % 4 == 1 {
        return Err(Error::new("invalid thread cursor"));
    }
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for b in text.bytes() {
        let v = ALPHABET
            .iter()
            .position(|a| *a == b)
            .ok_or_else(|| Error::new("invalid thread cursor"))? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}
fn anchor(s: &Snapshot, offset: usize) -> String {
    let mut bytes = s.bytes[..offset.min(256)].to_vec();
    bytes.extend_from_slice(&s.bytes[offset.saturating_sub(256)..offset]);
    digest(&bytes)
}
#[allow(clippy::unnecessary_cast)]
fn cursor(s: &Snapshot, offset: usize) -> Result<String> {
    let v = Value::Array(vec![
        1u64.into(),
        s.id.clone().into(),
        (s.stat.st_dev as u64).into(),
        (s.stat.st_ino as u64).into(),
        offset.into(),
        anchor(s, offset).into(),
    ]);
    Ok(b64(&encode(&v, false, true)?))
}
fn cursor_room(text: &str) -> Option<String> {
    let raw = unb64(text).ok()?;
    let v = parse(&raw, 8192).ok()?;
    v.as_array()?.get(1)?.as_str().map(str::to_owned)
}
#[allow(clippy::unnecessary_cast)]
fn offset(s: &Snapshot, text: &str) -> Result<usize> {
    if text.is_empty() {
        return Ok(0);
    }
    let raw = unb64(text)?;
    let v = parse(&raw, 8192)?;
    let a = v
        .as_array()
        .ok_or_else(|| Error::new("invalid thread cursor"))?;
    if a.len() != 6 {
        return Err(Error::new("invalid thread cursor"));
    }
    let at = a[4]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| Error::new("invalid thread cursor"))?;
    if a[0].as_u64() != Some(1)
        || a[1].as_str() != Some(&s.id)
        || a[2].as_u64() != Some(s.stat.st_dev as u64)
        || a[3].as_u64() != Some(s.stat.st_ino as u64)
        || at > s.bytes.len()
        || a[5].as_str() != Some(anchor(s, at).as_str())
    {
        return Err(Error::new(
            "stale thread cursor; inspect the thread before restarting delivery",
        ));
    }
    if at > 0 && s.bytes[at - 1] != b'\n' {
        return Err(Error::new("cursor is not at a complete turn"));
    }
    Ok(at)
}
impl Room<'_> {
    pub fn updates(
        &self,
        id: &str,
        who: &str,
        after: &str,
        budget: usize,
        limit: usize,
    ) -> Result<Value> {
        self.updates_with_state(id, who, after, budget, limit, None)
    }
    pub fn updates_with_state(
        &self,
        id: &str,
        who: &str,
        after: &str,
        budget: usize,
        limit: usize,
        state: Option<&Value>,
    ) -> Result<Value> {
        if !(1..=crate::MAX_ROW).contains(&budget) || !(1..=200).contains(&limit) {
            return Err(Error::new(
                "updates requires bytes 1..65536 and turns 1..200",
            ));
        }
        let id = self.resolve(id)?;
        let snapshot = self.snapshot(&id)?;
        let loaded;
        if let Some(state) = state {
            loaded = state.clone();
        } else {
            loaded = project(&snapshot.rows)?;
        }
        let state = &loaded;
        let p = member(state, who)?;
        if state.text("id") != id {
            return Err(Error::new("updates projection names another room"));
        }
        let mut after = after;
        if !after.is_empty()
            && let Some(old) = cursor_room(after)
            && old != id
            && self.predecessors(&id, true)?.contains(&old)
        {
            self.notices.borrow_mut().push(format!(
                "room: {old} continues as {id}; reading unread mail from its start"
            ));
            after = "";
        }
        if !after.is_empty() {
            let start = offset(&snapshot, after)?;
            let mut delta = self.delta(&snapshot, start, budget, limit, true)?;
            let turns = delta
                .list("turns")
                .iter()
                .filter(|r| visible_to(r, who, p.number("seq")))
                .cloned()
                .collect::<Vec<_>>();
            delta.set("turns", turns);
            return Ok(delta);
        }
        let known: BTreeSet<_> = state
            .list("messages")
            .iter()
            .map(|m| m.text("id"))
            .collect();
        let unread = unread_messages(state, who)?;
        let unread: BTreeSet<_> = unread.iter().map(|m| m.text("id")).collect();
        let at = snapshot
            .rows
            .iter()
            .position(|r| r["seq"] == p["seq"])
            .ok_or_else(|| Error::new("room enrollment turn is unavailable"))?;
        let enrollment = &snapshot.rows[at];
        if !matches!(
            enrollment["room_event"].text("kind"),
            "join" | "main-resume"
        ) || enrollment["room_event"].text("participant") != who
        {
            return Err(Error::new("room enrollment turn changed"));
        }
        let mut current = snapshot.ranges[at].end;
        let mut turns = Vec::new();
        let mut used = 0;
        let mut more = false;
        let mut stopped = false;
        for (row, range) in snapshot.rows.iter().zip(&snapshot.ranges).skip(at + 1) {
            checkpoint(self.stop)?;
            let e = &row["room_event"];
            if e.text("kind") == "message" && !known.contains(e.text("id")) {
                current = range.start;
                more = true;
                stopped = true;
                break;
            }
            if visible_to(row, who, p.number("seq")) && unread.contains(e.text("id")) {
                if turns.len() >= limit || !turns.is_empty() && used + range.len() > budget {
                    more = true;
                    stopped = true;
                    break;
                }
                turns.push(row.clone());
                used += range.len();
                current = range.end;
            }
        }
        if !stopped {
            current = snapshot.bytes.len();
        }
        Ok(obj([
            ("schema", 1u64.into()),
            ("thread", id.into()),
            ("turns", turns.into()),
            ("cursor", cursor(&snapshot, current)?.into()),
            ("has_more", more.into()),
        ]))
    }
    fn delta(
        &self,
        s: &Snapshot,
        start: usize,
        budget: usize,
        limit: usize,
        first_over: bool,
    ) -> Result<Value> {
        let mut at = start;
        let mut used = 0;
        let mut rows = Vec::new();
        for (row, range) in s
            .rows
            .iter()
            .zip(&s.ranges)
            .filter(|(_, r)| r.start >= start)
        {
            checkpoint(self.stop)?;
            if rows.len() >= limit {
                break;
            }
            if used + range.len() > budget {
                if rows.is_empty() {
                    if first_over {
                        rows.push(row.clone());
                        at = range.end;
                        break;
                    }
                    return Err(Error::new(
                        "next complete turn exceeds byte budget; increase --max-bytes",
                    ));
                }
                break;
            }
            rows.push(row.clone());
            used += range.len();
            at = range.end;
        }
        // Blank lines are accepted by records but do not deliver a phantom row.
        if s.rows.iter().zip(&s.ranges).all(|(_, r)| r.end <= at) {
            at = s.bytes.len();
        }
        Ok(obj([
            ("schema", 1u64.into()),
            ("thread", s.id.clone().into()),
            ("turns", rows.into()),
            ("cursor", cursor(s, at)?.into()),
            ("has_more", (at < s.bytes.len()).into()),
        ]))
    }
    pub fn thread_updates(
        &self,
        id: &str,
        after: &str,
        budget: usize,
        limit: usize,
        wait: f64,
        first_over: bool,
    ) -> Result<Value> {
        if !(1..=crate::MAX_ROW).contains(&budget)
            || !(1..=200).contains(&limit)
            || !wait.is_finite()
            || !(0.0..=30.0).contains(&wait)
        {
            return Err(Error::new(
                "updates requires bytes 1..65536, turns 1..200, wait 0..30",
            ));
        }
        let end = Instant::now() + Duration::from_secs_f64(wait);
        let mut snapshot = self.snapshot(id)?;
        let mut reader = crate::storage::LogReader::open(
            &self.storage()?.threads,
            crate::storage::Store::name(id),
        )?;
        let at = offset(&snapshot, after)?;
        let mut delta = self.delta(&snapshot, at, budget, limit, first_over)?;
        loop {
            checkpoint(self.stop)?;
            if !delta.list("turns").is_empty() || Instant::now() >= end {
                return Ok(delta);
            }
            std::thread::sleep(
                Duration::from_millis(50).min(end.saturating_duration_since(Instant::now())),
            );
            if let Some(updated) = reader.poll(&snapshot)? {
                // A retained cursor must still bind the generation after replacement/truncation.
                let at = offset(&updated, delta.text("cursor"))?;
                snapshot = std::sync::Arc::new(updated);
                delta = self.delta(&snapshot, at, budget, limit, first_over)?;
            }
        }
    }
    pub fn thread_receipt(&self, id: &str, after: &str, consumer: &str) -> Result<Value> {
        crate::identifier(consumer, "consumer")?;
        if after.is_empty() {
            return Err(Error::new("ack requires the cursor actually consumed"));
        }
        let snapshot = self.snapshot(id)?;
        offset(&snapshot, after)?;
        Ok(obj([
            ("receipt", "ack".into()),
            ("consumer", consumer.into()),
            ("after", after.into()),
        ]))
    }
}
