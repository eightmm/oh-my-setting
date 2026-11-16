use super::*;
use unicode_general_category::{GeneralCategory, get_general_category};

fn numeric(v: &Json) -> Option<&str> {
    match v {
        Json::Number(n) => Some(n),
        Json::Bool(true) => Some("1"),
        Json::Bool(false) => Some("0"),
        _ => None,
    }
}

fn numeric_equal(a: &str, b: &str) -> bool {
    let ai = integer(a);
    let bi = integer(b);
    if ai && bi {
        return (if a == "-0" { "0" } else { a }) == (if b == "-0" { "0" } else { b });
    }
    if !ai && !bi {
        return a.parse::<f64>().ok() == b.parse::<f64>().ok();
    }
    let (int, f) = if ai { (a, b) } else { (b, a) };
    let Ok(f) = f.parse::<f64>() else {
        return false;
    };
    if !f.is_finite() || f.fract() != 0.0 {
        return false;
    }
    let int = if int == "-0" { "0" } else { int };
    if f == 0.0 {
        return int == "0";
    }
    format!("{f:.0}") == int
}

fn equal(a: &Json, b: &Json) -> bool {
    let mut pending = vec![(a, b)];
    while let Some((a, b)) = pending.pop() {
        if let (Some(a), Some(b)) = (numeric(a), numeric(b)) {
            if !numeric_equal(a, b) {
                return false;
            }
            continue;
        }
        match (a, b) {
            (Json::Null, Json::Null) => (),
            (Json::String(a), Json::String(b)) if a == b => (),
            (Json::Array(a), Json::Array(b)) if a.len() == b.len() => {
                pending.extend(a.iter().zip(b))
            }
            (Json::Object(a), Json::Object(b)) if a.len() == b.len() => {
                let map: HashMap<_, _> = b.iter().map(|(k, v)| (k, v)).collect();
                for (k, v) in a {
                    if let Some(w) = map.get(k) {
                        pending.push((v, *w));
                    } else {
                        return false;
                    }
                }
            }
            _ => return false,
        }
    }
    true
}

fn digit(p: u32) -> Option<char> {
    let c = char::from_u32(p)?;
    if get_general_category(c) != GeneralCategory::DecimalNumber {
        return None;
    }
    let mut start = p;
    while start > 0
        && char::from_u32(start - 1)
            .is_some_and(|c| get_general_category(c) == GeneralCategory::DecimalNumber)
    {
        start -= 1;
    }
    char::from_digit((p - start) % 10, 10)
}

fn python_repr(s: &[u8]) -> String {
    let chars = points(s);
    let quote = if chars.contains(&39) && !chars.contains(&34) {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for p in chars {
        match p {
            0x5c => out.push_str("\\\\"),
            10 => out.push_str("\\n"),
            13 => out.push_str("\\r"),
            9 => out.push_str("\\t"),
            p if p == quote as u32 => {
                out.push('\\');
                out.push(quote);
            }
            p if p < 32 || (0x7f..=0x9f).contains(&p) => out.push_str(&format!("\\x{p:02x}")),
            p if (0xd800..=0xdfff).contains(&p) => out.push_str(&format!("\\u{p:04x}")),
            p => {
                if let Some(c) = char::from_u32(p) {
                    let category = get_general_category(c);
                    if p != 32
                        && matches!(
                            category,
                            GeneralCategory::Control
                                | GeneralCategory::Format
                                | GeneralCategory::LineSeparator
                                | GeneralCategory::ParagraphSeparator
                                | GeneralCategory::PrivateUse
                                | GeneralCategory::SpaceSeparator
                                | GeneralCategory::Unassigned
                        )
                    {
                        if p <= 0xff {
                            out.push_str(&format!("\\x{p:02x}"));
                        } else if p <= 0xffff {
                            out.push_str(&format!("\\u{p:04x}"));
                        } else {
                            out.push_str(&format!("\\U{p:08x}"));
                        }
                    } else {
                        out.push(c);
                    }
                }
            }
        }
    }
    out.push(quote);
    out
}

fn convert(v: Option<&Json>) -> Result<f64, Error> {
    let Some(v) = v else { return Ok(0.0) };
    match v {
        Json::Null | Json::Bool(false) => Ok(0.0),
        Json::Bool(true) => Ok(1.0),
        Json::Number(_) => float(v).map(|v| v.unwrap()),
        Json::Array(a) if a.is_empty() => Ok(0.0),
        Json::Object(a) if a.is_empty() => Ok(0.0),
        Json::Array(_) => Err(Error::Type(
            "float() argument must be a string or a real number, not 'list'".into(),
        )),
        Json::Object(_) => Err(Error::Type(
            "float() argument must be a string or a real number, not 'dict'".into(),
        )),
        Json::String(s) if s.is_empty() => Ok(0.0),
        Json::String(s) => {
            let mut normalized = String::new();
            for p in points(&stripped(s)) {
                if let Some(d) = digit(p) {
                    normalized.push(d);
                } else if p < 128 {
                    normalized.push(p as u8 as char);
                } else {
                    normalized.push('\u{fffd}');
                }
            }
            let bytes = normalized.as_bytes();
            let underscores_valid = bytes.iter().enumerate().all(|(i, b)| {
                *b != b'_'
                    || (i > 0
                        && i + 1 < bytes.len()
                        && bytes[i - 1].is_ascii_digit()
                        && bytes[i + 1].is_ascii_digit())
            });
            let clean = normalized.replace('_', "");
            if underscores_valid && let Ok(n) = clean.parse::<f64>() {
                return Ok(n);
            }
            Err(Error::Value(format!(
                "could not convert string to float: {}",
                python_repr(s)
            )))
        }
    }
}

/// Compare two contracts after validating the left and then the right input.
pub fn compare(left: &Json, right: &Json) -> Result<Json, Error> {
    let left = validate(left)?;
    let right = validate(right)?;
    let a = obj(&left.normalized).unwrap();
    let b = obj(&right.normalized).unwrap();
    let ac = obj(get(a, "controlled_variables").unwrap()).unwrap();
    let bc = obj(get(b, "controlled_variables").unwrap()).unwrap();
    let ac: HashMap<&[u8], &Json> = ac
        .iter()
        .map(|(key, value)| (key.as_slice(), value))
        .collect();
    let bc: HashMap<&[u8], &Json> = bc
        .iter()
        .map(|(key, value)| (key.as_slice(), value))
        .collect();
    let at = convert(ac.get(b"parameter_count_tolerance".as_slice()).copied())?;
    let bt = convert(bc.get(b"parameter_count_tolerance".as_slice()).copied())?;
    let tolerance = if bt > at { bt } else { at };
    let mut keys: Vec<_> = ac.keys().chain(bc.keys()).copied().collect();
    keys.sort_by_key(|k| points(k));
    keys.dedup();
    let null = Json::Null;
    let mut issues = Vec::new();
    for key in keys {
        let av = ac.get(key).copied().unwrap_or(&null);
        let bv = bc.get(key).copied().unwrap_or(&null);
        if key == b"parameter_count" && numeric(av).is_some() && numeric(bv).is_some() {
            let x = convert(Some(av))?;
            let y = convert(Some(bv))?;
            let scale = x.abs().max(y.abs()).max(1.0);
            if (x - y).abs() / scale <= tolerance {
                continue;
            }
        }
        if !equal(av, bv) {
            let mut field = b"controlled_variables.".to_vec();
            field.extend_from_slice(key);
            issues.push(object(vec![
                ("field", Json::String(field)),
                ("left", copy(av)),
                ("right", copy(bv)),
            ]));
        }
    }
    for field in ["metrics", "seeds", "success", "invariant_pack"] {
        let av = get(a, field).unwrap();
        let bv = get(b, field).unwrap();
        if !equal(av, bv) {
            issues.push(object(vec![
                ("field", string(field)),
                ("left", copy(av)),
                ("right", copy(bv)),
            ]));
        }
    }
    Ok(object(vec![
        ("schema", Json::Number("1".into())),
        ("comparable", Json::Bool(issues.is_empty())),
        ("issues", Json::Array(issues)),
        ("left_digest", string(&left.digest)),
        ("right_digest", string(&right.digest)),
    ]))
}
