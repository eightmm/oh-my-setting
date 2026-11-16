//! Append-oriented plain event lines. Blind reads reset the comparison frame.
use crate::Value;
use crate::value::{array, text};
use std::collections::BTreeMap;
pub fn clean(value: &str, maximum: usize) -> String {
    let text = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.chars().count() <= maximum {
        text
    } else if maximum == 0 {
        String::new()
    } else {
        text.chars()
            .take(maximum - 1)
            .chain(std::iter::once('~'))
            .collect()
    }
}
fn word(value: &str, maximum: usize) -> String {
    let v = clean(value, maximum).replace(' ', "_");
    if v.is_empty() { "-".into() } else { v }
}
pub fn line(at: &str, room: &str, member: &Value, old: Option<&str>) -> String {
    format!(
        "{} room={} role={} {} -> {} {}",
        word(at, 8),
        word(room, 64),
        word(&text(member, "role"), 32),
        word(old.unwrap_or("-"), 32),
        word(member["state"].as_str().unwrap_or("unknown"), 32),
        word_title(member)
    )
}
fn word_title(member: &Value) -> String {
    let v = clean(&text(member, "title"), 80);
    if v.is_empty() { "-".into() } else { v }
}
#[derive(Default)]
pub struct EventLog {
    room: Option<String>,
    known: BTreeMap<String, String>,
    seeded: bool,
}
impl EventLog {
    pub fn lines(&mut self, report: &Value, at: &str) -> Vec<String> {
        if report["collection"]["ok"] != true {
            self.known.clear();
            self.seeded = false;
            return vec![];
        }
        let room = text(&report["room"], "id");
        if self.room.as_deref() != Some(&room) {
            self.known.clear();
            self.seeded = false;
            self.room = Some(room.clone());
        }
        let mut output = vec![];
        let mut known = BTreeMap::new();
        for member in array(&report["room"]["participants"]) {
            let id = text(member, "participant");
            if id.is_empty() {
                continue;
            }
            let state = text(member, "state");
            if !self.seeded || self.known.get(&id) != Some(&state) {
                output.push(line(
                    at,
                    &room,
                    member,
                    self.known.get(&id).map(String::as_str),
                ));
            }
            known.insert(id, state);
        }
        self.known = known;
        self.seeded = true;
        output
    }
}
