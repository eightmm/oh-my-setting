use crate::{Error, Integer, OrderedObject, OrderedValue, PyString, Value};
use std::collections::{BTreeMap, HashMap, btree_map};
use std::slice;

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object()?.get(&PyString::from(key))
    }

    pub fn as_object(&self) -> Option<&BTreeMap<PyString, Value>> {
        match self {
            Self::Object(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_object_mut(&mut self) -> Option<&mut BTreeMap<PyString, Value>> {
        match self {
            Self::Object(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_integer(&self) -> Option<&Integer> {
        match self {
            Self::Integer(value) => Some(value),
            _ => None,
        }
    }

    /// Read a float without converting an exact integer.
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(value) => Some(*value),
            _ => None,
        }
    }

    /// Explicit numeric conversion; integers may round or overflow to infinity.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Integer(value) => value.as_decimal().parse().ok(),
            _ => self.as_float(),
        }
    }

    /// Explicit numeric conversion, with the same semantics as `as_f64`.
    pub fn as_number_f64(&self) -> Option<f64> {
        self.as_f64()
    }

    pub fn as_string(&self) -> Option<&PyString> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    /// Clone without recursive calls, rejecting more than 1024 nested containers.
    /// Integer digit counts and Python string code points are preserved.
    pub fn clone_bounded(&self) -> Result<Value, Error> {
        enum Frame<'a> {
            Node(&'a Value, usize),
            Array(slice::Iter<'a, Value>, Vec<Value>, usize),
            Object(
                btree_map::Iter<'a, PyString, Value>,
                BTreeMap<PyString, Value>,
                Option<PyString>,
                usize,
            ),
        }
        let mut stack = vec![Frame::Node(self, 0)];
        let mut result = None;
        while let Some(frame) = stack.pop() {
            match frame {
                Frame::Node(value, depth) => {
                    result = Some(match value {
                        Self::Null => Self::Null,
                        Self::Bool(value) => Self::Bool(*value),
                        Self::Integer(value) => Self::Integer(value.clone()),
                        Self::Float(value) => Self::Float(*value),
                        Self::String(value) => Self::String(value.clone()),
                        Self::Array(items) => {
                            if depth >= 1024 {
                                return Err(Error::DepthLimit);
                            }
                            stack.push(Frame::Array(items.iter(), Vec::new(), depth));
                            continue;
                        }
                        Self::Object(items) => {
                            if depth >= 1024 {
                                return Err(Error::DepthLimit);
                            }
                            stack.push(Frame::Object(items.iter(), BTreeMap::new(), None, depth));
                            continue;
                        }
                    });
                }
                Frame::Array(mut items, mut cloned, depth) => {
                    if let Some(value) = result.take() {
                        cloned.push(value);
                    }
                    if let Some(value) = items.next() {
                        stack.push(Frame::Array(items, cloned, depth));
                        stack.push(Frame::Node(value, depth + 1));
                    } else {
                        result = Some(Self::Array(cloned));
                    }
                }
                Frame::Object(mut items, mut cloned, previous, depth) => {
                    if let Some(key) = previous {
                        cloned.insert(key, result.take().expect("completed child"));
                    }
                    if let Some((key, value)) = items.next() {
                        stack.push(Frame::Object(items, cloned, Some(key.clone()), depth));
                        stack.push(Frame::Node(value, depth + 1));
                    } else {
                        result = Some(Self::Object(cloned));
                    }
                }
            }
        }
        Ok(result.expect("completed root"))
    }

    /// Structural equality, with distinct integer/float variants and IEEE float
    /// equality (`NaN` differs from itself). Returns false beyond 1024 containers.
    pub fn values_equal(a: &Value, b: &Value) -> bool {
        enum Frame<'a> {
            Node(&'a Value, &'a Value, usize),
            Array(slice::Iter<'a, Value>, slice::Iter<'a, Value>, usize),
            Object(
                btree_map::Iter<'a, PyString, Value>,
                btree_map::Iter<'a, PyString, Value>,
                usize,
            ),
        }
        let mut stack = vec![Frame::Node(a, b, 0)];
        while let Some(frame) = stack.pop() {
            match frame {
                Frame::Node(a, b, depth) => match (a, b) {
                    (Self::Null, Self::Null) => {}
                    (Self::Bool(a), Self::Bool(b)) if a == b => {}
                    (Self::Integer(a), Self::Integer(b)) if a == b => {}
                    (Self::Float(a), Self::Float(b)) if a == b => {}
                    (Self::String(a), Self::String(b)) if a == b => {}
                    (Self::Array(a), Self::Array(b)) if a.len() == b.len() && depth < 1024 => {
                        stack.push(Frame::Array(a.iter(), b.iter(), depth));
                    }
                    (Self::Object(a), Self::Object(b)) if a.len() == b.len() && depth < 1024 => {
                        stack.push(Frame::Object(a.iter(), b.iter(), depth));
                    }
                    _ => return false,
                },
                Frame::Array(mut a, mut b, depth) => {
                    if let (Some(left), Some(right)) = (a.next(), b.next()) {
                        stack.push(Frame::Array(a, b, depth));
                        stack.push(Frame::Node(left, right, depth + 1));
                    }
                }
                Frame::Object(mut a, mut b, depth) => {
                    if let (Some((ka, va)), Some((kb, vb))) = (a.next(), b.next()) {
                        if ka != kb {
                            return false;
                        }
                        stack.push(Frame::Object(a, b, depth));
                        stack.push(Frame::Node(va, vb, depth + 1));
                    }
                }
            }
        }
        true
    }
}

/// See `Value::values_equal` for depth and numeric equality semantics.
pub fn values_equal(a: &Value, b: &Value) -> bool {
    Value::values_equal(a, b)
}

/// See `OrderedValue::values_equal` for depth, order, and numeric equality semantics.
pub fn ordered_values_equal(a: &OrderedValue, b: &OrderedValue) -> bool {
    OrderedValue::values_equal(a, b)
}

impl OrderedObject {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            index: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> slice::Iter<'_, (PyString, OrderedValue)> {
        self.entries.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&PyString, &mut OrderedValue)> {
        self.entries.iter_mut().map(|(key, value)| (&*key, value))
    }

    pub fn as_slice(&self) -> &[(PyString, OrderedValue)] {
        &self.entries
    }

    pub fn into_vec(mut self) -> Vec<(PyString, OrderedValue)> {
        self.index.clear();
        std::mem::take(&mut self.entries)
    }

    pub fn contains_key(&self, key: &PyString) -> bool {
        self.index.contains_key(key)
    }

    pub fn get(&self, key: &str) -> Option<&OrderedValue> {
        self.get_key(&PyString::from(key))
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut OrderedValue> {
        self.get_key_mut(&PyString::from(key))
    }

    pub fn get_key(&self, key: &PyString) -> Option<&OrderedValue> {
        let &idx = self.index.get(key)?;
        Some(&self.entries[idx].1)
    }

    pub fn get_key_mut(&mut self, key: &PyString) -> Option<&mut OrderedValue> {
        let &idx = self.index.get(key)?;
        Some(&mut self.entries[idx].1)
    }

    /// Insert or replace in place at the original insertion index, matching Python `dict`.
    pub fn insert(&mut self, key: PyString, value: OrderedValue) -> Option<OrderedValue> {
        if let Some(&idx) = self.index.get(&key) {
            return Some(std::mem::replace(&mut self.entries[idx].1, value));
        }
        let idx = self.entries.len();
        self.index.insert(key.clone(), idx);
        self.entries.push((key, value));
        None
    }
}

impl IntoIterator for OrderedObject {
    type Item = (PyString, OrderedValue);
    type IntoIter = std::vec::IntoIter<(PyString, OrderedValue)>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_vec().into_iter()
    }
}

impl OrderedValue {
    pub fn get(&self, key: &str) -> Option<&OrderedValue> {
        self.as_object()?.get(key)
    }

    pub fn as_object(&self) -> Option<&OrderedObject> {
        match self {
            Self::Object(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_object_mut(&mut self) -> Option<&mut OrderedObject> {
        match self {
            Self::Object(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[OrderedValue]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_array_mut(&mut self) -> Option<&mut Vec<OrderedValue>> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_integer(&self) -> Option<&Integer> {
        match self {
            Self::Integer(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Integer(value) => value.as_decimal().parse().ok(),
            _ => self.as_float(),
        }
    }

    pub fn as_number_f64(&self) -> Option<f64> {
        self.as_f64()
    }

    pub fn as_string(&self) -> Option<&PyString> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    /// Clone iteratively, rejecting more than 1024 nested containers.
    pub fn clone_bounded(&self) -> Result<OrderedValue, Error> {
        enum Frame<'a> {
            Node(&'a OrderedValue, usize),
            Array(slice::Iter<'a, OrderedValue>, Vec<OrderedValue>, usize),
            Object(
                slice::Iter<'a, (PyString, OrderedValue)>,
                OrderedObject,
                Option<PyString>,
                usize,
            ),
        }
        let mut stack = vec![Frame::Node(self, 0)];
        let mut result = None;
        while let Some(frame) = stack.pop() {
            match frame {
                Frame::Node(value, depth) => {
                    result = Some(match value {
                        Self::Null => Self::Null,
                        Self::Bool(value) => Self::Bool(*value),
                        Self::Integer(value) => Self::Integer(value.clone()),
                        Self::Float(value) => Self::Float(*value),
                        Self::String(value) => Self::String(value.clone()),
                        Self::Array(items) => {
                            if depth >= 1024 {
                                return Err(Error::DepthLimit);
                            }
                            stack.push(Frame::Array(items.iter(), Vec::new(), depth));
                            continue;
                        }
                        Self::Object(items) => {
                            if depth >= 1024 {
                                return Err(Error::DepthLimit);
                            }
                            stack.push(Frame::Object(
                                items.iter(),
                                OrderedObject::new(),
                                None,
                                depth,
                            ));
                            continue;
                        }
                    });
                }
                Frame::Array(mut items, mut cloned, depth) => {
                    if let Some(value) = result.take() {
                        cloned.push(value);
                    }
                    if let Some(value) = items.next() {
                        stack.push(Frame::Array(items, cloned, depth));
                        stack.push(Frame::Node(value, depth + 1));
                    } else {
                        result = Some(Self::Array(cloned));
                    }
                }
                Frame::Object(mut items, mut cloned, previous, depth) => {
                    if let Some(key) = previous {
                        cloned.insert(key, result.take().expect("completed child"));
                    }
                    if let Some((key, value)) = items.next() {
                        stack.push(Frame::Object(items, cloned, Some(key.clone()), depth));
                        stack.push(Frame::Node(value, depth + 1));
                    } else {
                        result = Some(Self::Object(cloned));
                    }
                }
            }
        }
        Ok(result.expect("completed root"))
    }

    /// Iteratively convert into key-sorted `Value` without cloning scalars or keys.
    pub fn into_value(self) -> Result<Value, Error> {
        enum Frame {
            Node(OrderedValue, usize),
            Array(std::vec::IntoIter<OrderedValue>, Vec<Value>, usize),
            Object(
                std::vec::IntoIter<(PyString, OrderedValue)>,
                BTreeMap<PyString, Value>,
                Option<PyString>,
                usize,
            ),
        }
        let mut stack = vec![Frame::Node(self, 0)];
        let mut result = None;
        while let Some(frame) = stack.pop() {
            match frame {
                Frame::Node(mut value, depth) => match &mut value {
                    OrderedValue::Null => result = Some(Value::Null),
                    OrderedValue::Bool(b) => result = Some(Value::Bool(*b)),
                    OrderedValue::Integer(n) => {
                        result = Some(Value::Integer(Integer(std::mem::take(&mut n.0))));
                    }
                    OrderedValue::Float(f) => result = Some(Value::Float(*f)),
                    OrderedValue::String(s) => {
                        result = Some(Value::String(PyString(std::mem::take(&mut s.0))));
                    }
                    OrderedValue::Array(items) => {
                        if depth >= 1024 {
                            return Err(Error::DepthLimit);
                        }
                        let items = std::mem::take(items);
                        let cap = items.len();
                        stack.push(Frame::Array(
                            items.into_iter(),
                            Vec::with_capacity(cap),
                            depth,
                        ));
                    }
                    OrderedValue::Object(obj) => {
                        if depth >= 1024 {
                            return Err(Error::DepthLimit);
                        }
                        obj.index.clear();
                        let entries = std::mem::take(&mut obj.entries);
                        stack.push(Frame::Object(
                            entries.into_iter(),
                            BTreeMap::new(),
                            None,
                            depth,
                        ));
                    }
                },
                Frame::Array(mut items, mut built, depth) => {
                    if let Some(value) = result.take() {
                        built.push(value);
                    }
                    if let Some(item) = items.next() {
                        stack.push(Frame::Array(items, built, depth));
                        stack.push(Frame::Node(item, depth + 1));
                    } else {
                        result = Some(Value::Array(built));
                    }
                }
                Frame::Object(mut items, mut built, previous, depth) => {
                    if let Some(key) = previous {
                        built.insert(key, result.take().expect("completed child"));
                    }
                    if let Some((key, value)) = items.next() {
                        stack.push(Frame::Object(items, built, Some(key), depth));
                        stack.push(Frame::Node(value, depth + 1));
                    } else {
                        result = Some(Value::Object(built));
                    }
                }
            }
        }
        Ok(result.expect("completed root"))
    }

    /// Structural equality preserving object insertion order. Returns false beyond 1024 containers.
    pub fn values_equal(a: &OrderedValue, b: &OrderedValue) -> bool {
        enum Frame<'a> {
            Node(&'a OrderedValue, &'a OrderedValue, usize),
            Array(
                slice::Iter<'a, OrderedValue>,
                slice::Iter<'a, OrderedValue>,
                usize,
            ),
            Object(
                slice::Iter<'a, (PyString, OrderedValue)>,
                slice::Iter<'a, (PyString, OrderedValue)>,
                usize,
            ),
        }
        let mut stack = vec![Frame::Node(a, b, 0)];
        while let Some(frame) = stack.pop() {
            match frame {
                Frame::Node(a, b, depth) => match (a, b) {
                    (Self::Null, Self::Null) => {}
                    (Self::Bool(a), Self::Bool(b)) if a == b => {}
                    (Self::Integer(a), Self::Integer(b)) if a == b => {}
                    (Self::Float(a), Self::Float(b)) if a == b => {}
                    (Self::String(a), Self::String(b)) if a == b => {}
                    (Self::Array(a), Self::Array(b)) if a.len() == b.len() && depth < 1024 => {
                        stack.push(Frame::Array(a.iter(), b.iter(), depth));
                    }
                    (Self::Object(a), Self::Object(b)) if a.len() == b.len() && depth < 1024 => {
                        stack.push(Frame::Object(a.iter(), b.iter(), depth));
                    }
                    _ => return false,
                },
                Frame::Array(mut a, mut b, depth) => {
                    if let (Some(left), Some(right)) = (a.next(), b.next()) {
                        stack.push(Frame::Array(a, b, depth));
                        stack.push(Frame::Node(left, right, depth + 1));
                    }
                }
                Frame::Object(mut a, mut b, depth) => {
                    if let (Some((ka, va)), Some((kb, vb))) = (a.next(), b.next()) {
                        if ka != kb {
                            return false;
                        }
                        stack.push(Frame::Object(a, b, depth));
                        stack.push(Frame::Node(va, vb, depth + 1));
                    }
                }
            }
        }
        true
    }
}
