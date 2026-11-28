//! Adapter contract (`trait Source`) and compiled adapter registry for `oms intake`.

use crate::crypto::SecretStore;
use crate::linear::LinearSource;
use crate::model::WorkRequest;
use serde_json::{Map, Value};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnvVarSpec {
    pub name: &'static str,
    pub required: bool,
    pub sensitive: bool,
    pub description: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BindingFlagSpec {
    pub flag: &'static str,
    pub value_name: &'static str,
    pub required: bool,
    pub description: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct RequestHeaders {
    entries: Vec<(String, String)>,
}

impl RequestHeaders {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, name: String, value: String) {
        self.entries.push((name, value));
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn count(&self, name: &str) -> usize {
        self.entries
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(name))
            .count()
    }
}

pub struct SignatureInput<'a> {
    pub secret_env: &'static str,
    pub message: Cow<'a, [u8]>,
    pub signature_hex: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingDecision {
    Matched,
    Ignore,
    Forbidden,
    BadRequest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PayloadDecision {
    Work(WorkRequest),
    Ignore,
    BadRequest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoomDelivery {
    AppendedNew,
    Duplicate,
    Refused,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboundAck {
    pub delivery_id: String,
    pub session_ref: String,
    pub kind: String,
    pub body: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboundReply {
    pub delivery_id: String,
    pub session_ref: String,
    pub kind: String,
    pub body: String,
    pub reply_to: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreateDeliveryOutcome {
    Created,
    AlreadyExists,
}

#[derive(Clone, Debug)]
pub enum TransportError {
    OutcomeUnknown(String),
    Rejected(String),
}

impl TransportError {
    pub fn message(&self) -> &str {
        match self {
            Self::OutcomeUnknown(m) | Self::Rejected(m) => m,
        }
    }
}

/// Source-specific webhook and outbound reply adapter over the shared `oms intake` core.
pub trait Source: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn serve_env(&self) -> &'static [EnvVarSpec];
    fn reply_env(&self) -> &'static [EnvVarSpec];
    fn binding_flags(&self) -> &'static [BindingFlagSpec];
    fn reply_kinds(&self) -> &'static [&'static str];
    fn signature_header(&self) -> &'static str;

    fn validate_binding(&self, flags: &BTreeMap<String, String>) -> Result<(), String>;

    fn validate_endpoint(&self) -> Result<(), String> {
        Ok(())
    }

    fn signature_input<'a>(
        &self,
        headers: &'a RequestHeaders,
        raw_body: &'a [u8],
    ) -> Option<SignatureInput<'a>>;

    fn observed_session_ref(&self, payload: &Map<String, Value>) -> Option<String>;

    fn extract_timestamp_ms(
        &self,
        headers: &RequestHeaders,
        payload: &Map<String, Value>,
    ) -> Option<i64>;

    fn check_binding(
        &self,
        flags: &BTreeMap<String, String>,
        payload: &Map<String, Value>,
    ) -> BindingDecision;

    fn parse_work_request(&self, payload: &Map<String, Value>) -> PayloadDecision;

    /// Team keys or ids of a verified payload, most specific first, for route-table lookup.
    fn routing_keys(&self, _payload: &Map<String, Value>) -> Vec<String> {
        Vec::new()
    }

    /// Fixed reply for a request that no route accepts; nothing is written to any Room.
    fn unrouted_ack(&self, _req: &WorkRequest) -> Option<OutboundAck> {
        None
    }

    fn first_ack(
        &self,
        req: &WorkRequest,
        room_id: &str,
        delivery: RoomDelivery,
    ) -> Option<OutboundAck>;

    fn deliver_ack(
        &self,
        secrets: &SecretStore,
        ack: &OutboundAck,
        timeout: Duration,
        interrupted: &AtomicBool,
    ) -> Result<(), TransportError>;

    fn query_delivery(
        &self,
        secrets: &SecretStore,
        delivery_id: &str,
        session_ref: &str,
        timeout: Duration,
        interrupted: &AtomicBool,
    ) -> Result<bool, TransportError>;

    fn create_delivery(
        &self,
        secrets: &SecretStore,
        reply: &OutboundReply,
        timeout: Duration,
        interrupted: &AtomicBool,
    ) -> Result<CreateDeliveryOutcome, TransportError>;
}

type SourceFactory = fn() -> Arc<dyn Source>;

static SOURCE_FACTORIES: &[SourceFactory] = &[|| Arc::new(LinearSource::new())];

pub fn compiled_sources() -> Vec<Arc<dyn Source>> {
    SOURCE_FACTORIES.iter().map(|make| make()).collect()
}

pub fn find_source_arc(name: &str) -> Option<Arc<dyn Source>> {
    SOURCE_FACTORIES
        .iter()
        .map(|make| make())
        .find(|s| s.name() == name)
}
