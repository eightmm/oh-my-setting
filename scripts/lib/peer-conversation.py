"""Bounded control messages and native result envelopes for a delegated conversation."""

import json
import os
import re
import sys
import threading
from pathlib import Path


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def decode(raw):
    return json.loads(raw, object_pairs_hook=unique_object)


def control():
    # No buffered read-ahead: the next process must receive the next line.
    raw = bytearray()
    while len(raw) <= 32768:
        char = os.read(0, 1)
        if not char:
            raise ValueError("control input closed before finish")
        if char == b"\n":
            break
        raw.extend(char)
    else:
        raise ValueError("control message exceeds 32768 bytes")
    message = decode(raw.decode("utf-8"))
    if not isinstance(message, dict):
        raise ValueError("expected a JSON object")
    if set(message) == {"finish"} and message["finish"] is True:
        return 10
    if set(message) != {"prompt"} or not isinstance(message["prompt"], str):
        raise ValueError("expected prompt text or finish:true, with no extra fields")
    prompt = message["prompt"]
    if not prompt.strip() or any(ord(c) < 32 and c not in "\n\t\r" for c in prompt):
        raise ValueError("empty prompt or prohibited control character")
    if prompt.lstrip().startswith("/"):
        raise ValueError("native slash commands are not conversation instructions")
    print(prompt, end="")
    return 0


def result(provider, path, expected):
    with open(path, "rb") as handle:
        raw = handle.read(1048577)
    if len(raw) > 1048576:
        raise ValueError("provider envelope exceeds 1 MiB")
    envelopes = []
    for line in raw.decode("utf-8").splitlines():
        if not line.lstrip().startswith("{"):
            continue
        doc = decode(line)
        if isinstance(doc, dict) and (
            doc.get("type") == "result" if provider == "claude" else "conversation_id" in doc
        ):
            envelopes.append(doc)
    if len(envelopes) != 1:
        raise ValueError("expected exactly one native result envelope")
    doc = envelopes[0]
    if provider == "claude":
        ok = doc.get("subtype") == "success" and doc.get("is_error") is False
        ok = ok and doc.get("stop_reason") in (None, "end_turn", "stop_sequence")
        session_id, answer = doc.get("session_id"), doc.get("result")
    else:
        ok = doc.get("status") == "SUCCESS" and not doc.get("error")
        session_id, answer = doc.get("conversation_id"), doc.get("response")
    if not ok:
        raise ValueError("provider turn did not finish successfully or requires permission")
    if not isinstance(session_id, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,127}", session_id):
        raise ValueError("missing or invalid native session ID")
    if expected and session_id != expected:
        raise ValueError("native session ID changed")
    if not isinstance(answer, str) or not answer.strip():
        raise ValueError("provider returned no answer")
    denials = doc.get("permission_denials", []) if provider == "claude" else []
    if not isinstance(denials, list) or not all(isinstance(item, dict) for item in denials):
        raise ValueError("invalid permission denials")
    usage = doc.get("usage") or {}
    if not isinstance(usage, dict):
        raise ValueError("invalid provider usage object")
    tokens = sum(usage.get(k, 0) for k in ("input_tokens", "output_tokens")
                 if type(usage.get(k, 0)) is int and usage.get(k, 0) >= 0)
    return {"session_id": session_id, "response": answer, "tokens": tokens,
            "permission_denials": len(denials)}


def main():
    mode = sys.argv[1]
    if mode == "input":
        if len(sys.argv) == 2:
            return control()
        seconds = int(sys.argv[2])
        if not 1 <= seconds <= 3600:
            raise ValueError("input timeout must be 1-3600 seconds")
        # Keep the reader in the terminal's foreground process group. An outer
        # timeout in a background job makes a real TTY stop it with SIGTTIN.
        def expired():
            os.write(2, b"error: conversation input timed out\n")
            os._exit(124)
        timer = threading.Timer(seconds, expired)
        timer.daemon = True
        timer.start()
        try:
            return control()
        finally:
            timer.cancel()
    if mode == "result":
        print(json.dumps(result(*sys.argv[2:]), ensure_ascii=True))
    elif mode == "id":
        print(decode(Path(sys.argv[2]).read_text(encoding="utf-8"))["session_id"])
    elif mode == "denials":
        print(decode(Path(sys.argv[2]).read_text(encoding="utf-8"))["permission_denials"])
    elif mode == "emit":
        doc = decode(Path(sys.argv[2]).read_text(encoding="utf-8"))
        answer = doc["response"]
        print(json.dumps({"event": "turn", "turn": int(sys.argv[3]),
                          "response": answer[:12000], "truncated": len(answer) > 12000,
                          "permission_denials": doc["permission_denials"],
                          "worktree": sys.argv[4],
                          "verified": False}, ensure_ascii=True))
    elif mode == "text":
        doc = decode(Path(sys.argv[2]).read_text(encoding="utf-8"))
        print(doc["response"])
        if doc["permission_denials"]:
            print("Permission denials: %d; resolve in a follow-up before finish." % doc["permission_denials"])
        print("tokens used\n%d" % doc["tokens"])
    else:
        raise ValueError("unknown conversation operation")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ValueError, OSError, TypeError, KeyError) as exc:
        # Never echo rejected input or native output into diagnostics.
        print("error: conversation protocol: " + str(exc).split("\n")[0][:160], file=sys.stderr)
        raise SystemExit(2)
