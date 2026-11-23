//! Advisory council evidence shared by the native producer and Plan admission.
//! Complete observations and immutable bytes do not establish semantic truth.
use crate::json::{self, Value, number, object, string};
use crate::storage::{MAX_JSON, Store};
use crate::{Result, decision, error, require};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

/// Native observation for one call. Requested model is not served-model proof;
/// actual/fallback provenance remains in the existing usage receipts.
pub struct CouncilOutput {
    pub provider: String,
    pub requested_model: String,
    pub round: u64,
    pub artifact: String,
    pub artifact_sha256: String,
    pub output_start: usize,
    pub output_bytes: usize,
    pub output_sha256: String,
    pub complete: bool,
    pub exit: i32,
    pub quality: String,
}
impl CouncilOutput {
    fn value(&self) -> Value {
        object([
            ("provider", string(&self.provider)),
            ("requested_model", string(&self.requested_model)),
            ("round", number(self.round)),
            ("artifact", string(&self.artifact)),
            ("artifact_sha256", string(&self.artifact_sha256)),
            ("output_start", number(self.output_start)),
            ("output_bytes", number(self.output_bytes)),
            ("output_sha256", string(&self.output_sha256)),
            ("complete", Value::Bool(self.complete)),
            ("exit", number(self.exit)),
            ("quality", string(&self.quality)),
        ])
    }
}

/// Additive stdout protocol for `peer-ask --synthesis-json`.
pub fn encode_council_synthesis(
    prompt: &str,
    run: &str,
    rounds: u64,
    participants: &[CouncilOutput],
    synthesis: &CouncilOutput,
) -> Result<Vec<u8>> {
    let report = object([
        ("schema", number(1)),
        ("kind", string("oms-council-synthesis")),
        ("run", string(run)),
        ("prompt_sha256", string(json::sha(prompt))),
        ("rounds", number(rounds)),
        (
            "participants",
            Value::Array(participants.iter().map(CouncilOutput::value).collect()),
        ),
        ("synthesis", synthesis.value()),
    ]);
    let mut raw = json::pretty(&report)?;
    raw.push(b'\n');
    require(raw.len() <= 65536, "council report exceeds 65536 bytes")?;
    Ok(raw)
}

fn response_sections(answer: &str, required: &[&str]) -> Result<()> {
    require(
        !answer.trim().is_empty()
            && answer.len() <= 65536
            && !answer
                .chars()
                .any(|c| c.is_control() && !['\n', '\t', '\r'].contains(&c)),
        "council response must be bounded text",
    )?;
    let mut found = BTreeMap::<String, String>::new();
    let mut current = None;
    for line in answer.lines() {
        let heading = line.trim().trim_start_matches('#').trim();
        if let Some((name, rest)) = heading.split_once(':')
            && required
                .iter()
                .any(|wanted| name.eq_ignore_ascii_case(wanted))
        {
            let key = name.to_ascii_lowercase();
            require(
                !found.contains_key(&key),
                "duplicate council response section",
            )?;
            found.insert(key.clone(), rest.trim().to_owned());
            current = Some(key);
        } else if line.trim_start().starts_with('#') {
            current = None;
        } else if let Some(key) = &current {
            found.get_mut(key).unwrap().push_str(line);
        }
    }
    require(
        required.iter().all(|name| {
            found
                .get(&name.to_ascii_lowercase())
                .is_some_and(|text| !text.trim().is_empty())
        }),
        "council response lacks required nonempty sections",
    )
}

/// Structural lint preserves evidence and dissent for review. Presence proves
/// neither sound reasoning, independent models, consensus nor acceptance.
pub fn lint_council_response(answer: &str, round: u64, synthesis: bool) -> Result<()> {
    if synthesis {
        response_sections(
            answer,
            &[
                "Decision",
                "Evidence",
                "Remaining disagreements",
                "Verification",
            ],
        )
    } else if round == 0 {
        response_sections(
            answer,
            &[
                "Alternatives",
                "Evidence",
                "Counterargument",
                "Verification",
            ],
        )
    } else {
        response_sections(
            answer,
            &[
                "Alternatives",
                "Evidence",
                "Counterargument",
                "Verification",
                "Changed from previous round",
                "Remaining disagreements",
            ],
        )
    }
}

fn id(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn integer(v: &Value, low: u64, high: u64, label: &str) -> Result<u64> {
    v.int()
        .ok()
        .and_then(|n| u64::try_from(n).ok())
        .filter(|n| (low..=high).contains(n))
        .ok_or_else(|| error(format!("{label} must be {low}..{high}")))
}

pub struct ValidatedOutput {
    pub run: String,
    pub answer: String,
    /// Exact source files to recheck before the proposal is published.
    pub artifacts: Vec<(String, String)>,
}

fn validate_output(
    run: &str,
    row: &Value,
    tool: &str,
    read: &impl Fn(&str) -> Result<Vec<u8>>,
) -> Result<String> {
    json::keys(
        row,
        &[
            "provider",
            "requested_model",
            "round",
            "artifact",
            "artifact_sha256",
            "output_start",
            "output_bytes",
            "output_sha256",
            "complete",
            "exit",
            "quality",
        ],
        &[],
    )?;
    let provider = row.text("provider");
    let model = row.text("requested_model");
    let round = integer(row.get("round"), 0, 3, "native output round")?;
    let path = row.text("artifact");
    let digest = row.text("artifact_sha256");
    crate::engine::plain(model, "requested native model")?;
    require(
        id(provider, 64)
            && !model.is_empty()
            && model.len() <= 240
            && matches!(row.get("complete"), Value::Bool(true))
            && row.get("exit").is_int(0)
            && row.text("quality") == "ok"
            && Path::new(path).components().count() == 1
            && matches!(
                Path::new(path).components().next(),
                Some(Component::Normal(_))
            )
            && json::hex(digest, 64)
            && json::hex(row.text("output_sha256"), 64),
        "native call is incomplete or has malformed evidence",
    )?;
    let suffix = if round == 0 {
        format!("-{run}.md")
    } else {
        format!("-{run}-r{round}.md")
    };
    require(
        if tool == "_synthesis" {
            round == 0 && path == format!("_synthesis-{run}.md")
        } else {
            path.starts_with(&format!("{provider}-")) && path.ends_with(&suffix)
        },
        "native artifact belongs to a different provider, round or run",
    )?;
    let bytes = read(path)?;
    require(
        bytes.len() <= MAX_JSON && json::sha(&bytes) == digest,
        "native artifact bytes changed",
    )?;
    let header = format!(
        "# OMS {tool}\n\n- provider: {provider}\n- model: {model}\n- run: {run}\n- round: {round}\n"
    );
    let start = integer(
        row.get("output_start"),
        0,
        MAX_JSON as u64,
        "native output offset",
    )? as usize;
    let output_limit = if tool == "agent-call" {
        MAX_JSON as u64
    } else {
        65536
    };
    let count = integer(
        row.get("output_bytes"),
        1,
        output_limit,
        "native output bytes",
    )? as usize;
    let end = start
        .checked_add(count)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| error("invalid native output range"))?;
    const MARKER: &[u8] = b"\n## Output\n\n";
    require(
        bytes.starts_with(header.as_bytes())
            && bytes.ends_with(b"\n## Exit\n\n0\n")
            && start
                .checked_sub(MARKER.len())
                .is_some_and(|before| bytes.get(before..start) == Some(MARKER))
            && json::sha(&bytes[start..end]) == row.text("output_sha256")
            && (bytes[end..].starts_with(b"\n\n## Exit\n\n")
                || bytes[end..].starts_with(b"\n\n## Diagnostics\n\n")),
        "native artifact does not match its native output binding",
    )?;
    let answer =
        std::str::from_utf8(&bytes[start..end]).map_err(|_| error("native output is not UTF-8"))?;
    Ok(answer.to_owned())
}

/// Exact native stdout boundary for the planning caller. Human text mode is
/// unchanged; this opt-in envelope never treats prompt or diagnostics as output.
pub fn encode_agent_output(prompt: &str, run: &str, output: &CouncilOutput) -> Result<Vec<u8>> {
    let report = object([
        ("schema", number(1)),
        ("kind", string("oms-agent-output")),
        ("run", string(run)),
        ("prompt_sha256", string(json::sha(prompt))),
        ("output", output.value()),
    ]);
    let mut raw = json::pretty(&report)?;
    raw.push(b'\n');
    require(
        raw.len() <= 65536,
        "native output report exceeds 65536 bytes",
    )?;
    Ok(raw)
}

pub fn validate_agent_output(
    raw: &[u8],
    prompt: &str,
    provider: &str,
    model: Option<&str>,
    read: impl Fn(&str) -> Result<Vec<u8>>,
) -> Result<ValidatedOutput> {
    let report = json::parse_bounded(raw, 65536)?;
    json::keys(
        &report,
        &["schema", "kind", "run", "prompt_sha256", "output"],
        &[],
    )?;
    let row = report.get("output");
    let run = report.text("run");
    require(
        report.get("schema").is_int(1)
            && report.text("kind") == "oms-agent-output"
            && id(run, 160)
            && report.text("prompt_sha256") == json::sha(prompt)
            && row.text("provider") == provider
            && model.is_none_or(|m| row.text("requested_model") == m)
            && row.get("round").is_int(0),
        "native planner output differs from its request",
    )?;
    let answer = validate_output(run, row, "agent-call", &read)?;
    Ok(ValidatedOutput {
        run: run.to_owned(),
        answer,
        artifacts: vec![(
            row.text("artifact").to_owned(),
            row.text("artifact_sha256").to_owned(),
        )],
    })
}

/// The reader must be bounded and no-follow, rooted at this invocation's fresh
/// council directory. Provider prose is never parsed as a control line.
pub fn validate_council_synthesis(
    raw: &[u8],
    prompt: &str,
    providers: &[&str],
    synthesis_route: (&str, &str),
    rounds: u64,
    read: impl Fn(&str) -> Result<Vec<u8>>,
) -> Result<ValidatedOutput> {
    let report = json::parse_bounded(raw, 65536)?;
    json::keys(
        &report,
        &[
            "schema",
            "kind",
            "run",
            "prompt_sha256",
            "rounds",
            "participants",
            "synthesis",
        ],
        &[],
    )?;
    let run = report.text("run");
    require(
        report.get("schema").is_int(1)
            && report.text("kind") == "oms-council-synthesis"
            && id(run, 160)
            && report.text("prompt_sha256") == json::sha(prompt)
            && (1..=3).contains(&rounds)
            && report.get("rounds").is_int(rounds as i128)
            && !providers.is_empty()
            && providers.len() <= 16
            && providers.iter().all(|p| id(p, 64))
            && providers.iter().collect::<BTreeSet<_>>().len() == providers.len(),
        "council report differs from the requested discussion",
    )?;
    let rows = report.get("participants").array()?;
    require(
        rows.len() == providers.len() * (rounds as usize + 1),
        "council rounds are incomplete",
    )?;
    let mut matrix = BTreeSet::new();
    let mut files = BTreeSet::new();
    let mut artifacts = Vec::new();
    let mut inspect = |row: &Value, synthesis: bool| -> Result<String> {
        let provider = row.text("provider");
        let round = integer(row.get("round"), 0, rounds, "council round")?;
        require(
            files.insert(row.text("artifact").to_owned()),
            "duplicate council artifact",
        )?;
        require(
            if synthesis {
                round == 0
                    && provider == synthesis_route.0
                    && row.text("requested_model") == synthesis_route.1
            } else {
                providers.contains(&provider) && matrix.insert((provider.to_owned(), round))
            },
            "council artifact belongs to a different route or round",
        )?;
        let answer = validate_output(
            run,
            row,
            if synthesis { "_synthesis" } else { "peer-ask" },
            &read,
        )?;
        lint_council_response(&answer, round, synthesis)?;
        artifacts.push((
            row.text("artifact").to_owned(),
            row.text("artifact_sha256").to_owned(),
        ));
        Ok(answer)
    };
    for row in rows {
        inspect(row, false)?;
    }
    let answer = inspect(report.get("synthesis"), true)?;
    Ok(ValidatedOutput {
        run: run.to_owned(),
        answer,
        artifacts,
    })
}

/// Check the native decision-catalog reference retained in a reviewed proposal.
/// It is advisory lineage, never approval; legacy proposals may omit it.
pub fn validate_proposal(store: &Store, proposal: &Value) -> Result<Option<ValidatedOutput>> {
    let Some(reference) = proposal.opt("discussion") else {
        return Ok(None);
    };
    json::keys(reference, &["path", "sha256"], &[])?;
    let digest = json::digest(reference.get("sha256"))?;
    require(
        reference.text("path") == format!("{}/{digest}.json", decision::CATALOG),
        "discussion must reference the immutable native decision catalog",
    )?;
    let raw = store.read(reference.text("path"), 65536)?;
    require(
        json::sha(&raw) == digest,
        "planning discussion digest changed",
    )?;
    let manifest = json::parse_bounded(&raw, 65536)?;
    json::keys(
        &manifest,
        &[
            "schema",
            "kind",
            "spec_sha256",
            "plan_sha256",
            "base_sha",
            "allowed_envelope",
            "question",
            "providers",
            "rounds",
            "synthesis_route",
            "report",
        ],
        &[],
    )?;
    require(
        manifest.get("schema").is_int(1) && manifest.text("kind") == "oms-planning-discussion",
        "invalid planning discussion manifest",
    )?;
    for field in ["spec_sha256", "plan_sha256", "base_sha", "allowed_envelope"] {
        require(
            manifest.get(field) == proposal.get(field),
            "discussion belongs to different planning inputs",
        )?;
    }
    let question = manifest.get("question");
    json::keys(question, &["path", "sha256"], &[])?;
    let path = question.text("path");
    let parts = path.split('/').collect::<Vec<_>>();
    require(
        parts.len() == 4
            && parts[0] == ".oms"
            && parts[1] == "plan"
            && parts[3] == "question.md"
            && parts[2]
                .strip_prefix("council-")
                .is_some_and(|nonce| json::hex(nonce, 32)),
        "discussion question must belong to one native council directory",
    )?;
    let question_raw = store.read(path, MAX_JSON)?;
    require(
        json::sha(&question_raw) == json::digest(question.get("sha256"))?,
        "discussion question bytes changed",
    )?;
    let question_text = std::str::from_utf8(&question_raw)
        .map_err(|_| error("discussion question is not UTF-8"))?;
    let route = manifest.get("synthesis_route");
    json::keys(route, &["provider", "requested_model"], &[])?;
    let providers = manifest.get("providers").strings()?;
    let rounds = integer(manifest.get("rounds"), 1, 3, "discussion rounds")?;
    let directory = Path::new(path).parent().unwrap();
    validate_council_synthesis(
        &json::canonical(manifest.get("report"))?,
        question_text,
        &providers,
        (route.text("provider"), route.text("requested_model")),
        rounds,
        |name| store.read(directory.join(name), MAX_JSON),
    )
    .map(Some)
}
