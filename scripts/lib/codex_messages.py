"""Translate Messages tool loops to stateless Responses without executing tools."""

import base64
import json
import uuid

from codex_auth_broker import BridgeError


def json_text(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), allow_nan=False)


def blocks(value):
    if isinstance(value, str):
        return [{"type": "text", "text": value}]
    if not isinstance(value, list) or any(not isinstance(row, dict) for row in value):
        raise BridgeError("Message content must be text or an array of content blocks")
    return value


def image(part):
    source = part.get("source", {})
    if source.get("type") == "base64":
        mime = source.get("media_type")
        data = source.get("data")
        if mime not in ("image/png", "image/jpeg", "image/gif", "image/webp") or not isinstance(data, str):
            raise BridgeError("Unsupported image source")
        try:
            base64.b64decode(data, validate=True)
        except ValueError:
            raise BridgeError("Invalid base64 image") from None
        url = "data:%s;base64,%s" % (mime, data)
    elif source.get("type") == "url" and isinstance(source.get("url"), str) and source["url"].startswith("https://"):
        url = source["url"]
    else:
        raise BridgeError("Unsupported image source")
    return {"type": "input_image", "image_url": url}


def content_part(part, role):
    kind = part.get("type")
    if kind == "text" and isinstance(part.get("text"), str):
        return {"type": "output_text" if role == "assistant" else "input_text", "text": part["text"]}
    if kind == "image" and role == "user":
        return image(part)
    raise BridgeError("Unsupported content block: " + str(kind))


def from_messages(body):
    instructions = []
    for part in blocks(body.get("system", "")):
        if part.get("type") != "text" or not isinstance(part.get("text"), str):
            raise BridgeError("Only text system instructions are supported")
        instructions.append(part["text"])
    messages = body.get("messages")
    if not isinstance(messages, list) or not messages:
        raise BridgeError("messages must be a nonempty array")
    inputs = []
    pending_calls = set()
    seen_calls = set()
    for message in messages:
        if not isinstance(message, dict) or message.get("role") not in ("user", "assistant", "system"):
            raise BridgeError("Unsupported message role")
        role = message["role"]
        if role == "system":
            parts = [content_part(part, "user") for part in blocks(message.get("content"))]
            if any(part["type"] != "input_text" for part in parts):
                raise BridgeError("Only text system instructions are supported")
            inputs.append({"role": "developer", "content": parts})
            continue
        content = []

        def flush():
            if content:
                inputs.append({"role": role, "content": list(content)})
                content.clear()

        for part in blocks(message.get("content")):
            kind = part.get("type")
            if kind == "tool_use" and role == "assistant":
                flush()
                call_id, name = part.get("id"), part.get("name")
                if not isinstance(call_id, str) or not call_id or call_id in seen_calls or not isinstance(name, str) or not name:
                    raise BridgeError("Invalid or duplicate tool call")
                inputs.append({"type": "function_call", "call_id": call_id, "name": name,
                               "arguments": json_text(part.get("input", {}))})
                pending_calls.add(call_id)
                seen_calls.add(call_id)
            elif kind == "tool_result" and role == "user":
                flush()
                call_id = part.get("tool_use_id")
                if call_id not in pending_calls:
                    raise BridgeError("Tool result has no matching tool call")
                result = [content_part(row, "user") for row in blocks(part.get("content", ""))]
                inputs.append({"type": "function_call_output", "call_id": call_id, "output": result})
                pending_calls.remove(call_id)
            elif kind in ("thinking", "redacted_thinking") and role == "assistant":
                flush()
                signature = part.get("signature", part.get("data", ""))
                if isinstance(signature, str) and signature.startswith("oms-codex:"):
                    try:
                        row = json.loads(base64.b64decode(signature[10:], validate=True))
                        if not isinstance(row, dict) or row.get("type") != "reasoning":
                            raise ValueError()
                    except (ValueError, TypeError):
                        raise BridgeError("Invalid preserved Codex reasoning") from None
                    inputs.append(row)
            else:
                content.append(content_part(part, role))
        flush()
    if pending_calls:
        raise BridgeError("Every historical tool call must have a result")
    tools = []
    for tool in body.get("tools", []):
        if not isinstance(tool, dict) or tool.get("type", "custom") != "custom":
            raise BridgeError("Anthropic server tools are unsupported; use harness function tools")
        if not isinstance(tool.get("name"), str) or not isinstance(tool.get("input_schema"), dict):
            raise BridgeError("Invalid function tool")
        tools.append({"type": "function", "name": tool["name"], "description": tool.get("description", ""),
                      "parameters": tool["input_schema"], "strict": False})
    choice = body.get("tool_choice", {"type": "auto"})
    if not isinstance(choice, dict):
        raise BridgeError("Invalid tool_choice")
    kind = choice.get("type")
    choices = {"auto": "auto", "any": "required", "none": "none"}
    if kind == "tool" and any(row["name"] == choice.get("name") for row in tools):
        selected = {"type": "function", "name": choice["name"]}
    elif kind in choices:
        selected = choices[kind]
    else:
        raise BridgeError("Unsupported tool_choice")
    stops = stop_sequences(body)
    if stops and (tools or body.get("stream")):
        raise BridgeError("Custom stop sequences require nonstreaming text-only Messages requests")
    return {"instructions": "\n\n".join(instructions), "input": inputs, "tools": tools,
            "tool_choice": selected, "parallel_tool_calls": not choice.get("disable_parallel_tool_use", False)}


def stop_sequences(body):
    stops = body.get("stop_sequences", [])
    if (not isinstance(stops, list) or len(stops) > 16 or
            any(not isinstance(stop, str) or not stop or len(stop) > 4096 for stop in stops)):
        raise BridgeError("stop_sequences must contain at most 16 nonempty strings of at most 4096 characters")
    return stops


def subscription_request(body, model, effort, supported_efforts=None):
    if not isinstance(body, dict):
        raise BridgeError("Request must be a JSON object")
    supported = tuple(supported_efforts) if supported_efforts is not None else (effort,)
    reasoning = body.get("reasoning")
    requested = reasoning.get("effort") if isinstance(reasoning, dict) else None
    if requested in ("none", "minimal"):
        requested = next((level for level in ("none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra")
                          if level in supported), effort)
    if requested in supported and isinstance(requested, str):
        effort = requested
    result = dict(body)
    inputs = result.get("input")
    if isinstance(inputs, str):
        inputs = [{"role": "user", "content": inputs}]
    if not isinstance(inputs, list) or not inputs:
        raise BridgeError("input must be nonempty")
    instructions = result.get("instructions", "")
    if not isinstance(instructions, str):
        raise BridgeError("instructions must be text")
    normalized = []
    for part in inputs:
        if not isinstance(part, dict):
            raise BridgeError("Invalid Responses input item")
        if part.get("role") in ("system", "developer"):
            pieces = blocks(part.get("content", ""))
            if any(row.get("type") not in ("text", "input_text") or not isinstance(row.get("text"), str) for row in pieces):
                raise BridgeError("Only text system instructions are supported")
            if normalized:
                normalized.append({"role": "developer", "content": [
                    {"type": "input_text", "text": row["text"]} for row in pieces]})
            else:
                instructions += "\n\n" + "\n".join(row["text"] for row in pieces)
        else:
            normalized.append(part)
    if not normalized:
        raise BridgeError("input must contain a conversation")
    if result.get("previous_response_id") or result.get("background"):
        raise BridgeError("The bridge requires complete, stateless conversation input")
    # The legacy subscription endpoint rejects output-token caps and persistence.
    allowed = ("tools", "tool_choice", "parallel_tool_calls", "text", "prompt_cache_key")
    result = {key: result[key] for key in allowed if key in result}
    result.update(model=model, instructions=instructions or "You are a helpful assistant.",
                  input=normalized, stream=True, store=False, include=["reasoning.encrypted_content"])
    if effort:
        result["reasoning"] = {"effort": effort, "summary": "auto"}
    return result


def usage(response):
    row = response.get("usage") or {}
    cached = (row.get("input_tokens_details") or {}).get("cached_tokens", 0)
    return {"input_tokens": max(0, row.get("input_tokens", 0) - cached),
            "output_tokens": row.get("output_tokens", 0), "cache_read_input_tokens": cached,
            "cache_creation_input_tokens": 0}


def message(response, model, stops=()):
    parts = []
    for row in response.get("output", []):
        kind = row.get("type")
        if kind == "message":
            for part in row.get("content", []):
                if part.get("type") == "output_text":
                    parts.append({"type": "text", "text": part["text"]})
                elif part.get("type") == "refusal":
                    parts.append({"type": "text", "text": part["refusal"]})
        elif kind == "function_call":
            try:
                args = json.loads(row["arguments"])
                if not isinstance(args, dict):
                    raise ValueError()
            except (ValueError, KeyError):
                raise BridgeError("Codex returned invalid function arguments", 502) from None
            parts.append({"type": "tool_use", "id": row["call_id"], "name": row["name"], "input": args})
        elif kind == "reasoning":
            if row.get("encrypted_content"):
                summary = "\n".join(part.get("text", "") for part in row.get("summary", []))
                signature = "oms-codex:" + base64.b64encode(json_text(row).encode()).decode()
                if summary:
                    parts.append({"type": "thinking", "thinking": summary, "signature": signature})
                else:
                    parts.append({"type": "redacted_thinking", "data": signature})
        else:
            raise BridgeError("Codex returned an unsupported output item", 502)
    stop = "tool_use" if any(row["type"] == "tool_use" for row in parts) else "end_turn"
    matched = None
    if stops:
        # Classifier requests need Messages stop semantics; Responses has no stop field.
        # Wait for authoritative completion and never turn partial verdicts into success.
        if response.get("status") != "completed" or stop == "tool_use":
            raise BridgeError("Codex did not complete a text-only stop-sequence response", 502)
        text = "".join(row["text"] for row in parts if row["type"] == "text")
        matches = [(text.find(value), index, value) for index, value in enumerate(stops) if value in text]
        if matches:
            remaining, _index, matched = min(matches)
            trimmed = []
            for row in parts:
                if row["type"] == "text":
                    trimmed.append(dict(row, text=row["text"][:remaining]))
                    remaining -= min(remaining, len(row["text"]))
                    if remaining == 0:
                        break
                else:
                    trimmed.append(row)
            parts, stop = trimmed, "stop_sequence"
    if response.get("status") == "incomplete":
        stop = "max_tokens"
    return {"id": "msg_" + uuid.uuid4().hex, "type": "message", "role": "assistant", "model": model,
            "content": parts, "stop_reason": stop, "stop_sequence": matched, "usage": usage(response)}


class MessageStream:
    def __init__(self, model):
        self.model = model
        self.active = None
        self.index = -1
        self.tool = False
        self.started = False
        self.completed = False

    def start(self):
        self.started = True
        return [("message_start", {"message": {"id": "msg_" + uuid.uuid4().hex, "type": "message",
                 "role": "assistant", "model": self.model, "content": [], "stop_reason": None,
                 "stop_sequence": None, "usage": {"input_tokens": 0, "output_tokens": 0}}})]

    def stop_block(self):
        if self.active is None:
            return []
        self.active = None
        return [("content_block_stop", {"index": self.index})]

    def start_block(self, key, block):
        rows = self.stop_block()
        self.index += 1
        self.active = key
        rows.append(("content_block_start", {"index": self.index, "content_block": block}))
        return rows

    def feed(self, event):
        rows = [] if self.started else self.start()
        kind = event.get("type", "")
        if kind in ("error", "response.failed"):
            raise BridgeError("Codex inference failed", 502)
        if kind == "response.output_item.added":
            item = event.get("item", {})
            if item.get("type") == "function_call":
                self.tool = True
                rows += self.start_block(("tool", event.get("output_index")),
                                         {"type": "tool_use", "id": item["call_id"], "name": item["name"], "input": {}})
        elif kind in ("response.output_text.delta", "response.refusal.delta"):
            key = ("text", event.get("output_index"), event.get("content_index"))
            if self.active != key:
                rows += self.start_block(key, {"type": "text", "text": ""})
            rows.append(("content_block_delta", {"index": self.index,
                         "delta": {"type": "text_delta", "text": event.get("delta", "")}}))
        elif kind == "response.function_call_arguments.delta":
            if self.active != ("tool", event.get("output_index")):
                raise BridgeError("Codex interleaved function arguments unexpectedly", 502)
            rows.append(("content_block_delta", {"index": self.index,
                         "delta": {"type": "input_json_delta", "partial_json": event.get("delta", "")}}))
        elif kind == "response.output_item.done":
            rows += self.stop_block()
            item = event.get("item", {})
            if item.get("type") == "reasoning" and item.get("encrypted_content"):
                signature = "oms-codex:" + base64.b64encode(json_text(item).encode()).decode()
                rows += self.start_block(("reasoning", event.get("output_index")),
                                         {"type": "redacted_thinking", "data": signature})
                rows += self.stop_block()
        elif kind in ("response.completed", "response.incomplete"):
            response = event.get("response", {})
            if response.get("status") == "failed":
                raise BridgeError("Codex inference failed", 502)
            rows += self.stop_block()
            reason = "max_tokens" if kind == "response.incomplete" else ("tool_use" if self.tool else "end_turn")
            rows += [("message_delta", {"delta": {"stop_reason": reason, "stop_sequence": None}, "usage": usage(response)}),
                     ("message_stop", {})]
            self.completed = True
        return rows
