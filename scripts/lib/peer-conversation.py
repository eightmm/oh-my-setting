"""Bounded control messages and native result envelopes for a delegated conversation."""

import json
import os
import re
import sys
import tempfile
import threading
from pathlib import Path


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


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
    message = json.loads(raw.decode("utf-8"), object_pairs_hook=unique_object)
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


def native_lines(path):
    with open(path, "rb") as handle:
        raw = handle.read(1048577)
    if len(raw) > 1048576:
        raise ValueError("provider envelope exceeds 1 MiB")
    return raw.decode("utf-8").splitlines()


class ProviderTurnError(ValueError):
    """An antigravity turn that ended in its own error; carries the normalized stop line."""

    def __init__(self, message, stop):
        super().__init__(message)
        self.stop = stop


def _bounded(value, limit):
    value = value if isinstance(value, str) else ""
    return re.sub(r"\s+", " ", re.sub(r"[\x00-\x1f\x7f]", " ", value)).strip()[:limit]


# The same two transient phrases the provider-error scanner treats as agy's own stream failure.
AGY_STREAM_CUT = re.compile(r"stream was interrupted|stream idle timeout", re.I)
AGY_TOKEN_LIMIT = re.compile(r"output token limit|max_tokens", re.I)


def agy_turn_error(doc):
    # The cause is read from the whole error field (the envelope is already bounded to 1 MiB); only the
    # displayed copy is cut, so a long message cannot lose its cause to the display limit.
    cause = _bounded(doc.get("error"), 1048576)
    error = cause[:240]
    if doc.get("status") != "ERROR" and not error:
        return None
    finished = re.sub(r"[^A-Za-z0-9_.-]", "", _bounded(doc.get("finished_reason"), 40)) or "error"
    # Only a recognised cause is named; any other error stays a failed turn rather than a stream cut.
    reason = ("max_tokens" if AGY_TOKEN_LIMIT.search(cause) else
              "stream_truncated" if AGY_STREAM_CUT.search(cause) else "turn_failed")
    message = "antigravity turn ended with status=%s finished_reason=%s error=%s" % (
        _bounded(doc.get("status"), 20) or "unknown", finished, error or "none")
    stop = "stop-reason: provider=antigravity reason=%s subtype=%s is_error=1" % (reason, finished)
    return ProviderTurnError(message, stop)


def result(provider, lines, expected, include_usage=False):
    envelopes = []
    for line in lines:
        if not line.lstrip().startswith("{"):
            continue
        try:
            doc = json.loads(line, object_pairs_hook=unique_object)
        except ValueError:
            if include_usage:
                continue
            raise
        if isinstance(doc, dict) and (
            doc.get("type") == "result" if provider == "claude" else "conversation_id" in doc
        ):
            envelopes.append(doc)
    if not envelopes or (not include_usage and len(envelopes) != 1):
        raise ValueError("expected exactly one native result envelope")
    doc = envelopes[-1]
    if provider == "claude":
        ok = doc.get("subtype") == "success" and doc.get("is_error") is False
        ok = ok and doc.get("stop_reason") in (None, "end_turn", "stop_sequence")
        session_id, answer = doc.get("session_id"), doc.get("result")
    else:
        ok = doc.get("status") == "SUCCESS" and not doc.get("error")
        session_id, answer = doc.get("conversation_id"), doc.get("response")
    if not ok:
        if provider != "claude" and include_usage:
            failure = agy_turn_error(doc)
            if failure:
                raise failure
        raise ValueError("provider turn did not finish successfully or requires permission")
    if not isinstance(session_id, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,127}", session_id):
        raise ValueError("missing or invalid native session ID")
    if expected and session_id != expected:
        raise ValueError("native session ID changed")
    timeout = include_usage and doc.get("finished_reason") == "print_timeout"
    if not isinstance(answer, str) or (not answer.strip() and not timeout):
        raise ValueError("provider returned no answer")
    denials = doc.get("permission_denials", []) if provider == "claude" else []
    if not isinstance(denials, list) or not all(isinstance(item, dict) for item in denials):
        raise ValueError("invalid permission denials")
    usage = doc.get("usage") or {}
    if not isinstance(usage, dict):
        raise ValueError("invalid provider usage object")
    tokens = sum(usage.get(k, 0) for k in ("input_tokens", "output_tokens")
                 if type(usage.get(k, 0)) is int and usage.get(k, 0) >= 0)
    parsed = {"session_id": session_id, "response": answer, "tokens": tokens,
              "permission_denials": len(denials)}
    if include_usage:
        parsed["usage"] = usage
        parsed["timeout"] = timeout
    return parsed


def agy_text(path):
    from peer_artifacts import usage_footer
    lines = native_lines(path)
    try:
        parsed = result("antigravity", lines, "", include_usage=True)
    except ProviderTurnError as failure:
        # The raw envelope, partial response included, stays in the artifact for diagnosis.
        print("Error: " + str(failure) + "\n" + failure.stop, file=sys.stderr)
        raise SystemExit(2)
    usage = dict(parsed["usage"])
    usage.update({"cache_read_input_tokens": usage.get("cache_read_tokens")})
    footer = usage_footer("antigravity", [usage])
    answer = parsed["response"]
    output = answer + ("\n" if answer and not answer.endswith("\n") else "")
    output += "\n".join(footer) + "\n"
    banners = [line for line in lines
               if re.fullmatch(r"\[agy\] print timeout after .+ with turn in progress; returning partial output\s*", line)]
    if banners:
        output += "\n".join(banners) + "\n"
    if parsed["timeout"] or banners:
        output += "stop-reason: provider=antigravity reason=stream_truncated subtype=print_timeout is_error=0\n"
    fd, temporary = tempfile.mkstemp(prefix=".agy-output-", dir=os.path.dirname(os.path.abspath(path)))
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="") as handle:
            handle.write(output)
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def prepare(provider, path, expected, turn, workdir, scratch):
    doc = result(provider, native_lines(path), expected)
    answer = doc["response"]
    text = answer + "\n"
    if doc["permission_denials"]:
        text += "Permission denials: %d; resolve in a follow-up before finish.\n" % doc["permission_denials"]
    text += "tokens used\n%d\n" % doc["tokens"]
    event = {"event": "turn", "turn": int(turn),
             "response": answer[:12000], "truncated": len(answer) > 12000,
             "permission_denials": doc["permission_denials"],
             "worktree": workdir, "verified": False}
    Path(scratch, "text").write_text(text, encoding="utf-8", newline="")
    # The shell emits this only after its policy/answer checks have passed.
    Path(scratch, "event").write_text(json.dumps(event, ensure_ascii=True) + "\n", encoding="utf-8", newline="")
    print(doc["session_id"])
    print(doc["permission_denials"])


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
    if mode == "agy-text":
        agy_text(sys.argv[2])
    elif mode == "prepare":
        prepare(*sys.argv[2:])
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
