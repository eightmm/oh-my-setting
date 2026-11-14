use crate::CommandResult;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;

pub(crate) struct Args {
    pub group: String,
    pub action: String,
    values: BTreeMap<String, Value>,
}
impl Args {
    pub fn text(&self, key: &str) -> &str {
        self.values.get(key).and_then(Value::as_str).unwrap_or("")
    }
    pub fn flag(&self, key: &str) -> bool {
        self.values
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
    pub fn int(&self, key: &str) -> i64 {
        self.values
            .get(key)
            .map(|value| {
                value.as_i64().unwrap_or_else(|| {
                    value
                        .as_str()
                        .map(|number| {
                            number.parse().unwrap_or(if number.starts_with('-') {
                                i64::MIN
                            } else {
                                i64::MAX
                            })
                        })
                        .unwrap_or(0)
                })
            })
            .unwrap_or(0)
    }
    pub fn integer(&self, key: &str) -> oms_command_operations::value::V {
        let raw = self
            .values
            .get(key)
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string())
            })
            .unwrap_or_else(|| "0".into());
        oms_command_operations::value::parse_integer(&raw).unwrap_or(0_i64.into())
    }
    pub fn float(&self, key: &str) -> f64 {
        self.values.get(key).and_then(Value::as_f64).unwrap_or(0.0)
    }
    pub fn list(&self, key: &str) -> Vec<String> {
        self.values
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    }
}

pub(crate) fn parse(raw: &[OsString]) -> std::result::Result<Args, CommandResult> {
    let schema: Value = serde_json::from_str(include_str!("cli.json")).expect("checked CLI schema");
    let mut parser = &schema;
    let mut values = BTreeMap::new();
    let mut levels = Vec::new();
    let mut seen = BTreeSet::new();
    let mut mutex = BTreeMap::new();
    let mut position = 0;
    let mut positional = 0;
    let mut end = false;
    let words: Vec<String> = raw
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    let failure = |p: &Value, message: String| -> CommandResult {
        (
            2,
            vec![],
            format!(
                "{}{}: error: {}\n",
                p["usage"].as_str().unwrap_or(""),
                p["prog"].as_str().unwrap_or("oms graph"),
                message
            )
            .into_bytes(),
        )
    };
    loop {
        for option in parser["options"].as_array().unwrap() {
            let key = option["dest"].as_str().unwrap();
            if !option["default"].is_null() && !values.contains_key(key) {
                values.insert(key.to_owned(), option["default"].clone());
            }
        }
        if position >= words.len() {
            break;
        }
        let word = &words[position];
        if !end && word == "--" {
            end = true;
            position += 1;
            continue;
        }
        if !end && word.starts_with('-') && word != "-" {
            let (name, inline) = word
                .split_once('=')
                .map_or((word.as_str(), None), |(a, b)| (a, Some(b)));
            let options: Vec<&Value> = parser["options"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|o| {
                    o["names"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|n| n.as_str() == Some(name))
                })
                .collect();
            let options = if options.is_empty() {
                parser["options"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|o| {
                        o["names"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|n| n.as_str().is_some_and(|n| n.starts_with(name)))
                    })
                    .collect()
            } else {
                options
            };
            if options.len() != 1 {
                return Err(failure(parser, format!("unrecognized arguments: {word}")));
            }
            let option = options[0];
            let key = option["dest"].as_str().unwrap();
            let pair = option["nargs"].as_u64() == Some(2);
            if pair && inline.is_some() {
                return Err(failure(
                    parser,
                    format!("argument {name}: expected 2 arguments"),
                ));
            }
            let expected = if pair { "2 arguments" } else { "one argument" };
            let action = option["action"].as_str().unwrap();
            if action == "help" {
                return Err((
                    0,
                    parser["help"].as_str().unwrap().as_bytes().to_vec(),
                    vec![],
                ));
            }
            if action == "version" {
                return Err((0, b"0.1.0\n".to_vec(), vec![]));
            }
            if let Some(group) = option["mutex"].as_u64() {
                if let Some(old) = mutex.insert((levels.len(), group), name.to_owned()) {
                    if old != name {
                        return Err(failure(
                            parser,
                            format!("argument {name}: not allowed with argument {old}"),
                        ));
                    }
                }
            }
            let value = if action == "flag" {
                if inline.is_some() {
                    return Err(failure(
                        parser,
                        format!("argument {name}: ignored explicit argument"),
                    ));
                }
                json!(true)
            } else {
                let value = if let Some(v) = inline {
                    v.to_owned()
                } else {
                    position += 1;
                    words.get(position).cloned().ok_or_else(|| {
                        failure(parser, format!("argument {name}: expected {expected}"))
                    })?
                };
                if inline.is_none()
                    && value.starts_with('-')
                    && value != "-"
                    && value.parse::<f64>().is_err()
                {
                    return Err(failure(
                        parser,
                        format!("argument {name}: expected {expected}"),
                    ));
                }
                let choices = option["choices"].as_array().unwrap();
                if !choices.is_empty() && !choices.iter().any(|v| v.as_str() == Some(&value)) {
                    return Err(failure(
                        parser,
                        format!("argument {name}: invalid choice: '{value}'"),
                    ));
                }
                match option["type"].as_str().unwrap() {
                    "int" => {
                        let parsed =
                            oms_command_operations::value::parse_integer(&value).map_err(|_| {
                                failure(
                                    parser,
                                    format!("argument {name}: invalid int value: '{value}'"),
                                )
                            })?;
                        match parsed.py().parse::<i64>() {
                            Ok(number) => json!(number),
                            Err(_) => json!(parsed.py()),
                        }
                    }
                    "float" => json!(value.parse::<f64>().map_err(|_| failure(
                        parser,
                        format!("argument {name}: invalid float value: '{value}'")
                    ))?),
                    _ => json!(value),
                }
            };
            let value = if option["nargs"].as_u64() == Some(2) {
                position += 1;
                let next = words
                    .get(position)
                    .filter(|word| !word.starts_with('-'))
                    .ok_or_else(|| {
                        failure(parser, format!("argument {name}: expected 2 arguments"))
                    })?;
                json!([value, next])
            } else {
                value
            };
            if action == "append" {
                values
                    .entry(key.to_owned())
                    .or_insert(json!([]))
                    .as_array_mut()
                    .unwrap()
                    .push(value);
            } else {
                values.insert(key.to_owned(), value);
            }
            seen.insert(key.to_owned());
        } else if let Some(child) = parser["children"].get(word) {
            levels.push(word.clone());
            parser = child;
            positional = 0;
        } else {
            let options: Vec<&Value> = parser["options"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v["names"].as_array().unwrap().is_empty())
                .collect();
            if let Some(option) = options.get(positional) {
                let key = option["dest"].as_str().unwrap();
                values.insert(key.to_owned(), json!(word));
                seen.insert(key.to_owned());
                positional += 1;
            } else {
                return Err(failure(parser, format!("unrecognized arguments: {word}")));
            }
        }
        position += 1;
    }
    if levels.len() < 2 {
        return Err(failure(
            parser,
            format!(
                "the following arguments are required: {}",
                if levels.is_empty() { "group" } else { "action" }
            ),
        ));
    }
    let required: Vec<String> = parser["options"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["required"] == true && !seen.contains(o["dest"].as_str().unwrap()))
        .map(|o| {
            o["names"]
                .as_array()
                .unwrap()
                .first()
                .unwrap_or(&o["dest"])
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    if !required.is_empty() {
        return Err(failure(
            parser,
            format!(
                "the following arguments are required: {}",
                required.join(", ")
            ),
        ));
    }
    if levels[0] == "exec" && levels[1] == "commit" && !seen.contains("run") {
        values.insert(
            "run".into(),
            json!(std::env::var("OMS_GRAPH_RUN_ID").unwrap_or_default()),
        );
    }
    Ok(Args {
        group: levels[0].clone(),
        action: levels[1].clone(),
        values,
    })
}
