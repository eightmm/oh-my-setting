#!/usr/bin/env python3
"""Canonical repository path-scope normalization and membership predicates."""

from __future__ import print_function

import functools
import re
import string
import sys
import unicodedata


GLOB_MAGIC = "*?["
MAX_SCOPE_ITEM_BYTES = 4096
MAX_BRACKET_RANGE = 4096
MAX_BRACKET_EXPANSION = 4096
MAX_BRACKET_TOKENS = 4096
MAX_PATTERN_EXPANSION = 4096
MAX_PATTERN_REGEX_OUTPUT = 16384


def _ascii_range(first, last):
    return set(chr(value) for value in range(first, last + 1))


_POSIX_CLASSES = {
    "alnum": set(string.ascii_letters + string.digits),
    "alpha": set(string.ascii_letters),
    "ascii": _ascii_range(0x00, 0x7f),
    "blank": set(" \t"),
    "cntrl": _ascii_range(0x00, 0x1f) | {chr(0x7f)},
    "digit": set(string.digits),
    "graph": _ascii_range(0x21, 0x7e),
    "lower": set(string.ascii_lowercase),
    "print": _ascii_range(0x20, 0x7e),
    "punct": set(string.punctuation),
    "space": set(" \t\n\r\v\f"),
    "upper": set(string.ascii_uppercase),
    "word": set(string.ascii_letters + string.digits + "_"),
    "xdigit": set(string.hexdigits),
}


def _reject_controls(value, label):
    if any(unicodedata.category(ch) in ("Cc", "Cf", "Cs") for ch in value):
        raise ValueError("%s contains a control or format character" % label)


def _validate_scope_item_size(value, label="scope item"):
    """Bound UTF-8 size without allocating an encoded copy of attacker input."""
    utf8_bytes = 0
    for char in value:
        codepoint = ord(char)
        if codepoint <= 0x7f:
            utf8_bytes += 1
        elif codepoint <= 0x7ff:
            utf8_bytes += 2
        elif codepoint <= 0xffff:
            utf8_bytes += 3
        else:
            utf8_bytes += 4
        if utf8_bytes > MAX_SCOPE_ITEM_BYTES:
            raise ValueError(
                "%s exceeds the %d-byte UTF-8 limit" % (
                    label, MAX_SCOPE_ITEM_BYTES
                )
            )


def normalize(value, label="scope path"):
    """Return one normalized repo-relative scope declaration."""
    if not isinstance(value, str):
        raise ValueError("%s must be a string" % label)
    _validate_scope_item_size(value, label)
    _reject_controls(value, label)
    value = value.strip().replace("\\", "/")
    while value.startswith("./"):
        value = value[2:]
    value = value.rstrip("/") or "."
    if (value.startswith("/") or re.match(r"^[A-Za-z]:", value) or
            (value != "." and any(
                part in ("", ".", "..") for part in value.split("/")
            ))):
        raise ValueError("%s must be a normalized repo-relative path" % label)
    return value


def has_glob(pattern):
    return any(char in pattern for char in GLOB_MAGIC)


def _regex_class(chars, negated):
    if not chars:
        return r"[\s\S]" if negated else r"(?!)"
    encoded = []
    for char in sorted(chars, key=ord):
        code = ord(char)
        if code < 0x20 or code == 0x7f:
            encoded.append(r"\x%02x" % code)
        elif char in "\\^-][":
            encoded.append("\\" + char)
        else:
            encoded.append(char)
    return "[%s%s]" % ("^" if negated else "", "".join(encoded))


def _append_bracket_token(tokens, token):
    if len(tokens) >= MAX_BRACKET_TOKENS:
        raise ValueError("scope pattern bracket expression has too many tokens")
    tokens.append(token)


def _charge_bracket_expansion(local_count, amount, pattern_expansion):
    local_count += amount
    pattern_count = pattern_expansion[0] + amount
    if local_count > MAX_BRACKET_EXPANSION:
        raise ValueError("scope pattern bracket expansion is too large")
    if pattern_count > MAX_PATTERN_EXPANSION:
        raise ValueError("scope pattern total bracket expansion is too large")
    pattern_expansion[0] = pattern_count
    return local_count


def _bracket_regex(pattern, start, pattern_expansion):
    """Translate one Bash bracket expression, or return None if unclosed."""
    position = start + 1
    negated = False
    if position < len(pattern) and pattern[position] in "!^":
        negated = True
        position += 1

    tokens = []
    # Bash permits a literal closing bracket as the first set member.
    if position < len(pattern) and pattern[position] == "]":
        _append_bracket_token(tokens, ("char", "]"))
        position += 1

    while position < len(pattern):
        if pattern[position] == "]":
            chars = set()
            expansion_count = 0
            index = 0
            while index < len(tokens):
                token_kind, token_value = tokens[index]
                if (token_kind == "char" and index + 2 < len(tokens) and
                        tokens[index + 1] == ("char", "-") and
                        tokens[index + 2][0] == "char"):
                    range_end = tokens[index + 2][1]
                    if ord(token_value) > ord(range_end):
                        raise ValueError("scope pattern has a descending bracket range")
                    range_span = ord(range_end) - ord(token_value) + 1
                    if range_span > MAX_BRACKET_RANGE:
                        raise ValueError("scope pattern bracket range is too large")
                    expansion_count = _charge_bracket_expansion(
                        expansion_count, range_span, pattern_expansion
                    )
                    chars.update(chr(value) for value in range(
                        ord(token_value), ord(range_end) + 1
                    ))
                    if len(chars) > MAX_BRACKET_RANGE:
                        raise ValueError("scope pattern bracket set is too large")
                    index += 3
                    continue
                if token_kind == "set":
                    expansion_count = _charge_bracket_expansion(
                        expansion_count, len(token_value), pattern_expansion
                    )
                    chars.update(token_value)
                else:
                    expansion_count = _charge_bracket_expansion(
                        expansion_count, 1, pattern_expansion
                    )
                    chars.add(token_value)
                if len(chars) > MAX_BRACKET_RANGE:
                    raise ValueError("scope pattern bracket set is too large")
                index += 1
            return _regex_class(chars, negated), position + 1

        if pattern.startswith("[:", position):
            class_end = pattern.find(":]", position + 2)
            if class_end >= 0:
                class_name = pattern[position + 2:class_end]
                if class_name not in _POSIX_CLASSES:
                    raise ValueError("scope pattern has an unsupported POSIX class")
                _append_bracket_token(
                    tokens, ("set", _POSIX_CLASSES[class_name])
                )
                position = class_end + 2
                continue
        if pattern.startswith("[.", position) or pattern.startswith("[=", position):
            marker = pattern[position + 1]
            if pattern.find(marker + "]", position + 2) >= 0:
                raise ValueError("scope pattern has an unsupported collating expression")
        _append_bracket_token(tokens, ("char", pattern[position]))
        position += 1
    return None


def _append_regex_part(parts, fragment, output_size):
    output_size += len(fragment)
    if output_size > MAX_PATTERN_REGEX_OUTPUT:
        raise ValueError("scope pattern regex output is too large")
    parts.append(fragment)
    return output_size


@functools.lru_cache(maxsize=512)
def _compile_glob(pattern):
    _validate_scope_item_size(pattern, "scope pattern")
    parts = []
    output_size = _append_regex_part(parts, r"\A", 0)
    pattern_expansion = [0]
    position = 0
    while position < len(pattern):
        char = pattern[position]
        if char == "*":
            output_size = _append_regex_part(
                parts, r"[\s\S]*", output_size
            )
            position += 1
        elif char == "?":
            output_size = _append_regex_part(
                parts, r"[\s\S]", output_size
            )
            position += 1
        elif char == "[":
            translated = _bracket_regex(
                pattern, position, pattern_expansion
            )
            if translated is None:
                output_size = _append_regex_part(
                    parts, r"\[", output_size
                )
                position += 1
            else:
                fragment, position = translated
                output_size = _append_regex_part(
                    parts, fragment, output_size
                )
        else:
            output_size = _append_regex_part(
                parts, re.escape(char), output_size
            )
            position += 1
    _append_regex_part(parts, r"\Z", output_size)
    return re.compile("".join(parts))


def matches(path, pattern):
    """Whether a Git-relative path belongs to a normalized scope pattern."""
    if has_glob(pattern):
        compiled = _compile_glob(pattern)
        return compiled.match(path) is not None
    return pattern == "." or path == pattern or path.startswith(pattern + "/")


def validate_pattern(pattern):
    """Compile glob grammar before a scope declaration can be trusted."""
    _validate_scope_item_size(pattern, "scope pattern")
    if has_glob(pattern):
        _compile_glob(pattern)
    return pattern


def validate_patterns(patterns):
    patterns = tuple(patterns)
    for pattern in patterns:
        validate_pattern(pattern)
    return patterns


def matches_any(path, patterns):
    patterns = validate_patterns(patterns)
    return any(matches(path, pattern) for pattern in patterns)


def within_envelope(candidate, envelope):
    """Whether a task scope stays inside a reviewed envelope item.

    Comparing two different glob spellings by matching their pattern text is
    not language containment. Keep that case conservative: glob task scopes
    may equal a glob envelope exactly or narrow a literal directory envelope.
    Concrete task paths may still be selected by a reviewed glob.
    """
    validate_pattern(candidate)
    envelope = validate_patterns(envelope)
    candidate_is_glob = has_glob(candidate)
    for pattern in envelope:
        pattern_is_glob = has_glob(pattern)
        if candidate_is_glob and pattern_is_glob:
            if candidate == pattern:
                return True
            continue
        if matches(candidate, pattern):
            return True
    return False


def classify(path, allowed, forbidden):
    """Return forbidden, outside, or allowed; deny always wins."""
    allowed = validate_patterns(allowed)
    forbidden = validate_patterns(forbidden)
    if any(matches(path, pattern) for pattern in forbidden):
        return "forbidden"
    if allowed and not any(matches(path, pattern) for pattern in allowed):
        return "outside"
    return "allowed"


def normalize_list(text):
    values = [
        normalize(item)
        for item in re.split(r"[,\s]+", text)
        if item.strip()
    ]
    if not values:
        raise ValueError("scope list is empty")
    values = sorted(set(values))
    validate_patterns(values)
    return values


def _usage():
    print(
        "usage: path_scope.py match-any PATH PATTERN... | "
        "inside CANDIDATE ENVELOPE... | normalize-list TEXT",
        file=sys.stderr,
    )
    return 2


def main(argv):
    if len(argv) < 2:
        return _usage()
    action = argv[1]
    try:
        if action == "match-any" and len(argv) >= 4:
            path = argv[2]
            patterns = [normalize(item) for item in argv[3:]]
            return 0 if matches_any(path, patterns) else 1
        if action == "inside" and len(argv) >= 4:
            candidate = normalize(argv[2])
            envelope = [normalize(item) for item in argv[3:]]
            return 0 if within_envelope(candidate, envelope) else 1
        if action == "normalize-list" and len(argv) == 3:
            print(",".join(normalize_list(argv[2])))
            return 0
    except ValueError as exc:
        print("error: %s" % exc, file=sys.stderr)
        return 2
    return _usage()


if __name__ == "__main__":
    sys.exit(main(sys.argv))
