use crate::{Error, Integer, PyString, Value};
use std::collections::{BTreeMap, btree_map};
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
