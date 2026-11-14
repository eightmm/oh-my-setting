# oms-toml

A standalone TOML 1.0 parser for OMS consumers, using only Rust's standard
library. The crate has its own workspace and requires Rust 1.99.1.

`parse_str(&str)` and `parse_bytes(&[u8])` return `Result<Table, Error>` and
accept at most 16 MiB. `parse_bounded(&[u8], max_bytes)` selects an inclusive
byte limit; zero permits empty input only. All entry points reject invalid
UTF-8 and cap nesting at 256 levels, counting dotted keys, headers and nested
values together. Bounds intentionally reject otherwise valid oversized or
deep TOML documents. Byte-limit errors point to line 1, column 1. Other errors
carry a 1-based Unicode character line and column at the detected problem.

`Table` is `BTreeMap<String, Value>`. `Value` contains `String`, `Integer(i64)`,
`Float(f64)`, `Boolean(bool)`, `Datetime(String)`, `Array(Vec<Value>)`, and
`Table(Table)`. Getters `as_table`, `as_array`, `as_str`, `as_integer`,
`as_float`, `as_bool`, and `get` return `Option` without coercion. `Error`
contains public `line`, `column`, and `message` fields and implements `Display`
and `std::error::Error`.

Dates and times use ISO 8601 strings corresponding to Python 3.11 `tomllib`
values: `T` separates dates and times; UTC is `+00:00`; nonzero fractional
seconds have six digits, truncating excess precision. Zero fractional seconds
are omitted. CRLF is normalized to LF, including inside strings. Integers are
restricted to signed 64-bit range; floats include signed infinity and NaN.

The parser handles quoted and dotted keys, all four TOML string forms,
Unicode escapes, multiline continuations, arrays, inline tables, table headers,
and arrays of tables. It enforces duplicate declarations and inline collection
immutability. Inline tables reject newlines and comments between entries;
newlines inside an array or multiline string value are permitted by TOML 1.0.
Parsing has no filesystem, process, environment or shared-state side effects;
concurrent calls are independent and interrupted calls leave no written state.

Compile and formatting checks:

```sh
cargo +1.99.1 check --manifest-path rust/crates/oms-toml/Cargo.toml --all-targets --offline
cargo +1.99.1 fmt --manifest-path rust/crates/oms-toml/Cargo.toml -- --check
```

Unit cases cover representative `config.toml`, `pyproject.toml`, and `Cargo.toml`
documents, ownership conflicts, string escapes, numeric and calendar validation,
UTF-8, CRLF error positions, and byte/depth boundaries. These cases are compiled
by the check above; behavioral execution is a separate validation step.
