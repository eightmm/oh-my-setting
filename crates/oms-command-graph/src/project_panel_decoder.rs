//! Large indexes are validated in one pass. Only fields consumed by the existing
//! graph and declaration verifier survive. Small indexes use their original decoder.
use super::{MAX_INDEX_EDGES, MAX_INDEX_NODES};
use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use std::{
    cell::Cell,
    fmt,
    sync::atomic::{AtomicBool, Ordering},
};

pub(super) const LEGACY_BYTES: usize = 16 << 20;
pub(super) const MALFORMED: &str = "project graph index is unreadable or malformed";
const BUDGET: &str = "Cached graph exceeds the panel read budget.";
// Separate from the raw input cap and row counts. Limit the strings passed to
// Graph::new and, independently, the corpus it expands into term postings.
const SEARCH_BYTES: usize = 4 << 20;
const FIELD_BYTES: usize = 4096;
const MAX_VALUES: usize = 3_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Root,
    Nodes,
    Edges,
    Node,
    Edge,
    Metadata,
    Text,
    SearchText,
    Number,
    Bool,
    Skip,
    OverLimit,
}

// Unknown member names never become owned keys in the projection.
impl<'de> DeserializeSeed<'de> for Field {
    type Value = Option<(&'static str, Field)>;
    fn deserialize<D: serde::Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        de.deserialize_identifier(self)
    }
}
impl Visitor<'_> for Field {
    type Value = Option<(&'static str, Field)>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a graph field name")
    }
    fn visit_str<E>(self, key: &str) -> Result<Self::Value, E> {
        use Field::*;
        let fields: &[(&'static str, Field)] = match self {
            Root => &[
                ("schema", Number),
                ("revision", Text),
                ("nodes", Nodes),
                ("edges", Edges),
            ],
            Node => &[
                ("id", Text),
                ("name", SearchText),
                ("kind", Text),
                ("path", SearchText),
                ("language", Text),
                ("source_digest", Text),
                ("summary", SearchText),
                ("metadata", Metadata),
            ],
            Metadata => &[
                ("qualname", SearchText),
                ("signature", Text),
                ("line", Number),
                ("end_line", Number),
                ("span_complete", Bool),
                ("scope_truncated", Bool),
                ("signature_truncated", Bool),
            ],
            Edge => &[
                ("source", Text),
                ("target", Text),
                ("relation", Text),
                ("confidence", Text),
            ],
            _ => &[],
        };
        Ok(fields.iter().copied().find(|(name, _)| *name == key))
    }
}

struct Limits<'a> {
    cancel: &'a AtomicBool,
    values: Cell<usize>,
    strings: Cell<usize>,
    search: Cell<usize>,
    exceeded: Cell<bool>,
}

#[derive(Clone, Copy)]
struct Project<'a> {
    field: Field,
    limits: &'a Limits<'a>,
}

impl Project<'_> {
    fn child(self, field: Field) -> Self {
        Self { field, ..self }
    }

    fn over<E: serde::de::Error>(self) -> E {
        self.limits.exceeded.set(true);
        E::custom(BUDGET)
    }

    // Preserve an explicitly wrong verifier field as unequal even when the
    // producer omitted the field. Never store the supplied container or text.
    fn mismatched(self) -> Value {
        match self.field {
            Field::Bool | Field::Number | Field::Text | Field::SearchText => {
                Value::Array(Vec::new())
            }
            _ => Value::Null,
        }
    }
}

impl<'de> DeserializeSeed<'de> for Project<'_> {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(self, de: D) -> Result<Value, D::Error> {
        let count = self.limits.values.get() + 1;
        if count > MAX_VALUES || self.field == Field::OverLimit {
            return Err(self.over());
        }
        self.limits.values.set(count);
        if self.limits.cancel.load(Ordering::Relaxed) {
            return Err(D::Error::custom("interrupted"));
        }
        de.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Project<'_> {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a project graph JSON value")
    }
    fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Value, E> {
        if !matches!(self.field, Field::Text | Field::SearchText) {
            return Ok(self.mismatched());
        }
        let total = self.limits.strings.get() + s.len();
        if s.len() > FIELD_BYTES || total > LEGACY_BYTES {
            return Err(self.over());
        }
        if self.field == Field::SearchText {
            let search = self.limits.search.get() + s.len();
            if search > SEARCH_BYTES {
                return Err(self.over());
            }
            self.limits.search.set(search);
        }
        self.limits.strings.set(total);
        Ok(Value::String(s.into()))
    }
    fn visit_bool<E>(self, b: bool) -> Result<Value, E> {
        Ok(if self.field == Field::Bool {
            b.into()
        } else {
            self.mismatched()
        })
    }
    fn visit_u64<E>(self, n: u64) -> Result<Value, E> {
        Ok(if self.field == Field::Number {
            n.into()
        } else {
            self.mismatched()
        })
    }
    fn visit_i64<E>(self, _: i64) -> Result<Value, E> {
        Ok(self.mismatched())
    }
    fn visit_f64<E>(self, _: f64) -> Result<Value, E> {
        Ok(self.mismatched())
    }
    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(self.mismatched())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut fields = Map::new();
        while let Some(field) = map.next_key_seed(self.field)? {
            if let Some((key, kind)) = field {
                if fields.contains_key(key) {
                    return Err(A::Error::custom("duplicate graph field"));
                }
                fields.insert(key.into(), map.next_value_seed(self.child(kind))?);
            } else {
                map.next_value_seed(self.child(Field::Skip))?;
            }
        }
        Ok(match self.field {
            Field::Root | Field::Node | Field::Edge | Field::Metadata => Value::Object(fields),
            _ => self.mismatched(),
        })
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let (item, max) = match self.field {
            Field::Nodes => (Field::Node, MAX_INDEX_NODES),
            Field::Edges => (Field::Edge, MAX_INDEX_EDGES),
            _ => {
                while seq.next_element_seed(self.child(Field::Skip))?.is_some() {}
                return Ok(self.mismatched());
            }
        };
        let mut rows = Vec::new();
        loop {
            let kind = if rows.len() >= max {
                Field::OverLimit
            } else {
                item
            };
            match seq.next_element_seed(self.child(kind))? {
                Some(row) => rows.push(row),
                None => return Ok(Value::Array(rows)),
            }
        }
    }
}

pub(super) fn large_index(input: &[u8], cancel: &AtomicBool) -> Result<Value, &'static str> {
    let limits = Limits {
        cancel,
        values: Cell::new(0),
        strings: Cell::new(0),
        search: Cell::new(0),
        exceeded: Cell::new(false),
    };
    let mut de = serde_json::Deserializer::from_slice(input);
    Project {
        field: Field::Root,
        limits: &limits,
    }
    .deserialize(&mut de)
    .and_then(|value| de.end().map(|()| value))
    .map_err(|_| {
        if limits.exceeded.get() {
            BUDGET
        } else {
            MALFORMED
        }
    })
}
