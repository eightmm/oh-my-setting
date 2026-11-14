//! Bounded derived projections; cached observations are never write authority.
use crate::{Error, Result, Value, require};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
#[derive(Clone)]
struct Entry {
    fingerprint: String,
    at: Instant,
    value: Value,
}
pub struct Cache {
    maximum: usize,
    ttl: Duration,
    entries: Mutex<BTreeMap<String, Entry>>,
}
impl Cache {
    pub fn new(maximum: usize, ttl: Duration) -> Result<Self> {
        require(
            maximum > 0 && maximum <= 1024 && !ttl.is_zero(),
            "invalid panel cache limits",
        )?;
        Ok(Self {
            maximum,
            ttl,
            entries: Mutex::new(BTreeMap::new()),
        })
    }
    pub fn read(&self, key: &str, fingerprint: &str) -> Option<Value> {
        let mut entries = self.entries.lock().ok()?;
        if entries
            .get(key)
            .is_some_and(|v| v.fingerprint == fingerprint && v.at.elapsed() <= self.ttl)
        {
            entries.get(key).map(|v| v.value.clone())
        } else {
            entries.remove(key);
            None
        }
    }
    pub fn insert(&self, key: String, fingerprint: String, value: Value) -> Result<()> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| Error::new("panel cache unavailable"))?;
        if entries.len() >= self.maximum
            && !entries.contains_key(&key)
            && let Some(old) = entries
                .iter()
                .min_by_key(|(_, v)| v.at)
                .map(|(k, _)| k.clone())
        {
            entries.remove(&old);
        }
        entries.insert(
            key,
            Entry {
                fingerprint,
                at: Instant::now(),
                value,
            },
        );
        Ok(())
    }
    pub fn clear(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.clear();
        }
    }
}
/// One collection turn shares source reads without persisting stale authority.
#[derive(Default)]
pub struct TickReads {
    values: BTreeMap<String, Value>,
}
impl TickReads {
    pub fn read(&mut self, name: &str, producer: impl FnOnce() -> Result<Value>) -> Result<Value> {
        if let Some(value) = self.values.get(name) {
            return Ok(value.clone());
        }
        let value = producer()?;
        self.values.insert(name.into(), value.clone());
        Ok(value)
    }
}
pub fn fingerprint(parts: &[(&str, &[u8])]) -> String {
    let mut framed = Vec::new();
    for (name, bytes) in parts {
        framed.extend_from_slice(&(name.len() as u64).to_be_bytes());
        framed.extend_from_slice(name.as_bytes());
        framed.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        framed.extend_from_slice(bytes);
    }
    crate::digest(&framed)
}
