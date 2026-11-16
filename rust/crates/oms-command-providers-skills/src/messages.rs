use crate::common::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(crate) fn decode64(value: &str) -> Result<Vec<u8>> {
    let mut out = vec![];
    let mut acc = 0u32;
    let mut bits = 0u32;
    let mut padding = false;
    let mut count = 0usize;
    for b in value.bytes() {
        if b == b'=' {
            padding = true;
            count += 1;
            if count > 2 {
                return Err(error("invalid base64"));
            }
            continue;
        }
        if padding {
            return Err(error("invalid base64"));
        }
        let n = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return Err(error("invalid base64")),
        };
        acc = (acc << 6) | u32::from(n);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if bits == 6 || acc != 0 || (count > 0 && !value.len().is_multiple_of(4)) {
        return Err(error("invalid base64"));
    }
    Ok(out)
}
fn encode64(bytes: &[u8]) -> String {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(alphabet[((n >> 18) & 63) as usize] as char);
        out.push(alphabet[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            alphabet[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            alphabet[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
fn blocks(value: &Value) -> Result<Vec<Value>> {
    if let Some(text) = value.as_str() {
        return Ok(vec![json!({"type":"text","text":text})]);
    }
    value
        .as_array()
        .filter(|v| v.iter().all(Value::is_object))
        .cloned()
        .ok_or_else(|| error("Message content must be text or an array of content blocks"))
}
fn part(value: &Value, role: &str) -> Result<Value> {
    match value["type"].as_str() {
        Some("text") if value["text"].is_string() => Ok(
            json!({"type":if role=="assistant"{"output_text"}else{"input_text"},"text":value["text"]}),
        ),
        Some("image") if role == "user" => {
            let source = &value["source"];
            let url = match source["type"].as_str() {
                Some("base64") => {
                    let mime = source["media_type"]
                        .as_str()
                        .filter(|m| {
                            ["image/png", "image/jpeg", "image/gif", "image/webp"].contains(m)
                        })
                        .ok_or_else(|| error("Unsupported image source"))?;
                    let data = source["data"]
                        .as_str()
                        .ok_or_else(|| error("Unsupported image source"))?;
                    decode64(data)?;
                    format!("data:{mime};base64,{data}")
                }
                Some("url") => source["url"]
                    .as_str()
                    .filter(|u| u.starts_with("https://"))
                    .ok_or_else(|| error("Unsupported image source"))?
                    .to_owned(),
                _ => return Err(error("Unsupported image source")),
            };
            Ok(json!({"type":"input_image","image_url":url}))
        }
        _ => Err(error("Unsupported content block")),
    }
}
pub(crate) fn stops(body: &Value) -> Result<Vec<String>> {
    let Some(value) = body.get("stop_sequences") else {
        return Ok(vec![]);
    };
    let list = value
        .as_array()
        .filter(|v| v.len() <= 16)
        .ok_or_else(|| error("stop_sequences must contain at most 16 strings"))?;
    list.iter()
        .map(|v| {
            v.as_str()
                .filter(|s| !s.is_empty() && s.chars().count() <= 4096)
                .map(str::to_owned)
                .ok_or_else(|| error("invalid stop sequence"))
        })
        .collect()
}
pub(crate) fn from_messages(body: &Value) -> Result<Value> {
    let mut instructions = vec![];
    for part in blocks(body.get("system").unwrap_or(&json!("")))? {
        if part["type"] != "text" || !part["text"].is_string() {
            return Err(error("Only text system instructions are supported"));
        }
        instructions.push(part["text"].as_str().unwrap_or("").to_owned());
    }
    let messages = body["messages"]
        .as_array()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| error("messages must be a nonempty array"))?;
    let mut inputs = vec![];
    let mut pending = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for message in messages {
        let role = message["role"]
            .as_str()
            .filter(|r| ["user", "assistant", "system"].contains(r))
            .ok_or_else(|| error("Unsupported message role"))?;
        let mut content = vec![];
        for value in blocks(&message["content"])? {
            let kind = value["type"].as_str().unwrap_or("");
            if ["tool_use", "tool_result", "thinking", "redacted_thinking"].contains(&kind) {
                if !content.is_empty() {
                    inputs.push(json!({"role":role,"content":content}));
                    content = vec![];
                }
                match (kind, role) {
                    ("tool_use", "assistant") => {
                        let id = value["id"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .ok_or_else(|| error("Invalid tool call"))?;
                        let name = value["name"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .ok_or_else(|| error("Invalid tool call"))?;
                        if !seen.insert(id.to_owned()) {
                            return Err(error("Invalid or duplicate tool call"));
                        }
                        pending.insert(id.to_owned());
                        inputs.push(json!({"type":"function_call","call_id":id,"name":name,"arguments":value.get("input").unwrap_or(&json!({})).to_string()}));
                    }
                    ("tool_result", "user") => {
                        let id = value["tool_use_id"]
                            .as_str()
                            .ok_or_else(|| error("Tool result has no matching tool call"))?;
                        if !pending.remove(id) {
                            return Err(error("Tool result has no matching tool call"));
                        }
                        let output = blocks(value.get("content").unwrap_or(&json!("")))?
                            .iter()
                            .map(|v| part(v, "user"))
                            .collect::<Result<Vec<_>>>()?;
                        inputs.push(
                            json!({"type":"function_call_output","call_id":id,"output":output}),
                        );
                    }
                    ("thinking" | "redacted_thinking", "assistant") => {
                        if let Some(signature) = value
                            .get("signature")
                            .or_else(|| value.get("data"))
                            .and_then(Value::as_str)
                            .and_then(|s| s.strip_prefix("oms-codex:"))
                        {
                            let row = json(&decode64(signature)?)?;
                            if !row.is_object() || row["type"] != "reasoning" {
                                return Err(error("Invalid preserved Codex reasoning"));
                            }
                            inputs.push(row);
                        }
                    }
                    _ => return Err(error("Unsupported content block")),
                }
            } else {
                content.push(part(&value, if role == "system" { "user" } else { role })?);
            }
        }
        if !content.is_empty() {
            if role == "system" && content.iter().any(|v| v["type"] != "input_text") {
                return Err(error("Only text system instructions are supported"));
            }
            inputs.push(json!({"role":if role=="system"{"developer"}else{role},"content":content}));
        }
    }
    if !pending.is_empty() {
        return Err(error("Every historical tool call must have a result"));
    }
    let mut tools = vec![];
    if let Some(rows) = body.get("tools") {
        for tool in rows
            .as_array()
            .ok_or_else(|| error("Invalid function tools"))?
        {
            if tool.get("type").is_some_and(|v| v != "custom")
                || !tool["name"].is_string()
                || !tool["input_schema"].is_object()
            {
                return Err(error(
                    "Anthropic server tools are unsupported; use harness function tools",
                ));
            }
            tools.push(json!({"type":"function","name":tool["name"],"description":tool.get("description").unwrap_or(&json!("")),"parameters":tool["input_schema"],"strict":false}));
        }
    }
    let default = json!({"type":"auto"});
    let choice = body.get("tool_choice").unwrap_or(&default);
    let selected = match choice["type"].as_str() {
        Some("auto") => json!("auto"),
        Some("any") => json!("required"),
        Some("none") => json!("none"),
        Some("tool") if tools.iter().any(|v| v["name"] == choice["name"]) => {
            json!({"type":"function","name":choice["name"]})
        }
        _ => return Err(error("Unsupported tool_choice")),
    };
    if !stops(body)?.is_empty() && (!tools.is_empty() || body["stream"] == true) {
        return Err(error(
            "Custom stop sequences require nonstreaming text-only Messages requests",
        ));
    }
    Ok(
        json!({"instructions":instructions.join("\n\n"),"input":inputs,"tools":tools,"tool_choice":selected,"parallel_tool_calls":choice["disable_parallel_tool_use"]!=true}),
    )
}
pub(crate) fn subscription(
    body: &Value,
    model: &str,
    effort: Option<&str>,
    supported: &[String],
) -> Result<Value> {
    if !body.is_object() {
        return Err(error("Request must be a JSON object"));
    }
    if body
        .get("previous_response_id")
        .is_some_and(|v| !v.is_null() && v != "")
        || body["background"] == true
    {
        return Err(error(
            "The bridge requires complete, stateless conversation input",
        ));
    }
    let input = if let Some(s) = body["input"].as_str() {
        vec![json!({"role":"user","content":s})]
    } else {
        body["input"]
            .as_array()
            .cloned()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| error("input must be nonempty"))?
    };
    let mut instructions = body
        .get("instructions")
        .unwrap_or(&json!(""))
        .as_str()
        .ok_or_else(|| error("instructions must be text"))?
        .to_owned();
    let mut normalized = vec![];
    for item in input {
        if !item.is_object() {
            return Err(error("Invalid Responses input item"));
        }
        if matches!(item["role"].as_str(), Some("system" | "developer")) {
            let pieces = blocks(item.get("content").unwrap_or(&json!("")))?;
            if pieces.iter().any(|v| {
                !matches!(v["type"].as_str(), Some("text" | "input_text")) || !v["text"].is_string()
            }) {
                return Err(error("Only text system instructions are supported"));
            }
            if normalized.is_empty() {
                instructions.push_str("\n\n");
                instructions.push_str(
                    &pieces
                        .iter()
                        .map(|v| v["text"].as_str().unwrap_or(""))
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            } else {
                normalized.push(json!({"role":"developer","content":pieces.iter().map(|v|json!({"type":"input_text","text":v["text"]})).collect::<Vec<_>>()}));
            }
        } else {
            normalized.push(item);
        }
    }
    if normalized.is_empty() {
        return Err(error("input must contain a conversation"));
    }
    let mut result = json!({"model":model,"instructions":if instructions.is_empty(){"You are a helpful assistant."}else{&instructions},"input":normalized,"stream":true,"store":false,"include":["reasoning.encrypted_content"]});
    for name in [
        "tools",
        "tool_choice",
        "parallel_tool_calls",
        "text",
        "prompt_cache_key",
    ] {
        if let Some(value) = body.get(name) {
            result[name] = value.clone();
        }
    }
    let requested = body["reasoning"]["effort"].as_str();
    let requested = if matches!(requested, Some("none" | "minimal")) {
        [
            "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
        ]
        .into_iter()
        .find(|level| supported.iter().any(|s| s == level))
    } else {
        requested
    };
    let selected = requested
        .filter(|s| supported.iter().any(|v| v == s))
        .or(effort);
    if let Some(value) = selected {
        result["reasoning"] = json!({"effort":value,"summary":"auto"});
    }
    Ok(result)
}
fn usage(response: &Value) -> Value {
    let cached = response["usage"][concat!("input_to", "kens_details")]
        [concat!("cached_to", "kens")]
    .as_u64()
    .unwrap_or(0);
    json!({"input_tokens":response["usage"]["input_tokens"].as_u64().unwrap_or(0).saturating_sub(cached),"output_tokens":response["usage"]["output_tokens"].as_u64().unwrap_or(0),"cache_read_input_tokens":cached,"cache_creation_input_tokens":0})
}
pub(crate) fn message(response: &Value, model: &str, stops: &[String]) -> Result<Value> {
    let mut parts = vec![];
    for row in response["output"]
        .as_array()
        .ok_or_else(|| error("Codex returned invalid output"))?
    {
        match row["type"].as_str() {
            Some("message") => {
                for p in row["content"]
                    .as_array()
                    .ok_or_else(|| error("invalid response content"))?
                {
                    if let Some(text) = p
                        .get("text")
                        .or_else(|| p.get("refusal"))
                        .and_then(Value::as_str)
                    {
                        parts.push(json!({"type":"text","text":text}));
                    }
                }
            }
            Some("function_call") => {
                let args = json(
                    row["arguments"]
                        .as_str()
                        .ok_or_else(|| error("Codex returned invalid function arguments"))?
                        .as_bytes(),
                )?;
                if !args.is_object() {
                    return Err(error("Codex returned invalid function arguments"));
                }
                parts.push(
                    json!({"type":"tool_use","id":row["call_id"],"name":row["name"],"input":args}),
                );
            }
            Some("reasoning") => {
                if row["encrypted_content"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
                {
                    let signature = format!("oms-codex:{}", encode64(row.to_string().as_bytes()));
                    parts.push(json!({"type":"redacted_thinking","data":signature}));
                }
            }
            _ => return Err(error("Codex returned an unsupported output item")),
        }
    }
    let mut reason = if parts.iter().any(|v| v["type"] == "tool_use") {
        "tool_use"
    } else {
        "end_turn"
    };
    let mut matched = None;
    if !stops.is_empty() {
        if response["status"] != "completed" || reason == "tool_use" {
            return Err(error(
                "Codex did not complete a text-only stop-sequence response",
            ));
        }
        let text = parts
            .iter()
            .filter_map(|v| v["text"].as_str())
            .collect::<String>();
        if let Some((mut remaining, _, value)) = stops
            .iter()
            .enumerate()
            .filter_map(|(i, s)| text.find(s).map(|n| (n, i, s)))
            .min()
        {
            let mut trimmed = vec![];
            for mut row in parts {
                if let Some(text) = row["text"].as_str() {
                    let keep = remaining.min(text.len());
                    let part = text[..keep].to_owned();
                    remaining -= keep;
                    row["text"] = json!(part);
                    trimmed.push(row);
                    if remaining == 0 {
                        break;
                    }
                } else {
                    trimmed.push(row);
                }
            }
            parts = trimmed;
            reason = "stop_sequence";
            matched = Some(value.clone());
        }
    }
    if response["status"] == "incomplete" {
        reason = concat!("max_to", "kens");
    }
    Ok(
        json!({"id":format!("msg_{}",crate::https::random_hex(16)?),"type":"message","role":"assistant","model":model,"content":parts,"stop_reason":reason,"stop_sequence":matched,"usage":usage(response)}),
    )
}
pub(crate) struct MessageStream {
    model: String,
    index: i64,
    active: Option<Value>,
    tool: bool,
    started: bool,
}
impl MessageStream {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.into(),
            index: -1,
            active: None,
            tool: false,
            started: false,
        }
    }
    fn stop(&mut self, rows: &mut Vec<(String, Value)>) {
        if self.active.take().is_some() {
            rows.push(("content_block_stop".into(), json!({"index":self.index})));
        }
    }
    fn start(&mut self, key: Value, block: Value, rows: &mut Vec<(String, Value)>) {
        self.stop(rows);
        self.index += 1;
        self.active = Some(key);
        rows.push((
            "content_block_start".into(),
            json!({"index":self.index,"content_block":block}),
        ));
    }
    pub fn feed(&mut self, event: &Value) -> Result<Vec<(String, Value)>> {
        let mut rows = vec![];
        if !self.started {
            self.started = true;
            rows.push(("message_start".into(),json!({"message":{"id":format!("msg_{}",crate::https::random_hex(16)?),"type":"message","role":"assistant","model":self.model,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}})));
        }
        match event["type"].as_str().unwrap_or("") {
            "response.output_item.added" if event["item"]["type"] == "function_call" => {
                self.tool = true;
                self.start(json!(["tool",event["output_index"]]),json!({"type":"tool_use","id":event["item"]["call_id"],"name":event["item"]["name"],"input":{}}),&mut rows);
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let key = json!(["text", event["output_index"], event["content_index"]]);
                if self.active.as_ref() != Some(&key) {
                    self.start(key, json!({"type":"text","text":""}), &mut rows);
                }
                rows.push((
                    "content_block_delta".into(),
                    json!({"index":self.index,"delta":{"type":"text_delta","text":event["delta"]}}),
                ));
            }
            "response.function_call_arguments.delta" => {
                if self.active != Some(json!(["tool", event["output_index"]])) {
                    return Err(error("Codex interleaved function arguments unexpectedly"));
                }
                rows.push(("content_block_delta".into(),json!({"index":self.index,"delta":{"type":"input_json_delta","partial_json":event["delta"]}})));
            }
            "response.output_item.done" => {
                self.stop(&mut rows);
                let item = &event["item"];
                if item["type"] == "reasoning" && item["encrypted_content"].is_string() {
                    self.start(json!(["reasoning",event["output_index"]]),json!({"type":"redacted_thinking","data":format!("oms-codex:{}",encode64(item.to_string().as_bytes()))}),&mut rows);
                    self.stop(&mut rows);
                }
            }
            "response.completed" | "response.incomplete" => {
                self.stop(&mut rows);
                let reason = if event["type"] == "response.incomplete" {
                    "max_tokens"
                } else if self.tool {
                    "tool_use"
                } else {
                    "end_turn"
                };
                rows.push(("message_delta".into(),json!({"delta":{"stop_reason":reason,"stop_sequence":null},"usage":usage(&event["response"])})));
                rows.push(("message_stop".into(), json!({})));
            }
            _ => (),
        }
        Ok(rows)
    }
}
