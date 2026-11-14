# oms-json

Lossless decimal integers and Python string code points for OMS JSON values,
with iterative parsing, serialization, cloning and structural comparison.

`parse_strict(&[u8])` rejects duplicate object keys, nonfinite numbers, lone
surrogate escapes and invalid UTF-8. `parse_python(&[u8])` permits `NaN`,
`Infinity`, `-Infinity`, lone surrogate escapes, and duplicate keys (last value
wins). Both decode valid UTF-16 surrogate pairs into a single code point and
normalize integer `-0` to `0`, preserving float `-0.0`.

`parse_with_options` accepts `ParseOptions::strict()` or
`ParseOptions::python_compat()` with configurable `allow_nonfinite`,
`allow_duplicate_keys`, `allow_lone_surrogates`, `max_bytes`, `max_depth` and
`max_integer_digits`. Defaults are 64 MiB, 1024 nested containers and 4300
integer digits excluding the sign. Zero digits disables the digit limit while
the byte limit still applies. Zero depth permits scalar roots only; zero bytes
permits no JSON document. All containers count toward depth, including empty
ones. Limits include trailing whitespace in the byte count. Parse failures use
`ParseError`, separate from the existing serialization `Error`.

Integers preserve exact decimal text, including values outside 64-bit ranges.
Tokens containing a decimal point or exponent become `Float(f64)`; float
spelling and decimal precision are subject to binary64 conversion. Strict mode
rejects float overflow; Python mode permits infinity. Integer limits do not
apply to float tokens. Parsing has no file writes or external state.

`Integer` provides `as_decimal`, `as_i64`, `as_u64`, `is_negative`, and
`digit_len`. `PyString::to_str_checked` rejects lone surrogates;
`to_utf8_string_lossy` replaces each surrogate with `?`, matching
`utf8_with_replacement`.

`Value` provides `get`, `as_object`, `as_object_mut`, `as_array`, `as_array_mut`,
`as_bool`, `as_integer`, `as_string`, and `as_float`. `as_float` reads only the
float variant. `as_f64` and `as_number_f64` explicitly convert either numeric
variant; integer conversion can round or overflow to infinity without changing
the stored integer. Other variants return `None`.

`clone_bounded` returns `Error::DepthLimit` beyond 1024 nested containers and
otherwise preserves all values, including nonfinite floats, long integers and
surrogates. `Value::values_equal` (also available as `oms_json::values_equal`)
compares structures iteratively and returns false beyond that depth. Numeric
variants remain distinct; floats use IEEE equality, so NaN differs from itself
and positive and negative zero compare equal. These operations use memory
proportional to nesting for traversal; cloned output additionally owns its data.
Existing serializers retain their original limits and encoding policies.
