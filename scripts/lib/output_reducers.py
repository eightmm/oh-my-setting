"""Conservative complete-data representations; no inference or numeric rewriting."""

import json
import math
import re
from decimal import Decimal


def _dump(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), allow_nan=False)


def _object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def _scalar(value):
    return (value is None or type(value) in (bool, int)
            or type(value) is str and not any(0xD800 <= ord(char) <= 0xDFFF for char in value)
            or type(value) is float and math.isfinite(value))


def _float(token):
    value = float(token)
    # Decline rather than round a scientific literal or underflow it to zero.
    if not math.isfinite(value) or Decimal(token) != Decimal(str(value)):
        raise ValueError("JSON number would change decimal value")
    return value


def _json_table(text, max_bytes):
    try:
        data = json.loads(text, object_pairs_hook=_object, parse_float=_float,
                          parse_constant=lambda value: (_ for _ in ()).throw(ValueError(value)))
        if not isinstance(data, list) or len(data) < 2 or not all(type(row) is dict for row in data):
            return None
        if not all(_scalar(value) for row in data for value in row.values()):
            return None
        keys = list(dict.fromkeys(key for row in data for key in row))
        # Even one-character cells need a separator. Bound sparse-table expansion
        # before constructing rows for inputs with mostly distinct keys.
        if (not keys or len(keys) * len(data) * 2 > max_bytes
                or any(0xD800 <= ord(char) <= 0xDFFF for key in keys for char in key)):
            return None
        # [] is unambiguous: input cells are scalars, including null, never arrays.
        rows = [[row.get(key, []) for key in keys] for row in data]
        payload = {"keys": keys, "rows": rows}
        encoded = _dump(payload)
        check = json.loads(encoded)
        restored = [{key: cell for key, cell in zip(check["keys"], row) if cell != []}
                    for row in check["rows"]]
        # Ordinary Python equality conflates True/1/1.0. JSON encodings retain their types.
        if json.dumps(restored, sort_keys=True, allow_nan=False) != json.dumps(data, sort_keys=True, allow_nan=False):
            return None
        return encoded, {"algorithm": "json-table", "version": 1, "lossless": True,
                         "lossless_scope": "JSON values and scalar types", "source_bytes_lossless": False,
                         "rows": len(rows), "missing_cell": [],
                         "decode": "For each row zip keys with cells; omit cells equal to []; all other cells are JSON scalars."}
    except (ValueError, TypeError, OverflowError, RecursionError):
        return None


LOG_PREFIX = re.compile(r"^(?:TRACE|DEBUG|INFO|WARN(?:ING)?|ERROR|FATAL)(?:\s|:)")


def _repeated_lines(text, lines, failure_numbers):
    if not lines or not all(LOG_PREFIX.match(line) or number in failure_numbers
                            for number, line in enumerate(lines, 1)):
        return None
    runs = []
    for line in lines:
        if runs and runs[-1][1] == line:
            runs[-1][0] += 1
        else:
            runs.append([1, line])
    if not any(count >= 3 for count, _line in runs):
        return None
    encoded = _dump({"runs": runs, "final_newline": text.endswith("\n")})
    check = json.loads(encoded)
    restored = "\n".join(line for count, line in check["runs"] for _ in range(count))
    if check["final_newline"]:
        restored += "\n"
    if restored != text:
        return None
    return encoded, {"algorithm": "repeated-lines", "version": 1, "lossless": True,
                     "lossless_scope": "source bytes", "source_bytes_lossless": True,
                     "runs": len(runs),
                     "decode": "Repeat each runs [count,line] in order, join with LF; append LF only if final_newline is true."}


def reduce_output(text, lines, failure_numbers, max_bytes):
    """Use a representation only when every value survives and it saves >=20% and 64 bytes."""
    original_bytes = len(text.encode("utf-8"))
    for reducer in (lambda: _json_table(text, max_bytes),
                    lambda: _repeated_lines(text, lines, failure_numbers)):
        candidate = reducer()
        if candidate is None:
            continue
        encoded, metadata = candidate
        size = len(encoded.encode("utf-8"))
        if size <= max_bytes and size + 64 <= original_bytes and size * 5 <= original_bytes * 4:
            return encoded, metadata
    return None
