#![forbid(unsafe_code)]

use serde_json::Value;
use std::fmt::{self, Write};
use unicode_general_category::{GeneralCategory, get_general_category};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(&'static str);
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Error {}

fn field<'a>(v: &'a Value, key: &str) -> Result<&'a Value, Error> {
    v.get(key).ok_or(Error("invalid context report projection"))
}
fn string(v: &Value) -> Result<&str, Error> {
    v.as_str().ok_or(Error("invalid context report projection"))
}
fn number(v: &Value) -> Result<i64, Error> {
    v.as_i64().ok_or(Error("invalid context report projection"))
}
fn list(v: &Value) -> Result<&Vec<Value>, Error> {
    v.as_array()
        .ok_or(Error("invalid context report projection"))
}
fn text(v: &Value, k: &str) -> Result<String, Error> {
    Ok(label(string(field(v, k)?)?))
}
fn num(v: &Value, k: &str) -> Result<i64, Error> {
    number(field(v, k)?)
}
fn label(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i == 160 {
            out.push_str("... [label shortened]");
            break;
        }
        // Python 3.12's str.isspace includes these four information separators.
        if c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}') {
            out.push(' ');
        } else if matches!(
            get_general_category(c),
            GeneralCategory::Control
                | GeneralCategory::Format
                | GeneralCategory::Surrogate
                | GeneralCategory::PrivateUse
                | GeneralCategory::Unassigned
        ) {
            out.push('?');
        } else {
            out.push(c);
        }
    }
    if out.is_empty() {
        out.push_str("(empty)");
    }
    out
}
fn section(lines: &mut Vec<String>, title: &str, group: &Value, indent: &str) -> Result<(), Error> {
    lines.push(format!(
        "{indent}{title}: {} total; {} shown; {} not shown",
        num(group, "total")?,
        list(field(group, "items")?)?.len(),
        num(group, "omitted")?
    ));
    Ok(())
}
fn paths(lines: &mut Vec<String>, title: &str, group: &Value, indent: &str) -> Result<(), Error> {
    section(lines, title, group, indent)?;
    for item in list(field(group, "items")?)? {
        lines.push(format!("{indent}  - {}", label(string(item)?)));
    }
    Ok(())
}
/// Render a collector's bounded projection without reading files or adding a final newline.
pub fn render_report(report: &Value) -> Result<String, Error> {
    if report.get("schema").and_then(Value::as_i64) != Some(1) {
        return Err(Error("unsupported context report schema"));
    }
    let mut lines = match report.get("kind").and_then(Value::as_str) {
        Some("oms-context-inspection") => inspection(report)?,
        Some("oms-context-comparison") => comparison(report)?,
        _ => return Err(Error("unsupported context report kind")),
    };
    lines.extend(
        [
            "",
            "Read-only view; no bundle verification or acceptance judgment.",
            "No token, quota or billing savings are inferred.",
            "Long labels are marked; use JSON for full metadata. Metadata may be private.",
        ]
        .map(str::to_owned),
    );
    Ok(lines.join("\n"))
}
fn inspection(r: &Value) -> Result<Vec<String>, Error> {
    let d = field(r, "debt")?;
    let b = field(r, "budget")?;
    let sufficient = field(d, "reported_sufficient")?
        .as_bool()
        .ok_or(Error("invalid context report projection"))?;
    let mut l = vec![
        "OMS context inspection (recorded metadata)".into(),
        format!(
            "Phase: {}; recorded at: {}",
            text(r, "phase")?,
            text(r, "generated_at")?
        ),
        String::new(),
        format!("Required context debt (recorded): {}", num(d, "reported")?),
        format!(
            "Recorded sufficient: {}",
            if sufficient { "yes" } else { "no" }
        ),
    ];
    paths(
        &mut l,
        "Missing or incomplete required sources",
        field(d, "missing_required")?,
        "",
    )?;
    paths(
        &mut l,
        "Truncated required sources (subset above)",
        field(d, "truncated_required")?,
        "",
    )?;
    l.push(String::new());
    let c = field(r, "sources_current")?;
    if c.is_null() {
        l.push(
            "Current sources: not checked (use --verify-current to read selected sources)".into(),
        );
    } else {
        let (u, ch, m, un) = (
            num(c, "unchanged")?,
            num(c, "changed")?,
            num(c, "missing")?,
            num(c, "unreadable")?,
        );
        let checked = u
            .checked_add(ch)
            .and_then(|n| n.checked_add(m))
            .and_then(|n| n.checked_add(un))
            .ok_or(Error("invalid context report projection"))?;
        l.push(format!("Current sources (read-time): {checked} selected; {u} unchanged; {ch} changed; {m} missing; {un} unreadable"));
        paths(
            &mut l,
            "Changed source paths",
            field(c, "changed_paths")?,
            "",
        )?;
        paths(
            &mut l,
            "Missing source paths",
            field(c, "missing_paths")?,
            "",
        )?;
        l.push("Read-time hashes are not an atomic snapshot or a validation of the bundle.".into());
    }
    l.extend([
        String::new(),
        "Recorded byte budget (not tokens)".into(),
        format!(
            "  Used {} B / {} B; remaining {} B",
            num(b, "used_bytes")?,
            num(b, "bytes")?,
            num(b, "remaining_bytes")?
        ),
        format!(
            "  Source payload {} B; framing {} B",
            num(b, "source_payload_bytes")?,
            num(b, "framing_bytes")?
        ),
        String::new(),
    ]);
    paths(&mut l, "Targets", field(r, "targets")?, "")?;
    let s = field(r, "sources")?;
    section(
        &mut l,
        "Selected sources (largest recorded payload first)",
        s,
        "",
    )?;
    for row in list(field(s, "items")?)? {
        l.extend([
            format!("  - {}", text(row, "path")?),
            format!(
                "    {} B selected / {} B original; {}; {} B omitted",
                num(row, "bytes_selected")?,
                num(row, "bytes_before")?,
                string(field(row, "selection")?)?,
                num(row, "omitted_bytes")?
            ),
            format!("    Reason: {}", text(row, "reason")?),
            format!("    Source SHA-256: {}", string(field(row, "sha256")?)?),
        ]);
    }
    l.extend([
        "Selected payload can include the compiler's truncation marker.".into(),
        String::new(),
    ]);
    let o = field(r, "omissions")?;
    section(&mut l, "Omitted sources", o, "")?;
    for row in list(field(o, "items")?)? {
        let size = field(row, "bytes")?;
        let size = if size.is_null() {
            "unreported".into()
        } else {
            format!("{} B", number(size)?)
        };
        l.extend([
            format!("  - {} (size {size})", text(row, "path")?),
            format!("    Reason: {}", text(row, "reason")?),
        ]);
    }
    l.push(String::new());
    let g = field(r, "repeated_source_digests")?;
    section(
        &mut l,
        "Repeated full-source hashes (candidates only)",
        g,
        "",
    )?;
    for row in list(field(g, "items")?)? {
        l.push(format!("  SHA-256: {}", string(field(row, "sha256")?)?));
        paths(&mut l, "Paths", field(row, "paths")?, "  ")?;
    }
    let digest = field(r, "contract_state_digest")?;
    let digest = if digest.is_null() || digest.as_str() == Some("") {
        "unavailable"
    } else {
        string(digest)?
    };
    l.extend([
        "Same source hash does not prove the selected slices duplicate one another.".into(),
        String::new(),
        format!(
            "Recorded bundle SHA-256: {}",
            string(field(r, "bundle_sha256")?)?
        ),
        format!("Recorded contract digest: {digest}"),
        "This is an OMS context pack, not the provider's full context.".into(),
    ]);
    Ok(l)
}
fn comparison(r: &Value) -> Result<Vec<String>, Error> {
    let mut l = vec![
        "OMS context comparison (recorded metadata; right minus left)".into(),
        String::new(),
    ];
    if field(r, "recorded_scope_matches")?
        .as_bool()
        .ok_or(Error("invalid context report projection"))?
    {
        l.push("Recorded scope: matches; current task/repository identity is not verified.".into());
    } else {
        l.push("Recorded scope: different or unknown; do not infer a winning run.".into());
        for reason in list(field(r, "scope_mismatch_reasons")?)? {
            let reason = string(reason)?;
            let reason = match reason {
                "different-phase" => "different workflow phases",
                "different-targets" => "different target sets",
                "contract-identity-unavailable" => "contract identity unavailable",
                "different-contract-state" => "different recorded contract states",
                _ => reason,
            };
            l.push(format!("  - {}", label(reason)));
        }
    }
    let d = field(r, "delta")?;
    l.extend([String::new(), "Recorded deltas (right - left)".into()]);
    for (key, title) in [
        ("budget_bytes", "Budget"),
        ("selected_bytes", "Used"),
        ("remaining_bytes", "Remaining"),
    ] {
        l.push(format!("  {title}: {:+} B", num(d, key)?));
    }
    l.extend([
        format!("  Required context debt: {:+}", num(d, "context_debt")?),
        String::new(),
    ]);
    paths(&mut l, "Added selected sources", field(r, "added")?, "")?;
    paths(&mut l, "Removed selected sources", field(r, "removed")?, "")?;
    let c = field(r, "changed")?;
    section(&mut l, "Changed selected-source metadata", c, "")?;
    for row in list(field(c, "items")?)? {
        let mut fields = String::new();
        for (i, v) in list(field(row, "changed_fields")?)?.iter().enumerate() {
            if i > 0 {
                fields.push_str(", ");
            }
            write!(fields, "{}", label(string(v)?)).unwrap();
        }
        l.extend([
            format!("  - {}", text(row, "path")?),
            format!("    Fields: {fields}"),
        ]);
    }
    l.push("Zero deltas do not prove identical packs, quality or current validity.".into());
    Ok(l)
}
