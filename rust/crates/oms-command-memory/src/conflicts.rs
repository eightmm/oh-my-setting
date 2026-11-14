use crate::{
    store::{self, Store},
    *,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use unicode_normalization::UnicodeNormalization;
type Statements = (
    BTreeMap<String, BTreeSet<String>>,
    BTreeMap<(String, String), BTreeSet<String>>,
);
fn norm(s: &str) -> String {
    flat(
        &s.nfkc()
            .collect::<String>()
            .to_lowercase()
            .replace('’', "'")
            .replace(['`', '"', '*', '_'], ""),
    )
    .trim_matches([' ', '\t', '.', ';', ',', '!'])
    .into()
}
fn subject(s: &str) -> (String, String) {
    let ticked = regex::Regex::new(r"^`([^`]{1,80})`\s*(.*)$").unwrap();
    if let Some(c) = ticked.captures(s) {
        return (norm(&c[1]), norm(&c[2]));
    }
    let s = norm(s);
    let mut words: Vec<_> = s.split_whitespace().collect();
    while words
        .first()
        .is_some_and(|w| ["use", "using", "run", "call", "invoke"].contains(w))
    {
        words.remove(0);
    }
    while words.first().is_some_and(|w| {
        "the a an our this that these those any your my it them"
            .split_whitespace()
            .any(|s| s == *w)
    }) {
        words.remove(0);
    }
    let stop=words.iter().take(4).position(|w|"for in on when if unless because with from to instead over at during while as since until before after via here there now than".split_whitespace().any(|s|s==*w)).unwrap_or(words.len().min(4));
    (words[..stop].join(" "), words[stop..].join(" "))
}
fn statements(body: &str) -> Statements {
    let mut out: Statements = Default::default();
    let kv = regex::Regex::new(
        r"^\s*(?:[-*+]\s+|\d+[.)]\s+)?(?:\*\*)?([^:=\n]{1,40}?)(?:\*\*)?\s*[:=]\s*(.+?)\s*$",
    )
    .unwrap();
    let neg=regex::Regex::new(r"^(?:(?:and|but|so|also|please)\s+)*(?:(?:we|you|agents?|i|they|all)\s+)?(?:always\s+)?(?:do not|don't|dont|never|must not|mustn't|should not|shouldn't|cannot|can't|avoid|no longer|stop using)\s+(.+)$").unwrap();
    let pos=regex::Regex::new(r"^(?:(?:and|but|so|also|please)\s+)*(?:(?:we|you|agents?|i|they|all)\s+)?(?:always\s+|must\s+|should\s+)?(?:use|using|prefer|run)\s+(.+)$").unwrap();
    let conditional =
        regex::Regex::new(r"^(?:if|when|unless|once|while|after|before|until|whenever)\b").unwrap();
    let split = regex::Regex::new(r"[;,]\s*|\.\s+").unwrap();
    let mut fenced = false;
    let generic = "note notes why because context decision decisions summary todo fix example examples see warning tip result results task goal next next step imported from source link url ref refs reason rationale evidence impact background problem issue cause symptom lesson details detail rule update status state progress outcome files verification";
    for line in body.lines().take(40) {
        let stripped = line.trim();
        if stripped.starts_with("```") || stripped.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced
            || stripped.is_empty()
            || stripped.starts_with('>')
            || stripped.chars().count() > 300
            || stripped.ends_with('?')
        {
            continue;
        }
        if let Some(c) = kv.captures(line) {
            let key = norm(&c[1]);
            let value = norm(&c[2]);
            if key.starts_with(|c: char| c.is_ascii_lowercase())
                && key.split_whitespace().count() <= 4
                && !generic.split_whitespace().any(|s| s == key)
                && key != "next step"
                && key != "imported from"
                && !key.contains('/')
                && !value.is_empty()
                && value.chars().count() <= 120
                && !value.starts_with("//")
            {
                out.0.entry(key).or_default().insert(value);
                continue;
            }
        }
        if conditional.is_match(&norm(line)) {
            continue;
        }
        let clauses = line
            .nfkc()
            .collect::<String>()
            .to_lowercase()
            .replace('’', "'");
        for clause in split.split(&clauses) {
            let clause = clause
                .trim_matches([' ', '\t', '-', '*', '+'])
                .trim_end_matches('.');
            for (re, sign) in [(&neg, "neg"), (&pos, "pos")] {
                if let Some(c) = re.captures(clause) {
                    let (key, qualifier) = subject(&c[1]);
                    if key.chars().count() >= 2
                        && !"the a an our this that these those any your my it them"
                            .split_whitespace()
                            .any(|s| s == key)
                    {
                        out.1
                            .entry((key, qualifier))
                            .or_default()
                            .insert(sign.into());
                    }
                    break;
                }
            }
        }
    }
    out
}
pub(crate) fn contradictions(s: &Store, note: &str, paths: &str, source: &Path) -> Result<String> {
    if std::env::var("OMS_AGENT_MEMORY_CONTRADICTION_WARN").as_deref() == Ok("0") {
        return Ok(String::new());
    }
    let new = statements(note);
    if new.0.is_empty() && new.1.is_empty() {
        return Ok(String::new());
    }
    let (n, p) = s.logs()?;
    let state = records::states(&n, &p);
    let all: Vec<_> = n.iter().chain(&p).collect();
    let mut found = Vec::new();
    for e in all.iter().skip(all.len().saturating_sub(500)) {
        if state.contains_key(&e.event_id)
            || !["note", "imported", "pin"].contains(&e.kind.as_str())
            || e.body.trim().is_empty()
            || e.field("paths") != paths
            || e.source_path() != source.as_os_str()
        {
            continue;
        }
        let old = statements(&e.body);
        let mut reasons = Vec::new();
        for (key, values) in &new.0 {
            if let Some(before) = old.0.get(key) {
                if before.is_disjoint(values) {
                    reasons.push(format!(
                        "key \"{key}\" was \"{}\"",
                        clip(before.first().unwrap(), 60)
                    ));
                }
            }
        }
        for (key, signs) in &new.1 {
            if let Some(before) = old.1.get(key) {
                if signs.len() == 1 && before.len() == 1 && signs != before {
                    reasons.push(format!(
                        "\"{}\" was {}",
                        key.0,
                        if before.contains("neg") {
                            "forbidden"
                        } else {
                            "advised"
                        }
                    ));
                }
            }
        }
        if !reasons.is_empty() {
            let path = if e.source == "shared" {
                &s.shared
            } else {
                &s.pins
            };
            let raw = store::read(path)?;
            let line = raw[..e.start].iter().filter(|b| **b == b'\n').count() + 1;
            found.push((e, reasons, line));
        }
    }
    let mut out = String::new();
    for (e, reasons, line) in found.iter().rev().take(3) {
        out.push_str(&format!("memory: warning: this note may contradict {} ({}, {}.md line {}): {}{}\nmemory: if it replaces that note, run: oms agent-memory supersede --id {} --text TEXT\n",e.event_id,e.occurred_at,e.source,line,reasons[0],if reasons.len()>1{format!(" (+{} more)",reasons.len()-1)}else{String::new()},e.event_id));
    }
    if found.len() > 3 {
        out.push_str(&format!(
            "memory: warning: {} more older notes may contradict it\n",
            found.len() - 3
        ));
    }
    Ok(out)
}
