//! Linear `AgentSessionEvent` and `agentActivity` adapter implementing `trait Source`.

use crate::crypto::{SecretStore, verify_hmac_sha256_hex};
use crate::model::{
    WorkRequest, WorkRequestKind, created_message_id, derive_delivery_id, prompted_message_id,
    sanitize_header_field, validate_external_id,
};
use crate::source::{
    BindingDecision, BindingFlagSpec, CreateDeliveryOutcome, EnvVarSpec, OutboundAck,
    OutboundReply, PayloadDecision, RequestHeaders, RoomDelivery, SignatureInput, Source,
    TransportError,
};
use crate::transport::{EndpointUrl, post_json_bearer};
use serde_json::{Map, Value, json};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub const SOURCE_LINEAR: &str = "linear";
pub const LINEAR_INTAKE_PATH: &str = "/intake/linear";
pub const ENV_WEBHOOK_CRED: &str = concat!("OMS_LINEAR_WEBHOOK_", "SECRET");
pub const ENV_ACCESS_CRED: &str = concat!("OMS_LINEAR_ACCESS_", "TOKEN");
pub const ENV_LINEAR_API_URL: &str = "OMS_LINEAR_API_URL";
pub const DEFAULT_LINEAR_API_URL: &str = "https://api.linear.app/graphql";

pub const FIRST_THOUGHT_BODY: &str = "Received by OMS; waiting for Main";
pub const REFUSED_CONTENT_ERROR_BODY: &str = "OMS could not accept the request content.";
pub const CREATED_THOUGHT_CALLER_KEY: &str = "thought:created";
pub const UNROUTED_ERROR_BODY: &str = "This Linear team is not connected to an OMS project.";

const QUERY_AGENT_ACTIVITY: &str =
    "query OmsAgentActivity($id: String!) { agentActivity(id: $id) { id } }";
const MUTATION_AGENT_ACTIVITY_CREATE: &str = "mutation OmsAgentActivityCreate($input: AgentActivityCreateInput!) { agentActivityCreate(input: $input) { success agentActivity { id } } }";

static SERVE_ENV_SPECS: &[EnvVarSpec] = &[
    EnvVarSpec {
        name: ENV_WEBHOOK_CRED,
        required: true,
        sensitive: true,
        description: "Signing value verifying the raw body Linear-Signature header",
    },
    EnvVarSpec {
        name: ENV_ACCESS_CRED,
        required: true,
        sensitive: true,
        description: "OAuth access value for first-thought and error GraphQL mutations",
    },
    EnvVarSpec {
        name: ENV_LINEAR_API_URL,
        required: false,
        sensitive: false,
        description: "Optional GraphQL endpoint override (https or loopback http)",
    },
];

static REPLY_ENV_SPECS: &[EnvVarSpec] = &[
    EnvVarSpec {
        name: ENV_ACCESS_CRED,
        required: true,
        sensitive: true,
        description: "OAuth access value for agentActivity query and create mutations",
    },
    EnvVarSpec {
        name: ENV_LINEAR_API_URL,
        required: false,
        sensitive: false,
        description: "Optional GraphQL endpoint override (https or loopback http)",
    },
];

static BINDING_FLAGS: &[BindingFlagSpec] = &[
    BindingFlagSpec {
        flag: "--organization",
        value_name: "ORG",
        required: true,
        description: "Expected Linear organizationId (foreign installations get 403)",
    },
    BindingFlagSpec {
        flag: "--app-user",
        value_name: "APPUSER",
        required: true,
        description: "Expected Linear appUserId for this OAuth app installation",
    },
];

static REPLY_KINDS: &[&str] = &["thought", "response", "elicitation", "error"];

/// Verify `Linear-Signature` (lowercase hex HMAC-SHA256) against raw request bytes.
pub fn verify_linear_signature(
    signing_material: &[u8],
    raw_body: &[u8],
    signature_header: &str,
) -> bool {
    verify_hmac_sha256_hex(signing_material, raw_body, signature_header)
}

#[derive(Clone, Debug, Default)]
pub struct LinearSource {
    endpoint_override: Option<EndpointUrl>,
}

impl LinearSource {
    pub const fn new() -> Self {
        Self {
            endpoint_override: None,
        }
    }

    pub fn with_endpoint(endpoint: EndpointUrl) -> Self {
        Self {
            endpoint_override: Some(endpoint),
        }
    }

    fn resolve_endpoint(&self) -> Result<EndpointUrl, String> {
        match &self.endpoint_override {
            Some(ep) => Ok(ep.clone()),
            None => EndpointUrl::from_env(ENV_LINEAR_API_URL, DEFAULT_LINEAR_API_URL),
        }
    }

    fn graphql_post(
        &self,
        secrets: &SecretStore,
        payload: &Value,
        timeout: Duration,
        interrupted: &AtomicBool,
    ) -> Result<Value, TransportError> {
        let access_cred = secrets.get(ENV_ACCESS_CRED).ok_or_else(|| {
            TransportError::Rejected(format!("missing required credential {}", ENV_ACCESS_CRED))
        })?;
        let endpoint = self.resolve_endpoint().map_err(TransportError::Rejected)?;
        post_json_bearer(&endpoint, access_cred, payload, timeout, interrupted)
    }

    fn create_activity_call(
        &self,
        secrets: &SecretStore,
        ack: &OutboundAck,
        timeout: Duration,
        interrupted: &AtomicBool,
    ) -> Result<CreateDeliveryOutcome, TransportError> {
        let payload = json!({
            "query": MUTATION_AGENT_ACTIVITY_CREATE,
            "variables": {
                "input": {
                    "id": ack.delivery_id,
                    "agentSessionId": ack.session_ref,
                    "content": {
                        "type": ack.kind,
                        "body": ack.body,
                    }
                }
            }
        });
        let resp = self.graphql_post(secrets, &payload, timeout, interrupted)?;
        let created = resp.get("data").and_then(|d| d.get("agentActivityCreate"));
        if created
            .and_then(|c| c.get("success"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            return Ok(CreateDeliveryOutcome::Created);
        }
        if let Some(errors) = resp.get("errors").and_then(Value::as_array)
            && !errors.is_empty()
        {
            let already_exists = errors.iter().any(|err| {
                let msg = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_ascii_lowercase();
                msg.contains("already exists") || msg.contains("duplicate")
            });
            if already_exists {
                return Ok(CreateDeliveryOutcome::AlreadyExists);
            }
            return Err(TransportError::Rejected(
                "Linear agentActivityCreate mutation returned errors".into(),
            ));
        }
        Err(TransportError::OutcomeUnknown(
            "Linear agentActivityCreate returned unexpected shape".into(),
        ))
    }
}

fn extract_webhook_timestamp_ms(val: &Value) -> Option<i64> {
    val.as_i64().or_else(|| {
        val.as_f64()
            .filter(|f| f.is_finite() && f.fract() == 0.0 && *f >= 0.0 && *f <= i64::MAX as f64)
            .map(|f| f as i64)
    })
}

fn extract_prompt_body(content: Option<&Value>) -> Option<String> {
    match content {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Object(map)) => {
            if let Some(body) = map.get("body").and_then(Value::as_str) {
                Some(body.to_owned())
            } else {
                serde_json::to_string(map).ok()
            }
        }
        Some(other) => serde_json::to_string(other).ok(),
    }
}

impl Source for LinearSource {
    fn name(&self) -> &'static str {
        SOURCE_LINEAR
    }

    fn description(&self) -> &'static str {
        "Linear AgentSessionEvent webhooks and GraphQL agentActivity replies"
    }

    fn serve_env(&self) -> &'static [EnvVarSpec] {
        SERVE_ENV_SPECS
    }

    fn reply_env(&self) -> &'static [EnvVarSpec] {
        REPLY_ENV_SPECS
    }

    fn binding_flags(&self) -> &'static [BindingFlagSpec] {
        BINDING_FLAGS
    }

    fn reply_kinds(&self) -> &'static [&'static str] {
        REPLY_KINDS
    }

    fn signature_header(&self) -> &'static str {
        "Linear-Signature"
    }

    fn validate_binding(&self, flags: &BTreeMap<String, String>) -> Result<(), String> {
        let org = flags
            .get("--organization")
            .ok_or_else(|| "missing required option --organization".to_owned())?;
        let app_user = flags
            .get("--app-user")
            .ok_or_else(|| "missing required option --app-user".to_owned())?;
        validate_external_id(org, "organization")?;
        validate_external_id(app_user, "app-user")?;
        Ok(())
    }

    fn validate_endpoint(&self) -> Result<(), String> {
        self.resolve_endpoint().map(|_| ())
    }

    fn signature_input<'a>(
        &self,
        headers: &'a RequestHeaders,
        raw_body: &'a [u8],
    ) -> Option<SignatureInput<'a>> {
        let sig = headers.get(self.signature_header())?;
        Some(SignatureInput {
            secret_env: ENV_WEBHOOK_CRED,
            message: Cow::Borrowed(raw_body),
            signature_hex: sig,
        })
    }

    fn observed_session_ref(&self, payload: &Map<String, Value>) -> Option<String> {
        payload
            .get("agentSession")
            .and_then(|s| s.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    fn extract_timestamp_ms(
        &self,
        _headers: &RequestHeaders,
        payload: &Map<String, Value>,
    ) -> Option<i64> {
        payload
            .get("webhookTimestamp")
            .and_then(extract_webhook_timestamp_ms)
    }

    fn check_binding(
        &self,
        flags: &BTreeMap<String, String>,
        payload: &Map<String, Value>,
    ) -> BindingDecision {
        let Some(event_type) = payload.get("type").and_then(Value::as_str) else {
            return BindingDecision::BadRequest;
        };
        if event_type != "AgentSessionEvent" {
            return BindingDecision::Ignore;
        }
        let expected_org = flags.get("--organization").map(String::as_str);
        let expected_app = flags.get("--app-user").map(String::as_str);
        let org_id = payload.get("organizationId").and_then(Value::as_str);
        let app_user_id = payload.get("appUserId").and_then(Value::as_str);
        if org_id != expected_org || app_user_id != expected_app {
            BindingDecision::Forbidden
        } else {
            BindingDecision::Matched
        }
    }

    fn parse_work_request(&self, payload: &Map<String, Value>) -> PayloadDecision {
        let Some(action) = payload.get("action").and_then(Value::as_str) else {
            return PayloadDecision::BadRequest;
        };
        if !matches!(action, "created" | "prompted") {
            return PayloadDecision::Ignore;
        }
        let Some(session_obj) = payload.get("agentSession").and_then(Value::as_object) else {
            return PayloadDecision::BadRequest;
        };
        let Some(raw_session_id) = session_obj.get("id").and_then(Value::as_str) else {
            return PayloadDecision::BadRequest;
        };
        if validate_external_id(raw_session_id, "agentSession.id").is_err() {
            return PayloadDecision::BadRequest;
        }

        if action == "created" {
            let prompt_ctx = match payload.get("promptContext") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(s.as_str()),
                Some(_) => return PayloadDecision::BadRequest,
            };
            let issue = sanitize_header_field(
                session_obj
                    .get("issue")
                    .and_then(|i| i.get("identifier"))
                    .and_then(Value::as_str),
                80,
            );
            let url = sanitize_header_field(
                session_obj.get("url").and_then(Value::as_str).or_else(|| {
                    session_obj
                        .get("issue")
                        .and_then(|i| i.get("url"))
                        .and_then(Value::as_str)
                }),
                240,
            );
            let body = prompt_ctx
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("(no promptContext)");
            let raw_context = format!("issue: {issue}\nurl: {url}\n\n{body}");
            let msg_id = created_message_id(SOURCE_LINEAR, raw_session_id);
            match WorkRequest::new(
                SOURCE_LINEAR,
                raw_session_id,
                raw_session_id,
                WorkRequestKind::Created,
                &raw_context,
                msg_id,
                None,
            ) {
                Ok(req) => return PayloadDecision::Work(req),
                Err(_) => return PayloadDecision::BadRequest,
            }
        }

        let Some(activity_obj) = payload.get("agentActivity").and_then(Value::as_object) else {
            return PayloadDecision::BadRequest;
        };
        let Some(activity_id) = activity_obj.get("id").and_then(Value::as_str) else {
            return PayloadDecision::BadRequest;
        };
        if validate_external_id(activity_id, "agentActivity.id").is_err() {
            return PayloadDecision::BadRequest;
        }
        if let Some(act_sess) = activity_obj.get("agentSessionId").and_then(Value::as_str)
            && !act_sess.is_empty()
            && act_sess != raw_session_id
        {
            return PayloadDecision::BadRequest;
        }
        let signal = match activity_obj.get("signal") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.as_str()),
            Some(_) => return PayloadDecision::BadRequest,
        };
        let is_stop = signal == Some("stop");
        let kind = if is_stop {
            WorkRequestKind::Stop
        } else {
            WorkRequestKind::Prompted
        };
        let prompt_body = extract_prompt_body(activity_obj.get("content"));
        let trimmed = prompt_body
            .as_deref()
            .map(|s| s.trim_end_matches(['\r', '\n']))
            .unwrap_or("");
        let raw_context = if is_stop && trimmed.trim().is_empty() {
            ""
        } else if trimmed.trim().is_empty() {
            "(empty prompt)"
        } else {
            trimmed
        };
        let msg_id = prompted_message_id(SOURCE_LINEAR, activity_id);
        let origin_created_id = created_message_id(SOURCE_LINEAR, raw_session_id);

        match WorkRequest::new(
            SOURCE_LINEAR,
            activity_id,
            raw_session_id,
            kind,
            raw_context,
            msg_id,
            Some(origin_created_id),
        ) {
            Ok(req) => PayloadDecision::Work(req),
            Err(_) => PayloadDecision::BadRequest,
        }
    }

    fn routing_keys(&self, payload: &Map<String, Value>) -> Vec<String> {
        let Some(issue) = payload
            .get("agentSession")
            .and_then(|s| s.get("issue"))
            .and_then(Value::as_object)
        else {
            return Vec::new();
        };
        let team = issue.get("team");
        let mut keys = Vec::new();
        for value in [
            team.and_then(|t| t.get("key")),
            issue.get("teamId"),
            team.and_then(|t| t.get("id")),
        ] {
            if let Some(key) = value.and_then(Value::as_str)
                && key != crate::routes::ANY_TEAM
                && crate::routes::validate_team(key).is_ok()
                && !keys.iter().any(|k| k == key)
            {
                keys.push(key.to_owned());
            }
        }
        keys
    }

    fn unrouted_ack(&self, req: &WorkRequest) -> Option<OutboundAck> {
        (req.kind != WorkRequestKind::Stop).then(|| OutboundAck {
            delivery_id: derive_delivery_id(
                SOURCE_LINEAR,
                &req.session_ref,
                "",
                &format!("error:unrouted:{}", req.idempotency_key),
            ),
            session_ref: req.session_ref.clone(),
            kind: "error".to_owned(),
            body: UNROUTED_ERROR_BODY.to_owned(),
        })
    }

    fn first_ack(
        &self,
        req: &WorkRequest,
        room_id: &str,
        delivery: RoomDelivery,
    ) -> Option<OutboundAck> {
        match delivery {
            RoomDelivery::AppendedNew if req.kind == WorkRequestKind::Created => {
                Some(OutboundAck {
                    delivery_id: derive_delivery_id(
                        SOURCE_LINEAR,
                        &req.session_ref,
                        room_id,
                        CREATED_THOUGHT_CALLER_KEY,
                    ),
                    session_ref: req.session_ref.clone(),
                    kind: "thought".to_owned(),
                    body: FIRST_THOUGHT_BODY.to_owned(),
                })
            }
            RoomDelivery::Refused => Some(OutboundAck {
                delivery_id: derive_delivery_id(
                    SOURCE_LINEAR,
                    &req.session_ref,
                    room_id,
                    &format!("error:refused:{}", req.idempotency_key),
                ),
                session_ref: req.session_ref.clone(),
                kind: "error".to_owned(),
                body: REFUSED_CONTENT_ERROR_BODY.to_owned(),
            }),
            RoomDelivery::AppendedNew | RoomDelivery::Duplicate => None,
        }
    }

    fn deliver_ack(
        &self,
        secrets: &SecretStore,
        ack: &OutboundAck,
        timeout: Duration,
        interrupted: &AtomicBool,
    ) -> Result<(), TransportError> {
        self.create_activity_call(secrets, ack, timeout, interrupted)
            .map(|_| ())
    }

    fn query_delivery(
        &self,
        secrets: &SecretStore,
        delivery_id: &str,
        _session_ref: &str,
        timeout: Duration,
        interrupted: &AtomicBool,
    ) -> Result<bool, TransportError> {
        let payload = json!({
            "query": QUERY_AGENT_ACTIVITY,
            "variables": { "id": delivery_id },
        });
        let resp = self.graphql_post(secrets, &payload, timeout, interrupted)?;
        if let Some(found_id) = resp
            .get("data")
            .and_then(|d| d.get("agentActivity"))
            .and_then(|a| a.get("id"))
            .and_then(Value::as_str)
        {
            return Ok(found_id == delivery_id);
        }
        if let Some(errors) = resp.get("errors").and_then(Value::as_array)
            && !errors.is_empty()
        {
            let all_not_found = errors.iter().all(|err| {
                let msg = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_ascii_lowercase();
                let ext = err
                    .get("extensions")
                    .and_then(|e| e.get("code"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_ascii_lowercase();
                msg.contains("not found") || ext.contains("not_found")
            });
            if all_not_found {
                return Ok(false);
            }
            return Err(TransportError::Rejected(
                "Linear GraphQL query returned errors".into(),
            ));
        }
        if resp
            .get("data")
            .and_then(|d| d.get("agentActivity"))
            .is_some_and(Value::is_null)
        {
            return Ok(false);
        }
        Err(TransportError::OutcomeUnknown(
            "Linear GraphQL query returned unexpected shape".into(),
        ))
    }

    fn create_delivery(
        &self,
        secrets: &SecretStore,
        reply: &OutboundReply,
        timeout: Duration,
        interrupted: &AtomicBool,
    ) -> Result<CreateDeliveryOutcome, TransportError> {
        let req = OutboundAck {
            delivery_id: reply.delivery_id.clone(),
            session_ref: reply.session_ref.clone(),
            kind: reply.kind.clone(),
            body: reply.body.clone(),
        };
        self.create_activity_call(secrets, &req, timeout, interrupted)
    }
}
