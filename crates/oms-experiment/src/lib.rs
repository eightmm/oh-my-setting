//! Pure ExperimentContract v2 normalization.
#![forbid(unsafe_code)]
use oms_context::Json;
use oms_json::{PyString, Value, canonical_json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(String),
    /// A contract error containing WTF-8 code points that cannot be encoded as UTF-8.
    InvalidWtf8(Vec<u8>),
    MalformedNumber,
    Value(String),
    Type(String),
    Overflow(String),
    Encoding(oms_json::Error),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(s) | Self::Value(s) | Self::Type(s) | Self::Overflow(s) => f.write_str(s),
            Self::MalformedNumber => f.write_str("invalid JSON number"),
            Self::InvalidWtf8(_) => f.write_str("contract error contains an unencodable surrogate"),
            Self::Encoding(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for Error {}
fn invalid<T>(s: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(s.into()))
}
fn invalid_key<T>(prefix: &str, key: &[u8]) -> Result<T, Error> {
    let mut message = prefix.as_bytes().to_vec();
    message.extend_from_slice(key);
    match String::from_utf8(message) {
        Ok(message) => Err(Error::Invalid(message)),
        Err(error) => Err(Error::InvalidWtf8(error.into_bytes())),
    }
}
fn obj(v: &Json) -> Option<&Vec<(Vec<u8>, Json)>> {
    if let Json::Object(o) = v {
        Some(o)
    } else {
        None
    }
}
fn get<'a>(o: &'a [(Vec<u8>, Json)], k: &str) -> Option<&'a Json> {
    o.iter()
        .rev()
        .find(|(key, _)| key == k.as_bytes())
        .map(|(_, v)| v)
}
fn put(o: &mut Vec<(Vec<u8>, Json)>, k: &str, v: Json) {
    if let Some((_, old)) = o.iter_mut().find(|(key, _)| key == k.as_bytes()) {
        *old = v;
    } else {
        o.push((k.as_bytes().to_vec(), v));
    }
}
fn remove(o: &mut Vec<(Vec<u8>, Json)>, k: &str) {
    o.retain(|(key, _)| key != k.as_bytes());
}
fn copy(root: &Json) -> Json {
    enum Frame<'a> {
        Node(&'a Json),
        Array(usize),
        Object(Vec<Vec<u8>>),
    }
    let mut frames = vec![Frame::Node(root)];
    let mut values = Vec::new();
    while let Some(frame) = frames.pop() {
        match frame {
            Frame::Node(node) => match node {
                Json::Null => values.push(Json::Null),
                Json::Bool(b) => values.push(Json::Bool(*b)),
                Json::Number(n) => {
                    values.push(Json::Number(if n == "-0" { "0".into() } else { n.clone() }))
                }
                Json::String(s) => values.push(Json::String(s.clone())),
                Json::Array(items) => {
                    frames.push(Frame::Array(items.len()));
                    frames.extend(items.iter().rev().map(Frame::Node));
                }
                Json::Object(items) => {
                    frames.push(Frame::Object(
                        items.iter().map(|(k, _)| k.clone()).collect(),
                    ));
                    frames.extend(items.iter().rev().map(|(_, v)| Frame::Node(v)));
                }
            },
            Frame::Array(len) => {
                let items = values.split_off(values.len() - len);
                values.push(Json::Array(items));
            }
            Frame::Object(keys) => {
                let items = values.split_off(values.len() - keys.len());
                let mut fields: Vec<(Vec<u8>, Json)> = Vec::new();
                let mut positions: HashMap<Vec<u8>, usize> = HashMap::new();
                for (key, item) in keys.into_iter().zip(items) {
                    if let Some(&pos) = positions.get(&key) {
                        fields[pos].1 = item;
                    } else {
                        positions.insert(key.clone(), fields.len());
                        fields.push((key, item));
                    }
                }
                values.push(Json::Object(fields));
            }
        }
    }
    values.pop().expect("one root")
}

fn string(s: &str) -> Json {
    Json::String(s.as_bytes().to_vec())
}
fn object(items: Vec<(&str, Json)>) -> Json {
    Json::Object(
        items
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}
fn number(n: f64) -> Json {
    Json::Number(format_float(n))
}
fn format_float(n: f64) -> String {
    String::from_utf8(canonical_json(&Value::Float(n)).expect("finite float")).expect("UTF-8")
}
fn integer(n: &str) -> bool {
    !n.contains(['.', 'e', 'E']) && n != "NaN" && n != "Infinity" && n != "-Infinity"
}
fn float(v: &Json) -> Result<Option<f64>, Error> {
    if let Json::Number(n) = v {
        let x = n.parse::<f64>().unwrap_or(f64::NAN);
        if integer(n) && x.is_infinite() {
            return Err(Error::Overflow("int too large to convert to float".into()));
        }
        Ok(Some(x))
    } else {
        Ok(None)
    }
}
fn valid_wtf8(mut bytes: &[u8]) -> bool {
    loop {
        match std::str::from_utf8(bytes) {
            Ok(_) => return true,
            Err(e) => {
                bytes = &bytes[e.valid_up_to()..];
                if bytes.len() < 3
                    || bytes[0] != 0xed
                    || !(0xa0..=0xbf).contains(&bytes[1])
                    || !(0x80..=0xbf).contains(&bytes[2])
                {
                    return false;
                }
                bytes = &bytes[3..];
            }
        }
    }
}
fn valid_number(n: &str) -> bool {
    if matches!(n, "NaN" | "Infinity" | "-Infinity") {
        return true;
    }
    let b = n.as_bytes();
    let mut i = usize::from(b.first() == Some(&b'-'));
    if i == b.len() {
        return false;
    }
    if b[i] == b'0' {
        i += 1;
    } else if (b'1'..=b'9').contains(&b[i]) {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    } else {
        return false;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    if i < b.len() && matches!(b[i], b'e' | b'E') {
        i += 1;
        if i < b.len() && matches!(b[i], b'+' | b'-') {
            i += 1;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    i == b.len()
}
fn check_strings(raw: &Json) -> Result<(), Error> {
    let mut pending = vec![raw];
    while let Some(node) = pending.pop() {
        match node {
            Json::Number(n) if !valid_number(n) => return Err(Error::MalformedNumber),
            Json::String(s) if !valid_wtf8(s) => {
                return Err(Error::Encoding(oms_json::Error::InvalidCodepoint));
            }
            Json::Array(items) => pending.extend(items),
            Json::Object(items) => {
                for (key, value) in items {
                    if !valid_wtf8(key) {
                        return Err(Error::Encoding(oms_json::Error::InvalidCodepoint));
                    }
                    pending.push(value);
                }
            }
            _ => {}
        }
    }
    Ok(())
}
fn points(bytes: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let (n, len) = if b < 128 {
            (b as u32, 1)
        } else if b < 224 {
            (((b & 31) as u32) << 6 | (bytes[i + 1] & 63) as u32, 2)
        } else if b < 240 {
            (
                ((b & 15) as u32) << 12
                    | (((bytes[i + 1] & 63) as u32) << 6)
                    | (bytes[i + 2] & 63) as u32,
                3,
            )
        } else {
            (
                ((b & 7) as u32) << 18
                    | (((bytes[i + 1] & 63) as u32) << 12)
                    | (((bytes[i + 2] & 63) as u32) << 6)
                    | (bytes[i + 3] & 63) as u32,
                4,
            )
        };
        out.push(n);
        i += len;
    }
    out
}
fn pystr(bytes: &[u8]) -> PyString {
    PyString::from_codepoints(points(bytes)).expect("valid WTF-8")
}
fn value(root: &Json) -> Result<Value, Error> {
    enum Frame<'a> {
        Node(&'a Json),
        Array(usize),
        Object(Vec<PyString>),
    }
    let mut frames = vec![Frame::Node(root)];
    let mut values = Vec::new();
    while let Some(frame) = frames.pop() {
        match frame {
            Frame::Node(node) => match node {
                Json::Null => values.push(Value::Null),
                Json::Bool(b) => values.push(Value::Bool(*b)),
                Json::Number(n) if integer(n) => values.push(
                    Value::integer(if n == "-0" { "0" } else { n }).map_err(Error::Encoding)?,
                ),
                Json::Number(n) => values.push(Value::Float(n.parse().unwrap_or(f64::NAN))),
                Json::String(s) => values.push(Value::String(pystr(s))),
                Json::Array(items) => {
                    frames.push(Frame::Array(items.len()));
                    frames.extend(items.iter().rev().map(Frame::Node));
                }
                Json::Object(items) => {
                    frames.push(Frame::Object(items.iter().map(|(k, _)| pystr(k)).collect()));
                    frames.extend(items.iter().rev().map(|(_, v)| Frame::Node(v)));
                }
            },
            Frame::Array(len) => {
                let items = values.split_off(values.len() - len);
                values.push(Value::Array(items));
            }
            Frame::Object(keys) => {
                let items = values.split_off(values.len() - keys.len());
                let map: BTreeMap<_, _> = keys.into_iter().zip(items).collect();
                values.push(Value::Object(map));
            }
        }
    }
    Ok(values.pop().expect("one root"))
}

fn canonical(j: &Json) -> Result<Vec<u8>, Error> {
    check_strings(j)?;
    canonical_json(&value(j)?).map_err(Error::Encoding)
}
fn ws(p: u32) -> bool {
    matches!(p, 9..=13 | 0x1c..=0x20 | 0x85 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000)
}
fn stripped(s: &[u8]) -> Vec<u8> {
    let p = points(s);
    let start = p.iter().position(|&c| !ws(c)).unwrap_or(p.len());
    let end = p.iter().rposition(|&c| !ws(c)).map_or(start, |i| i + 1);
    let mut out = Vec::new();
    for &c in &p[start..end] {
        if (0xd800..=0xdfff).contains(&c) {
            out.extend_from_slice(&[
                0xe0 | (c >> 12) as u8,
                0x80 | ((c >> 6) & 63) as u8,
                0x80 | (c & 63) as u8,
            ]);
        } else {
            let mut buf = [0; 4];
            out.extend_from_slice(char::from_u32(c).unwrap().encode_utf8(&mut buf).as_bytes());
        }
    }
    out
}
fn required(o: &[(Vec<u8>, Json)], key: &str, limit: usize) -> Result<Vec<u8>, Error> {
    if let Some(Json::String(s)) = get(o, key)
        && !stripped(s).is_empty()
        && points(s).len() <= limit
        && !s.iter().any(|c| matches!(c, b'\r' | b'\n' | b'\t'))
    {
        return Ok(stripped(s));
    }
    invalid(format!(
        "experiment {key} must be one line of 1-{limit} characters"
    ))
}
fn safe(s: &[u8], max: usize, metric: bool) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s[0].is_ascii_alphanumeric()
        && s.iter().all(|b| {
            b.is_ascii_alphanumeric()
                || if metric {
                    b"._/:+-".contains(b)
                } else {
                    b"._-".contains(b)
                }
        })
}
fn text(v: &Json) -> String {
    match v {
        Json::String(s) => String::from_utf8_lossy(s).into_owned(),
        Json::Null => "None".into(),
        Json::Bool(b) => if *b { "True" } else { "False" }.into(),
        Json::Number(n) => n.clone(),
        Json::Array(_) => "[]".into(),
        Json::Object(_) => "{}".into(),
    }
}
fn metric(v: &Json, label: &str, observe: bool) -> Result<(Vec<u8>, String, Json), Error> {
    let (name, direction) = match v {
        Json::String(s) => (stripped(s), "observe".into()),
        Json::Object(o) => (
            required(o, "name", 160)?,
            get(o, "direction").map_or("observe".into(), text),
        ),
        _ => return invalid(format!("{label} must be a metric name or object")),
    };
    if !safe(&name, 160, true) {
        return invalid(format!("{label}.name must be a safe metric identifier"));
    }
    if direction != "minimize" && direction != "maximize" && !(observe && direction == "observe") {
        return invalid(format!(
            "{label}.direction must be {}",
            if observe {
                "maximize or minimize or observe"
            } else {
                "maximize or minimize"
            }
        ));
    }
    let spec = object(vec![
        ("name", Json::String(name.clone())),
        ("direction", string(&direction)),
    ]);
    Ok((name, direction, spec))
}
/// Canonical template with schema version 2.
pub fn template() -> Json {
    oms_context::parse_json(br#"{"schema":2,"question":"","hypothesis":"","prediction":"","baseline":{"run_id":"","config_digest":""},"treatment":{"independent_change":"","config_digest":""},"controlled_variables":{"code_base":"","dataset_split":"","training_steps":null,"parameter_count_tolerance":0.01},"seeds":[0,1,2],"metrics":{"primary":{"name":"","direction":"minimize"},"secondary":[]},"success":{"min_improvement":0.0,"no_regression":{}},"invariant_pack":[]}"#).expect("template")
}
/// Normalized contract and canonical bytes of its digest source.
pub struct Report {
    pub normalized: Json,
    pub source_bytes: Vec<u8>,
    pub digest: String,
}
impl Report {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, Error> {
        canonical(&self.normalized)
    }
}
/// Validate and normalize a lossless JSON contract without I/O.
pub fn validate(raw: &Json) -> Result<Report, Error> {
    check_strings(raw)?;
    let normalized_input = copy(raw);
    let raw = &normalized_input;
    let o = obj(raw)
        .ok_or_else(|| Error::Invalid("experiment contract must be a JSON object".into()))?;
    if !matches!(get(o,"schema"),Some(Json::Number(n)) if n.parse::<f64>().ok()==Some(2.0)) {
        return invalid("experiment contract schema must be 2");
    }
    for key in ["question", "hypothesis", "prediction"] {
        required(o, key, 2000)?;
    }
    if !matches!(get(o,"baseline"),Some(Json::Object(x)) if !x.is_empty()) {
        return invalid("experiment baseline must be a nonempty object");
    }
    let treatment = match get(o, "treatment") {
        Some(Json::Object(x)) if !x.is_empty() => x,
        _ => return invalid("experiment treatment must be a nonempty object"),
    };
    if !matches!(get(treatment,"independent_change"),Some(Json::String(s)) if !stripped(s).is_empty())
    {
        return invalid("treatment.independent_change is required");
    }
    if !matches!(get(o, "controlled_variables"), Some(Json::Object(_))) {
        return invalid("experiment controlled_variables must be an object");
    }
    let metrics = match get(o, "metrics") {
        Some(Json::Object(x)) if matches!(get(x, "primary"), Some(Json::Object(_))) => x,
        _ => return invalid("experiment metrics.primary must be an object"),
    };
    let (primary_name, primary_dir, primary) =
        metric(get(metrics, "primary").unwrap(), "metrics.primary", false)?;
    let mut specs = vec![(primary_name.clone(), primary_dir, copy(&primary))];
    let mut secondary = Vec::new();
    if let Some(v) = get(metrics, "secondary") {
        let a = if let Json::Array(a) = v {
            a
        } else {
            return invalid("metrics.secondary must be a list");
        };
        for (i, item) in a.iter().enumerate() {
            let (name, dir, spec) = metric(item, &format!("metrics.secondary[{i}]"), true)?;
            if specs.iter().any(|(n, _, _)| *n == name) {
                return invalid(format!(
                    "metric is declared more than once: {}",
                    String::from_utf8_lossy(&name)
                ));
            }
            specs.push((name, dir, copy(&spec)));
            secondary.push(spec);
        }
    }
    let seeds = match get(o, "seeds") {
        Some(Json::Array(a)) if !a.is_empty() => a,
        _ => return invalid("experiment seeds must be a nonempty list"),
    };
    let mut seen = HashSet::new();
    for seed in seeds {
        if !matches!(seed, Json::Number(n) if integer(n)) {
            return invalid("experiment seeds must contain integers");
        }
    }
    for seed in seeds {
        if let Json::Number(n) = seed
            && !seen.insert(if n == "-0" { "0" } else { n.as_str() })
        {
            return invalid("experiment seeds must be unique");
        }
    }
    let success = match get(o, "success") {
        Some(Json::Object(x)) if !x.is_empty() => x,
        _ => return invalid("experiment success must be a nonempty object"),
    };
    let threshold = match get(success, "min_improvement") {
        None => 0.0,
        Some(v) => match float(v)? {
            Some(n) if n.is_finite() => n,
            _ => return invalid("success.min_improvement must be finite numeric"),
        },
    };
    let mut regress = Vec::new();
    if let Some(v) = get(success, "no_regression") {
        let a = if let Json::Object(a) = v {
            a
        } else {
            return invalid("success.no_regression must be an object");
        };
        for (name, v) in a {
            let dir = match specs.iter().find(|(n, _, _)| n == name) {
                Some((_, d, _)) => d,
                _ => return invalid_key("no-regression metric is not declared: ", name),
            };
            let label = std::str::from_utf8(name).expect("declared metric is ASCII");
            let (d, n) = match v {
                Json::Number(_) => {
                    if dir == "observe" {
                        return invalid(format!(
                            "no-regression metric needs minimize/maximize direction: {label}"
                        ));
                    }
                    (dir.clone(), float(v)?.unwrap())
                }
                Json::Object(fields) => {
                    let d = get(fields, "direction").map_or(dir.clone(), text);
                    if d != "minimize" && d != "maximize" {
                        return invalid(format!(
                            "no-regression direction must be minimize or maximize: {label}"
                        ));
                    }
                    let n = match get(fields, "max_regression") {
                        None => 0.0,
                        Some(v) => match float(v)? {
                            Some(n) => n,
                            None => {
                                return invalid(format!(
                                    "no-regression max_regression must be numeric: {label}"
                                ));
                            }
                        },
                    };
                    (d, n)
                }
                _ => {
                    return invalid(format!(
                        "no-regression threshold must be numeric or an object: {label}"
                    ));
                }
            };
            if !n.is_finite() || n < 0.0 {
                return invalid(format!(
                    "no-regression max_regression must be finite and non-negative: {label}"
                ));
            }
            regress.push((
                name.clone(),
                object(vec![
                    ("direction", string(&d)),
                    ("max_regression", number(n)),
                ]),
            ));
        }
    }
    let mut noise = None;
    if let Some(v) = get(success, "noise_band")
        && !matches!(v, Json::Null)
    {
        let fields = if let Json::Object(f) = v {
            f
        } else {
            return invalid("success.noise_band must be an object");
        };
        let mode = get(fields, "mode");
        if matches!(mode, Some(Json::Array(_))) {
            return Err(Error::Type("unhashable type: 'list'".into()));
        }
        if matches!(mode, Some(Json::Object(_))) {
            return Err(Error::Type("unhashable type: 'dict'".into()));
        }
        let key = match mode {
            Some(Json::String(s)) if s == b"fixed" => "delta",
            Some(Json::String(s)) if s == b"baseline_stdev" => "z",
            _ => return invalid("success.noise_band.mode must be fixed or baseline_stdev"),
        };
        let n = match get(fields, key) {
            Some(v) => match float(v)? {
                Some(n) if n.is_finite() && n >= 0.0 => n,
                _ => {
                    return invalid(format!(
                        "success.noise_band.{key} must be finite and non-negative"
                    ));
                }
            },
            None => {
                return invalid(format!(
                    "success.noise_band.{key} must be finite and non-negative"
                ));
            }
        };
        noise = Some(object(vec![
            ("mode", copy(mode.unwrap())),
            (key, number(n)),
        ]));
    }
    let mut invariants = Vec::new();
    let mut inv_seen = HashSet::new();
    if let Some(v) = get(o, "invariant_pack") {
        let a = if let Json::Array(a) = v {
            a
        } else {
            return invalid("invariant_pack must be a list");
        };
        for (i, item) in a.iter().enumerate() {
            let f = match item {
                Json::Object(f) => f,
                _ => {
                    return invalid(format!(
                        "invariant_pack[{i}] must have name and nonempty command array"
                    ));
                }
            };
            let (name, command) = match (get(f, "name"), get(f, "command")) {
                (Some(Json::String(n)), Some(Json::Array(c))) if !c.is_empty() => (n, c),
                _ => {
                    return invalid(format!(
                        "invariant_pack[{i}] must have name and nonempty command array"
                    ));
                }
            };
            if !safe(name, 80, false) {
                return invalid(format!(
                    "invariant_pack[{i}].name must be a safe identifier"
                ));
            }
            if !inv_seen.insert(name) {
                return invalid(format!(
                    "duplicate invariant name: {}",
                    String::from_utf8_lossy(name)
                ));
            }
            if command
                .iter()
                .any(|v| !matches!(v,Json::String(s) if !s.is_empty()))
            {
                return invalid(format!("invariant_pack[{i}].command must contain strings"));
            }
            let required = match get(f, "required") {
                None => true,
                Some(Json::Bool(b)) => *b,
                _ => return invalid(format!("invariant_pack[{i}].required must be boolean")),
            };
            invariants.push(object(vec![
                ("name", Json::String(name.clone())),
                ("command", Json::Array(command.iter().map(copy).collect())),
                ("required", Json::Bool(required)),
            ]));
        }
    }
    let mut budget = None;
    if let Some(v) = get(o, "budget")
        && !matches!(v, Json::Null)
    {
        let b = match v {
            Json::Object(b) if !b.is_empty() => b,
            _ => return invalid("budget must be a nonempty object"),
        };
        let mut unknown: Vec<_> = b
            .iter()
            .map(|(k, _)| k)
            .filter(|k| ![b"gpu_hours".as_slice(), b"runs", b"final_evals"].contains(&k.as_slice()))
            .collect();
        unknown.sort_by_key(|k| points(k));
        if let Some(k) = unknown.first() {
            return invalid_key("budget has unknown key: ", k);
        }
        let mut out = Vec::new();
        for key in ["gpu_hours", "runs", "final_evals"] {
            if let Some(v) = get(b, key) {
                let v = if key == "gpu_hours" {
                    match float(v)? {
                        Some(n) if n.is_finite() && n > 0.0 => number(n),
                        _ => return invalid("budget.gpu_hours must be a positive number"),
                    }
                } else {
                    match v {
                        Json::Number(n) if integer(n) && n != "0" && !n.starts_with('-') => copy(v),
                        _ => {
                            return invalid(format!("budget.{key} must be a positive integer"));
                        }
                    }
                };
                out.push((key.as_bytes().to_vec(), v));
            }
        }
        budget = Some(Json::Object(out));
    }
    let mut source: Vec<_> = o.iter().map(|(k, v)| (k.clone(), copy(v))).collect();
    for key in [
        "contract_digest",
        "experiment_id",
        "registered_at",
        "primary_metric",
        "metric_specs",
    ] {
        remove(&mut source, key);
    }
    put(
        &mut source,
        "seeds",
        Json::Array(seeds.iter().map(copy).collect()),
    );
    put(
        &mut source,
        "metrics",
        object(vec![
            ("primary", primary),
            ("secondary", Json::Array(secondary)),
        ]),
    );
    let mut s = success.iter().map(|(k, v)| (k.clone(), copy(v))).collect();
    put(&mut s, "min_improvement", number(threshold));
    put(&mut s, "no_regression", Json::Object(regress));
    if let Some(v) = noise {
        put(&mut s, "noise_band", v);
    }
    put(&mut source, "success", Json::Object(s));
    put(&mut source, "invariant_pack", Json::Array(invariants));
    if let Some(b) = budget {
        put(&mut source, "budget", b);
    } else {
        remove(&mut source, "budget");
    }
    let source_json = Json::Object(source);
    let source_bytes = canonical(&source_json)?;
    let digest = format!("{:x}", Sha256::digest(&source_bytes));
    let mut normalized: Vec<_> = obj(&source_json)
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), copy(v)))
        .collect();
    put(
        &mut normalized,
        "primary_metric",
        Json::String(primary_name),
    );
    put(
        &mut normalized,
        "metric_specs",
        Json::Object(specs.into_iter().map(|(n, _, s)| (n, s)).collect()),
    );
    put(&mut normalized, "contract_digest", string(&digest));
    Ok(Report {
        normalized: Json::Object(normalized),
        source_bytes,
        digest,
    })
}

mod compare;
pub use compare::compare;

/// Encode a typed JSON value with the shared canonical codec.
pub fn encode_json(input: &Json, pretty: bool) -> Result<Vec<u8>, Error> {
    check_strings(input)?;
    let v = value(input)?;
    if pretty {
        oms_json::pretty_json(&v).map_err(Error::Encoding)
    } else {
        canonical_json(&v).map_err(Error::Encoding)
    }
}
