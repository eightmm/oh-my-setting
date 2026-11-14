//! Notebook input is data. Outputs are never passed to the Python helper or
//! used as graph evidence; source positions refer to the notebook JSON file.
use oms_graph_python::Parser;
use oms_graph_shell::ParseResult;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Default)]
struct Cell {
    kind: String,
    source: Vec<(usize, String)>,
}
struct Frame {
    bracket: u8,
    key: Option<String>,
    index: usize,
}
fn fragments(text: &str) -> Option<BTreeMap<usize, Cell>> {
    let bytes = text.as_bytes();
    let mut offset = 0;
    let mut line = 1;
    let mut frames: Vec<Frame> = vec![];
    let mut cells: BTreeMap<usize, Cell> = BTreeMap::new();
    while offset < bytes.len() {
        let b = bytes[offset];
        match b {
            b'{' | b'[' => frames.push(Frame {
                bracket: b,
                key: None,
                index: 0,
            }),
            b'}' | b']' => {
                frames.pop()?;
            }
            b',' => {
                if let Some(f) = frames.last_mut() {
                    if f.bracket == b'[' {
                        f.index += 1;
                    } else {
                        f.key = None;
                    }
                }
            }
            b'"' => {
                let start = offset;
                offset += 1;
                while offset < bytes.len() {
                    if bytes[offset] == b'\\' {
                        offset += 2;
                    } else if bytes[offset] == b'"' {
                        break;
                    } else {
                        offset += 1;
                    }
                }
                let value: String = serde_json::from_str(text.get(start..=offset)?).ok()?;
                if frames
                    .last()
                    .is_some_and(|f| f.bracket == b'{' && f.key.is_none())
                {
                    frames.last_mut()?.key = Some(value);
                } else if frames.len() >= 3
                    && frames[0].bracket == b'{'
                    && frames[0].key.as_deref() == Some("cells")
                    && frames[1].bracket == b'['
                    && frames[2].bracket == b'{'
                {
                    let cell = cells.entry(frames[1].index).or_default();
                    let key = frames[2].key.as_deref();
                    if frames.len() == 3 && key == Some("cell_type") {
                        cell.kind = value;
                    } else if key == Some("source")
                        && (frames.len() == 3 || (frames.len() == 4 && frames[3].bracket == b'['))
                    {
                        cell.source.push((line, value));
                    }
                }
            }
            b'\n' => line += 1,
            _ => {}
        }
        offset += 1;
    }
    Some(cells)
}
fn logical(fragments: &[(usize, String)]) -> Vec<(String, usize)> {
    let mut result = vec![];
    let mut current = String::new();
    let mut start = None;
    for (physical, fragment) in fragments {
        let normalized = fragment.replace("\r\n", "\n").replace('\r', "\n");
        for (i, part) in normalized.split('\n').enumerate() {
            if i > 0 {
                result.push((std::mem::take(&mut current), start.unwrap_or(*physical)));
                start = None;
            }
            if !part.is_empty() {
                current.push_str(part);
                start.get_or_insert(*physical);
            }
        }
    }
    if !current.is_empty() {
        result.push((current, start.unwrap_or(1)));
    }
    result
}
fn valid<P: Parser + ?Sized>(
    path: &str,
    text: &str,
    digest: &str,
    python: &mut P,
) -> Result<bool, oms_graph_python::Error> {
    Ok(python.parse(path, text, digest)?.syntax_valid)
}
#[derive(Default)]
struct Continuation {
    quote: Option<u8>,
    triple: bool,
    brackets: usize,
    explicit: bool,
}
impl Continuation {
    fn open(&self) -> bool {
        self.quote.is_some() || self.brackets > 0 || self.explicit
    }
    fn line(&mut self, line: &str) {
        self.explicit = false;
        let bytes = line.as_bytes();
        let mut i = 0;
        let mut escaped = false;
        while i < bytes.len() {
            let b = bytes[i];
            if let Some(quote) = self.quote {
                if escaped {
                    escaped = false;
                    i += 1;
                    continue;
                }
                if b == b'\\' {
                    if i + 1 == bytes.len() {
                        escaped = true;
                    } else {
                        i += 1;
                    }
                    i += 1;
                    continue;
                }
                if b == quote
                    && (!self.triple
                        || bytes
                            .get(i..i + 3)
                            .is_some_and(|s| s.iter().all(|b| *b == quote)))
                {
                    i += if self.triple { 3 } else { 1 };
                    self.quote = None;
                    self.triple = false;
                    continue;
                }
            } else {
                match b {
                    b'#' => break,
                    b'\'' | b'"' => {
                        self.quote = Some(b);
                        self.triple = bytes
                            .get(i..i + 3)
                            .is_some_and(|s| s.iter().all(|v| *v == b));
                        if self.triple {
                            i += 2;
                        }
                    }
                    b'(' | b'[' | b'{' => self.brackets += 1,
                    b')' | b']' | b'}' => self.brackets = self.brackets.saturating_sub(1),
                    b'\\' if i + 1 == bytes.len() => self.explicit = true,
                    _ => {}
                }
            }
            i += 1;
        }
        if self.quote.is_some() && !self.triple && !escaped {
            self.quote = None;
        }
    }
}
fn python_lines<P: Parser + ?Sized>(
    path: &str,
    lines: &[String],
    digest: &str,
    python: &mut P,
) -> Result<Option<Vec<String>>, oms_graph_python::Error> {
    if valid(path, &lines.join("\n"), digest, python)? {
        return Ok(Some(lines.to_vec()));
    }
    let first = lines
        .iter()
        .map(|s| s.trim_start())
        .find(|s| !s.is_empty() && !s.starts_with('#'))
        .unwrap_or("");
    if first.starts_with("%%") {
        return Ok(None);
    }
    let mut state = Continuation::default();
    let mut filtered = vec![];
    for line in lines {
        let next = if line.trim_start().starts_with(['%', '!']) && !state.open() {
            String::new()
        } else {
            line.clone()
        };
        state.line(&next);
        filtered.push(next);
    }
    if filtered != lines && valid(path, &filtered.join("\n"), digest, python)? {
        Ok(Some(filtered))
    } else {
        Ok(None)
    }
}

/// Reuse the persistent isolated parser capability, including for empty input.
pub fn parse<P: Parser + ?Sized>(
    path: &str,
    text: &str,
    digest: &str,
    python: &mut P,
) -> Result<ParseResult, oms_graph_python::Error> {
    fn convert(value: oms_graph_python::ParseResult) -> ParseResult {
        let v = serde_json::to_value(value).expect("typed parser result");
        ParseResult {
            nodes: v["nodes"].as_array().unwrap().clone(),
            edges: v["edges"].as_array().unwrap().clone(),
            refs: v["refs"].as_array().unwrap().clone(),
        }
    }
    let mut empty = convert(python.parse(path, "", digest)?);
    for node in &mut empty.nodes {
        if node["language"] == "python" {
            node["language"] = json!("notebook");
        }
    }
    if let Some(module) = empty.nodes.first_mut() {
        module["metadata"]["code_cells"] = json!(0);
        module["metadata"]["skipped_cells"] = json!(0);
    }
    let document: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Ok(empty),
    };
    let Some(cells) = document["cells"].as_array() else {
        return Ok(empty);
    };
    let count = cells.iter().filter(|c| c["cell_type"] == "code").count();
    if let Some(module) = empty.nodes.first_mut() {
        module["metadata"]["code_cells"] = json!(count);
        module["metadata"]["skipped_cells"] = json!(count);
    }
    let kernel = document["metadata"]["language_info"]["name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| document["metadata"]["kernelspec"]["language"].as_str())
        .unwrap_or("")
        .to_lowercase();
    if !kernel.is_empty() && !kernel.starts_with("python") && !kernel.starts_with("ipython") {
        return Ok(empty);
    }
    let Some(found) = fragments(text) else {
        return Ok(empty);
    };
    let mut lines = vec![];
    let mut physical = vec![];
    let mut owners = vec![];
    let mut parsed = 0;
    for (index, cell) in cells
        .iter()
        .enumerate()
        .filter(|(_, c)| c["cell_type"] == "code")
    {
        let expected = match &cell["source"] {
            Value::String(s) => s.clone(),
            Value::Array(a) if a.iter().all(Value::is_string) => {
                a.iter().filter_map(Value::as_str).collect::<String>()
            }
            Value::Null => String::new(),
            _ => continue,
        };
        let Some(fragments) = found.get(&index) else {
            continue;
        };
        if fragments.kind != "code"
            || fragments
                .source
                .iter()
                .map(|(_, s)| s.as_str())
                .collect::<String>()
                != expected
        {
            continue;
        }
        let logical = logical(&fragments.source);
        let Some(usable) = python_lines(
            path,
            &logical.iter().map(|(s, _)| s.clone()).collect::<Vec<_>>(),
            digest,
            python,
        )?
        else {
            continue;
        };
        parsed += 1;
        for (line, (_, position)) in usable.into_iter().zip(logical) {
            lines.push(line);
            physical.push(position);
            owners.push(index + 1);
        }
    }
    let joined = lines.join("\n");
    let parsed_source = python.parse(path, &joined, digest)?;
    if !parsed_source.syntax_valid {
        return Ok(empty);
    }
    let mut result = convert(parsed_source);
    let remap = |value: &Value| -> Value {
        json!(
            value
                .as_u64()
                .and_then(|p| p.checked_sub(1))
                .and_then(|p| physical.get(p as usize))
                .copied()
                .unwrap_or(1)
        )
    };
    for node in &mut result.nodes {
        if node["language"] == "python" {
            node["language"] = json!("notebook");
        }
        if node["kind"] == "module" {
            node["metadata"]["code_cells"] = json!(count);
            node["metadata"]["skipped_cells"] = json!(count - parsed);
        }
        if matches!(node["kind"].as_str(), Some("class" | "function" | "method")) {
            let details = &mut node["metadata"];
            if let Some(owner) = details["line"]
                .as_u64()
                .and_then(|p| p.checked_sub(1))
                .and_then(|p| owners.get(p as usize))
            {
                details["cell"] = json!(owner);
            }
            for key in ["line", "end_line"] {
                if details.get(key).is_some() {
                    details[key] = remap(&details[key]);
                }
            }
        }
    }
    for edge in &mut result.edges {
        if edge["evidence"].get("line").is_some() {
            edge["evidence"]["line"] = remap(&edge["evidence"]["line"]);
        }
    }
    for reference in &mut result.refs {
        if reference.get("line").is_some() {
            reference["line"] = remap(&reference["line"]);
        }
    }
    Ok(result)
}
