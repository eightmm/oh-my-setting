//! Insertion-ordered Python JSON for journal inputs and payload construction.
//!
//! Every persisted/API input uses python_compat, matching Python json.loads:
//! duplicate keys replace in place; nonfinite values and surrogate code points
//! survive parsing. Schema sanitization removes nonfinite metrics, and canonical
//! writers reject nonfinite values/surrogates instead of changing their value.

pub use oms_json::{OrderedObject as Map, OrderedValue as Value};
use oms_json::{ParseError, PyString};

pub fn from_slice(bytes: &[u8]) -> Result<Value, ParseError> {
    oms_json::parse_python_ordered(bytes)
}
pub fn from_str(text: &str) -> Result<Value, ParseError> {
    from_slice(text.as_bytes())
}

static NULL: Value = Value::Null;

pub trait JsonExt {
    fn pointer(&self, path: &str) -> Option<&Value>;
    fn as_str(&self) -> Option<String>;
    fn as_i64(&self) -> Option<i64>;
    fn as_u64(&self) -> Option<u64>;
    fn is_null(&self) -> bool;
    fn is_object(&self) -> bool;
    fn is_string(&self) -> bool;
    fn is_number(&self) -> bool;
    fn at(&self, key: &str) -> &Value;
    fn at_mut(&mut self, key: &str) -> &mut Value;
    fn copy(&self) -> Self;
    fn to_string(&self) -> String;
}
impl JsonExt for Value {
    fn pointer(&self, path: &str) -> Option<&Value> {
        let mut node = self;
        for key in path.strip_prefix('/')?.split('/') {
            node = node.get(key)?;
        }
        Some(node)
    }
    fn as_str(&self) -> Option<String> {
        self.as_string()?.to_str_checked()
    }
    fn as_i64(&self) -> Option<i64> {
        self.as_integer()?.as_i64()
    }
    fn as_u64(&self) -> Option<u64> {
        self.as_integer()?.as_u64()
    }
    fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
    fn is_object(&self) -> bool {
        matches!(self, Self::Object(_))
    }
    fn is_string(&self) -> bool {
        matches!(self, Self::String(_))
    }
    fn is_number(&self) -> bool {
        matches!(self, Self::Integer(_) | Self::Float(_))
    }
    fn at(&self, key: &str) -> &Value {
        self.get(key).unwrap_or(&NULL)
    }
    fn at_mut(&mut self, key: &str) -> &mut Value {
        if self.is_null() {
            *self = Self::Object(Map::new());
        }
        let obj = self.as_object_mut().expect("JSON object");
        if obj.get(key).is_none() {
            obj.insert(key.into(), Self::Null);
        }
        obj.get_mut(key).expect("inserted key")
    }
    fn copy(&self) -> Self {
        self.clone_bounded()
            .expect("journal JSON depth is bounded at ingestion")
    }
    fn to_string(&self) -> String {
        crate::sanitize::canonical_json_string(self)
            .unwrap_or_else(|e| format!("<invalid JSON: {e}>"))
    }
}
pub trait MapExt {
    fn at(&self, key: &str) -> &Value;
    fn values(&self) -> impl Iterator<Item = &Value>;
    fn copy(&self) -> Self;
    fn remove(&mut self, key: &str) -> Option<Value>;
    fn has_key(&self, key: &str) -> bool;
}
impl MapExt for Map {
    fn at(&self, key: &str) -> &Value {
        self.get(key).unwrap_or(&NULL)
    }
    fn values(&self) -> impl Iterator<Item = &Value> {
        self.iter().map(|(_, v)| v)
    }
    fn copy(&self) -> Self {
        let mut out = Map::new();
        for (k, v) in self.iter() {
            out.insert(k.clone(), v.copy());
        }
        out
    }
    fn remove(&mut self, key: &str) -> Option<Value> {
        let mut removed = None;
        for (k, v) in std::mem::take(self) {
            if k == PyString::from(key) {
                removed = Some(v);
            } else {
                self.insert(k, v);
            }
        }
        removed
    }
    fn has_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
}

pub fn key(text: &str) -> PyString {
    text.into()
}

pub trait BuildJson {
    fn build_json(&self) -> Value;
}
impl BuildJson for Value {
    fn build_json(&self) -> Value {
        self.copy()
    }
}
impl BuildJson for PyString {
    fn build_json(&self) -> Value {
        Value::String(self.clone())
    }
}
impl BuildJson for str {
    fn build_json(&self) -> Value {
        Value::String(self.into())
    }
}
impl BuildJson for String {
    fn build_json(&self) -> Value {
        self.as_str().build_json()
    }
}
impl<T: BuildJson + ?Sized> BuildJson for &T {
    fn build_json(&self) -> Value {
        (**self).build_json()
    }
}
impl BuildJson for bool {
    fn build_json(&self) -> Value {
        Value::Bool(*self)
    }
}
impl<T: BuildJson> BuildJson for Option<T> {
    fn build_json(&self) -> Value {
        self.as_ref().map_or(Value::Null, BuildJson::build_json)
    }
}
impl<T: BuildJson> BuildJson for [T] {
    fn build_json(&self) -> Value {
        Value::Array(self.iter().map(BuildJson::build_json).collect())
    }
}
impl<T: BuildJson> BuildJson for Vec<T> {
    fn build_json(&self) -> Value {
        self.as_slice().build_json()
    }
}
impl BuildJson for Map {
    fn build_json(&self) -> Value {
        Value::Object(self.copy())
    }
}
macro_rules! integers { ($($ty:ty),*) => { $(impl BuildJson for $ty { fn build_json(&self) -> Value { Value::integer(&self.to_string()).expect("native integer") } })* }; }
integers!(i32, i64, u32, u64, usize);
impl BuildJson for f64 {
    fn build_json(&self) -> Value {
        Value::Float(*self)
    }
}

macro_rules! json {
    (@array [$($items:expr,)*]) => { vec![$($items,)*] };
    (@array [$($items:expr,)*] , $($rest:tt)*) => { $crate::json::json!(@array [$($items,)*] $($rest)*) };
    (@array [$($items:expr,)*] null $($rest:tt)*) => { $crate::json::json!(@array [$($items,)* $crate::json::json!(null),] $($rest)*) };
    (@array [$($items:expr,)*] [$($array:tt)*] $($rest:tt)*) => { $crate::json::json!(@array [$($items,)* $crate::json::json!([$($array)*]),] $($rest)*) };
    (@array [$($items:expr,)*] {$($obj:tt)*} $($rest:tt)*) => { $crate::json::json!(@array [$($items,)* $crate::json::json!({$($obj)*}),] $($rest)*) };
    (@array [$($items:expr,)*] $value:expr, $($rest:tt)*) => { $crate::json::json!(@array [$($items,)* $crate::json::json!($value),] $($rest)*) };
    (@array [$($items:expr,)*] $value:expr) => { vec![$($items,)* $crate::json::json!($value)] };
    (@object $obj:ident) => {};
    (@object $obj:ident , $($rest:tt)*) => { $crate::json::json!(@object $obj $($rest)*) };
    (@object $obj:ident $key:tt : null $($rest:tt)*) => { $obj.insert($crate::json::key(&($key)), $crate::json::json!(null)); $crate::json::json!(@object $obj $($rest)*); };
    (@object $obj:ident $key:tt : [$($array:tt)*] $($rest:tt)*) => { $obj.insert($crate::json::key(&($key)), $crate::json::json!([$($array)*])); $crate::json::json!(@object $obj $($rest)*); };
    (@object $obj:ident $key:tt : {$($nested:tt)*} $($rest:tt)*) => { $obj.insert($crate::json::key(&($key)), $crate::json::json!({$($nested)*})); $crate::json::json!(@object $obj $($rest)*); };
    (@object $obj:ident $key:tt : $value:expr, $($rest:tt)*) => { $obj.insert($crate::json::key(&($key)), $crate::json::json!($value)); $crate::json::json!(@object $obj $($rest)*); };
    (@object $obj:ident $key:tt : $value:expr) => { $obj.insert($crate::json::key(&($key)), $crate::json::json!($value)); };
    (null) => { $crate::json::Value::Null };
    ([$($array:tt)*]) => { $crate::json::Value::Array($crate::json::json!(@array [] $($array)*)) };
    ({}) => { $crate::json::Value::Object($crate::json::Map::new()) };
    ({$($obj:tt)*}) => {{ let mut object = $crate::json::Map::new(); $crate::json::json!(@object object $($obj)*); $crate::json::Value::Object(object) }};
    ($value:expr) => { $crate::json::BuildJson::build_json(&$value) };
}
pub(crate) use json;

pub fn object(items: impl IntoIterator<Item = (PyString, Value)>) -> Map {
    let mut out = Map::new();
    for (k, v) in items {
        out.insert(k, v);
    }
    out
}
