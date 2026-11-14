//! Review/council metadata and resumable settlement claims.
use crate::{Error, Result, Store, Value, require};
use std::path::Path;
use std::time::Duration;
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Candidate {
    pub patch: String,
    pub sha256: String,
    pub base: String,
}
impl Candidate {
    pub fn validate(&self) -> Result<()> {
        crate::storage::path(Path::new(&self.patch))?;
        require(
            self.patch.starts_with(".oms/artifacts/")
                && crate::hex(&self.sha256, 64)
                && (7..=64).contains(&self.base.len())
                && self
                    .base
                    .bytes()
                    .all(|v| v.is_ascii_hexdigit() && !v.is_ascii_uppercase()),
            "automatic review requires linked patch digest and recorded base",
        )
    }
}
pub fn verdict(raw: &Value) -> Result<&str> {
    let verdict = raw["verdict"]
        .as_str()
        .ok_or_else(|| Error::new("review verdict absent"))?;
    require(
        ["pass", "fail", "blocked", "approve", "request_changes"].contains(&verdict),
        "invalid review verdict",
    )?;
    Ok(verdict)
}
pub fn automatic_review_workload(patch: &[u8]) -> Result<&'static str> {
    let paths = crate::files::patch_paths(patch)?;
    if paths
        .iter()
        .all(|p| p.ends_with(".md") || p.starts_with("docs/"))
    {
        Ok("routine")
    } else {
        Ok("deep")
    }
}
const STEPS: &[&str] = &[
    "settlement",
    "room_result",
    "structured_event",
    "auto_review",
    "room_quiet",
    "review_launch",
];
#[derive(Clone, Debug)]
pub struct Settlement {
    pub member: String,
    pub claim_id: String,
    pub owner: oms_reference::process::Owner,
}
impl Settlement {
    fn path(member: &str) -> String {
        format!(
            ".oms/artifacts/panel-dispatch/settlement-{}.json",
            crate::digest(member.as_bytes())
        )
    }
    pub fn claim(store: &Store, member: &str, status: i32) -> Result<Option<Self>> {
        crate::parent_only()?;
        crate::identifier(member)?;
        let owner = crate::terminals::process(std::process::id())?;
        let tx = store.transaction("panel-call-settle", Duration::from_secs(10))?;
        let path = Self::path(member);
        let old = tx.json(Path::new(&path), 65536, true)?;
        if let Some(old) = &old {
            require(
                old["schema"] == 1
                    && old["member"] == member
                    && old["exit"] == status
                    && old["steps"].is_object(),
                "invalid or conflicting settlement claim",
            )?;
            if old["complete"] == true {
                return Ok(None);
            }
            let prior = owner_from(old)?;
            if old["active"] == true && !crate::terminals::process_gone(&prior) {
                return Ok(None);
            }
        }
        let claim_id = old
            .as_ref()
            .and_then(|v| v["claim_id"].as_str())
            .map(str::to_owned)
            .unwrap_or(crate::value::unique()?);
        let mut row = old.unwrap_or_else(|| serde_json::json!({"schema":1,"member":member,"exit":status,"claim_id":claim_id,"steps":{},"complete":false}));
        row["owner_pid"] = owner.pid.into();
        row["owner_identity"] = owner.birth.clone().into();
        row["owner_uid"] = owner.uid.into();
        row["active"] = true.into();
        tx.write_json(Path::new(&path), &row, false, 65536)?;
        Ok(Some(Self {
            member: member.into(),
            claim_id,
            owner,
        }))
    }
    pub fn step(&self, store: &Store, name: &str) -> Result<()> {
        require(STEPS.contains(&name), "invalid settlement step")?;
        self.change(store, |row| {
            row["steps"][name] = true.into();
            Ok(())
        })
    }
    pub fn complete(&self, store: &Store) -> Result<()> {
        self.change(store, |row| {
            require(
                ["settlement", "room_result", "structured_event"]
                    .iter()
                    .all(|s| row["steps"][s] == true),
                "required settlement steps are incomplete",
            )?;
            row["complete"] = true.into();
            row["active"] = false.into();
            Ok(())
        })
    }
    pub fn release(&self, store: &Store) -> Result<()> {
        self.change(store, |row| {
            row["active"] = false.into();
            Ok(())
        })
    }
    fn change(&self, store: &Store, change: impl FnOnce(&mut Value) -> Result<()>) -> Result<()> {
        crate::parent_only()?;
        let tx = store.transaction("panel-call-settle", Duration::from_secs(10))?;
        let path = Self::path(&self.member);
        let mut row = tx
            .json(Path::new(&path), 65536, false)?
            .ok_or_else(|| Error::new("settlement claim missing"))?;
        require(
            row["claim_id"] == self.claim_id
                && owner_from(&row)? == self.owner
                && row["active"] == true,
            "settlement claim owner changed",
        )?;
        change(&mut row)?;
        tx.write_json(Path::new(&path), &row, false, 65536)?;
        Ok(())
    }
}
fn owner_from(row: &Value) -> Result<oms_reference::process::Owner> {
    let pid = row["owner_pid"]
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v > 0)
        .ok_or_else(|| Error::new("settlement owner PID invalid"))?;
    let birth = row["owner_identity"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::new("settlement owner generation missing"))?
        .to_owned();
    let uid = row["owner_uid"]
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| Error::new("settlement owner UID invalid"))?;
    Ok(oms_reference::process::Owner { pid, birth, uid })
}
pub fn council_metadata(
    task: &str,
    label: &str,
    rounds: usize,
    seats: &[crate::routing::Candidate],
) -> Result<Value> {
    crate::identifier(task)?;
    require(
        (1..=2).contains(&rounds),
        "council rounds must be 1 or 2; use CouncilRequest for lenses/until-agreement",
    )?;
    let seats = budget_seats(seats.to_vec(), rounds, false)?;
    Ok(
        serde_json::json!({"schema":1,"kind":"oms-panel-council","task_id":task,"label":crate::eventlog::clean(label,120),"rounds":rounds,"seats":seats,"executes":false}),
    )
}

#[derive(Clone, Debug)]
pub struct CouncilRequest {
    pub owner: String,
    pub task_id: String,
    pub label: String,
    pub prompt: String,
    pub thread: Option<String>,
    pub rounds: Option<usize>,
    pub workload: String,
    pub lenses: Option<Vec<String>>,
    pub until_agreement: bool,
    pub max_rounds: Option<usize>,
    pub allow_partial: bool,
    pub dry_run: bool,
    pub room: Option<String>,
    pub main_attempt: Option<String>,
    pub participant: Option<String>,
}
fn budget_seats(
    mut seats: Vec<crate::routing::Candidate>,
    rounds: usize,
    lenses: bool,
) -> Result<Vec<crate::routing::Candidate>> {
    for seat in &seats {
        crate::routing::valid_route(seat)?;
    }
    if !lenses {
        let mut first = std::collections::BTreeSet::new();
        let duplicate = seats
            .iter()
            .map(|v| !first.insert(v.provider.clone()))
            .collect::<Vec<_>>();
        for index in (0..seats.len()).rev() {
            if seats.len().saturating_mul(rounds + 1) <= 12 {
                break;
            }
            if duplicate[index] {
                seats.remove(index);
            }
        }
    }
    require(
        !seats.is_empty() && seats.len().saturating_mul(rounds + 1) <= 12,
        "council exceeds the 12-call budget",
    )?;
    require(
        seats
            .iter()
            .map(|v| &v.provider)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            >= 2,
        "council requires two independent provider families",
    )?;
    let mut seen = std::collections::BTreeSet::new();
    let (mut first, repeated): (Vec<_>, Vec<_>) = seats
        .into_iter()
        .partition(|v| seen.insert(v.provider.clone()));
    first.extend(repeated);
    Ok(first)
}
/// Build the exact PeerAsk argv without executing provider calls.
pub fn council_plan(
    repo: &Path,
    request: &CouncilRequest,
    policy: &crate::routing::Policy,
    service: &dyn crate::Services,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<Value> {
    crate::identifier(&request.task_id)?;
    require(
        !request.label.trim().is_empty() && !request.label.chars().any(char::is_control),
        "council needs a task title",
    )?;
    require(
        !request.prompt.trim().is_empty()
            && request.prompt.len() <= crate::dispatch::MAX_PROMPT
            && !request.prompt.contains('\0'),
        "council needs a bounded prompt",
    )?;
    crate::outbound(request.prompt.as_bytes())?;
    require(
        request.label.chars().count() <= 120,
        "council title exceeds 120 characters",
    )?;
    crate::outbound(request.label.as_bytes())?;
    require(
        ["routine", "deep", "frontier"].contains(&request.workload.as_str()),
        "invalid council workload",
    )?;
    require(
        request.until_agreement || request.max_rounds.is_none(),
        "--max-rounds requires --until-agreement",
    )?;
    let rounds = if request.until_agreement {
        require(
            request.lenses.is_none() && request.rounds.is_none(),
            "--until-agreement cannot combine with --lenses or --rounds",
        )?;
        let rounds = request.max_rounds.unwrap_or(4);
        require((1..=6).contains(&rounds), "--max-rounds must be 1 to 6")?;
        rounds
    } else if let Some(lenses) = &request.lenses {
        require(
            request.rounds.unwrap_or(0) == 0 && (2..=12).contains(&lenses.len()),
            "--lenses requires --rounds 0 and 2..12 lenses",
        )?;
        let mut seen = std::collections::BTreeSet::new();
        for lens in lenses {
            require(
                !lens.is_empty()
                    && lens.len() <= 32
                    && lens
                        .bytes()
                        .all(|v| v.is_ascii_lowercase() || v.is_ascii_digit() || v == b'-')
                    && seen.insert(lens),
                "lenses must be unique [a-z0-9-] names of 1..32 characters",
            )?;
        }
        0
    } else {
        let rounds = request.rounds.unwrap_or(1);
        require((1..=2).contains(&rounds), "council rounds must be 1 or 2")?;
        rounds
    };
    let mut seats = policy
        .tiers
        .get(if request.workload == "routine" {
            "routine"
        } else {
            "deep"
        })
        .cloned()
        .ok_or_else(|| Error::new("council tier absent"))?;
    if request.workload != "routine" {
        seats.extend(
            policy
                .tiers
                .get(if request.workload == "frontier" {
                    "frontier"
                } else {
                    "routine"
                })
                .ok_or_else(|| Error::new("council tier absent"))?
                .clone(),
        );
    }
    for extra in &policy.council.extra {
        if seats.iter().all(|v| v.provider != extra.provider) {
            seats.push(extra.clone());
        }
    }
    let providers = seats
        .iter()
        .map(|v| v.provider.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    let catalog = crate::services::query(
        service,
        crate::NativeCommand::ProviderCatalog,
        repo,
        &["--providers", &providers, "--json"],
        stop,
    )?;
    let rows = if catalog.is_array() {
        crate::value::array(&catalog)
    } else {
        crate::value::array(&catalog["providers"])
    };
    let allow_partial = request.allow_partial
        || std::env::var_os("OMS_COUNCIL_ALLOW_PARTIAL").is_some_and(|v| v == "1");
    let mut kept = Vec::new();
    let mut skipped = Vec::new();
    for mut seat in seats {
        let resolve = || -> Result<String> {
            let row = rows
                .iter()
                .find(|v| v["provider"] == seat.provider)
                .ok_or_else(|| Error::new("catalog lookup failed"))?;
            require(
                row["installed"] == true || row["present"] == true,
                "provider is not installed",
            )?;
            let models = row
                .get("routable")
                .or_else(|| row.get("catalog_models"))
                .or_else(|| row.get("models"))
                .ok_or_else(|| Error::new("model catalog unavailable"))?;
            let models = models
                .as_array()
                .ok_or_else(|| Error::new("malformed model catalog"))?
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| Error::new("malformed model catalog"))
                })
                .collect::<Result<Vec<_>>>()?;
            require(!models.is_empty(), "empty model catalog")?;
            crate::routing::routable_route_model(&seat.provider, &seat.model, &models)
        };
        match resolve() {
            Ok(model) => {
                seat.model = model;
                kept.push(seat);
            }
            Err(e) if allow_partial => skipped.push(
                serde_json::json!({"provider":seat.provider,"model":seat.model,"reason":e.message}),
            ),
            Err(e) => {
                return Err(Error::new(format!(
                    "council: {} seat unavailable: {}",
                    seat.provider, e.message
                )));
            }
        }
    }
    let mut seats = budget_seats(kept, rounds, request.lenses.is_some())?;
    if let Some(lenses) = &request.lenses {
        seats = (0..lenses.len())
            .map(|i| seats[i % seats.len()].clone())
            .collect();
        let families = seats
            .iter()
            .map(|v| &v.provider)
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        require(
            families >= 2 && (allow_partial || lenses.len() < 3 || families >= 3),
            "council lenses require independent provider families (three for 3+ lenses)",
        )?;
    }
    let thread = if let Some(thread) = &request.thread {
        crate::identifier(thread)?;
        thread.clone()
    } else if request.lenses.is_some() {
        "panel-council".into()
    } else {
        format!("panel-council-{}", &url_uuid_v5(&request.task_id)[..24])
    };
    let thread = if request.lenses.is_some() {
        format!(
            "{}-{}",
            &thread[..thread.len().min(126)],
            crate::value::unique()?
        )
    } else {
        thread
    };
    let targets = seats
        .iter()
        .map(|v| format!("{}:model={}", v.provider, v.model))
        .collect::<Vec<_>>()
        .join(",");
    let mut argv = vec!["--providers".to_owned(), targets];
    if request.until_agreement {
        argv.extend([
            "--until-agreement".into(),
            "--max-rounds".into(),
            rounds.to_string(),
        ]);
    } else {
        argv.extend(["--debate".into(), rounds.to_string()]);
    }
    argv.extend([
        "--deliberation".into(),
        "--require-complete".into(),
        "--no-task".into(),
        "--no-memory".into(),
        "--reasoning-effort".into(),
        "high".into(),
        "--prompt".into(),
        request.prompt.clone(),
        "--thread".into(),
        thread.clone(),
    ]);
    let primary = seats.len() * (rounds + 1);
    Ok(
        serde_json::json!({"schema":1,"kind":"oms-panel-council-plan","argv":argv,"thread":thread,"seats":seats.len(),"seat_routes":seats,"families":seats.iter().map(|v| &v.provider).collect::<std::collections::BTreeSet<_>>().len(),"primary_calls":primary,"max_format_repairs":primary,"max_calls":primary*2,"skipped":skipped,"executes":false}),
    )
}

/// RFC 4122 URL namespace UUID, using RFC 3174 SHA-1 only for name identity.
fn url_uuid_v5(name: &str) -> String {
    let mut bytes = vec![
        0x6b, 0xa7, 0xb8, 0x11, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30,
        0xc8,
    ];
    bytes.extend_from_slice(name.as_bytes());
    let bits = (bytes.len() as u64).wrapping_mul(8);
    bytes.push(0x80);
    while bytes.len() % 64 != 56 {
        bytes.push(0);
    }
    bytes.extend_from_slice(&bits.to_be_bytes());
    let mut state = [
        0x67452301u32,
        0xefcdab89,
        0x98badcfe,
        0x10325476,
        0xc3d2e1f0,
    ];
    for block in bytes.as_chunks::<64>().0 {
        let mut words = [0u32; 80];
        for (slot, chunk) in words.iter_mut().zip(block.as_chunks::<4>().0) {
            *slot = u32::from_be_bytes(*chunk);
        }
        for i in 16..80 {
            words[i] = (words[i - 3] ^ words[i - 8] ^ words[i - 14] ^ words[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (i, word) in words.into_iter().enumerate() {
            let (f, k) = match i {
                0..20 => ((b & c) | ((!b) & d), 0x5a827999),
                20..40 => (b ^ c ^ d, 0x6ed9eba1),
                40..60 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(word);
            (a, b, c, d, e) = (next, a, b.rotate_left(30), c, d);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut uuid = [0u8; 16];
    for (chunk, word) in uuid.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    uuid[6] = (uuid[6] & 0x0f) | 0x50;
    uuid[8] = (uuid[8] & 0x3f) | 0x80;
    uuid.iter().map(|v| format!("{v:02x}")).collect()
}

#[cfg(test)]
mod uuid_tests {
    #[test]
    fn url_namespace_vectors() {
        for (name, expected) in [
            ("", "1b4db7eb40575ddf91e036dec72071f5"),
            ("python.org", "7af94e2b4dd950f09c9a8a48519bdef0"),
            (
                "https://www.widgets.com",
                "423435676fc35a6a80a283d9a01cadaa",
            ),
            ("한글", "518e75929d3c5384be103e828da44b67"),
        ] {
            assert_eq!(super::url_uuid_v5(name), expected);
        }
    }
}
pub fn council_execute(
    repo: &Path,
    request: &CouncilRequest,
    policy: &crate::routing::Policy,
    service: &dyn crate::Services,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<Value> {
    let mut plan = council_plan(repo, request, policy, service, stop)?;
    if request.dry_run {
        return Ok(plan);
    }
    crate::parent_only()?;
    require(
        ["codex", "claude", "antigravity", "pi"].contains(&request.owner.as_str()),
        "invalid council owner",
    )?;
    if let Some(parent) = &request.main_attempt {
        crate::identifier(parent)?;
        let row = crate::services::query(
            service,
            crate::NativeCommand::Events,
            repo,
            &["show", "--attempt", parent, "--json"],
            stop,
        )?;
        require(
            row["attempt_id"] == *parent
                && row["tool"] == "panel-main"
                && row["provider"] == request.owner
                && row["terminal"] == false,
            "council main identity does not match an active owner",
        )?;
        if let Some(room) = &request.room {
            require(
                row["refs"]["panel_room_id"] == *room
                    && row["refs"]["panel_room_participant"].as_str()
                        == request.participant.as_deref(),
                "council main room binding differs",
            )?;
        }
    }
    if request.room.is_some() {
        require(
            request.main_attempt.is_some() && request.participant.is_some(),
            "room council requires an owning main",
        )?;
    }
    if let Some(room_id) = &request.room {
        crate::identifier(room_id)?;
        let room = crate::services::query(
            service,
            crate::NativeCommand::Room,
            repo,
            &["show", "--id", room_id, "--json"],
            stop,
        )?;
        require(
            room["closed"] == false
                && crate::value::array(&room["participants"]).iter().any(|v| {
                    v["participant"].as_str() == request.participant.as_deref()
                        && v["role"] == "main"
                        && v["joined"] == true
                }),
            "council owner is not joined to an open room",
        )?;
        let attempts = crate::services::query(
            service,
            crate::NativeCommand::Events,
            repo,
            &["list", "--json"],
            stop,
        )?;
        let mains =
            crate::state::active_mains(crate::value::array(&attempts), room_id, &request.owner);
        require(
            mains.len() == 1 && mains[0]["attempt_id"].as_str() == request.main_attempt.as_deref(),
            "select a unique active main before starting a council",
        )?;
    }
    let mut start = crate::services::args(&[
        "start",
        "--provider",
        &request.owner,
        "--tool",
        "panel-council",
        "--task-id",
        &request.task_id,
        "--ref",
        "panel_role=advisor",
        "--ref",
        &format!("panel_owner={}", request.owner),
        "--ref",
        &format!("panel_label={}", request.label),
        "--ref",
        "panel_purpose=advise",
        "--ref",
        "panel_location=repository",
        "--ref",
        &format!(
            "panel_thread={}",
            plan["thread"].as_str().unwrap_or_default()
        ),
        "--then",
        "starting",
        "--then",
        "working",
    ]);
    if let Some(main) = &request.main_attempt {
        start.extend(["--parent-attempt-id".into(), main.into()]);
    }
    if let Some(room) = &request.room {
        start.extend(["--ref".into(), format!("panel_room_id={room}").into()]);
    }
    if let Some(participant) = &request.participant {
        start.extend([
            "--ref".into(),
            format!("panel_room_participant={participant}").into(),
        ]);
    }
    let started = service.command(crate::NativeCommand::Events, repo, &start, stop)?;
    started.success()?;
    let attempt = String::from_utf8(started.stdout)
        .map_err(Error::new)?
        .trim()
        .to_owned();
    crate::identifier(&attempt)?;
    let argv = crate::value::array(&plan["argv"])
        .iter()
        .map(|v| {
            v.as_str()
                .map(std::ffi::OsString::from)
                .ok_or_else(|| Error::new("malformed council argv"))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut execution = crate::services::ExecutionRequest::new(argv, repo);
    execution.heartbeat = Some(crate::services::ActivityHeartbeat {
        repo: repo.to_path_buf(),
        attempt: attempt.clone(),
        actor: "panel-council".into(),
    });
    execution.remove_environment = crate::services::args(&["OMS_ATTEMPT_ID", "OMS_COUNCIL_LENSES"]);
    execution.environment = vec![
        ("OMS_TASK_ID".into(), request.task_id.clone().into()),
        ("OMS_PANEL_DISPATCH".into(), "1".into()),
        ("OMS_PANEL_ROLE".into(), "advisor".into()),
        ("OMS_PANEL_STRATEGY".into(), "advisor".into()),
        ("OMS_PANEL_PURPOSE".into(), "advise".into()),
        ("OMS_PANEL_WORKLOAD".into(), "main".into()),
        ("OMS_PANEL_LABEL".into(), request.label.clone().into()),
    ];
    if let Some(lenses) = &request.lenses {
        execution
            .environment
            .push(("OMS_COUNCIL_LENSES".into(), lenses.join(",").into()));
    }
    let outcome = service.command_context(crate::NativeCommand::PeerAsk, &execution, stop);
    let output = outcome.map_err(|e| Error {
        publication_uncertain: true,
        ..Error::new(format!(
            "council {attempt} execution unconfirmed; preserve live lifecycle and reconcile: {}",
            e.message
        ))
    })?;
    require(
        output.cleanup_complete,
        "council cleanup unconfirmed; preserve live lifecycle and reconcile",
    )
    .map_err(|e| Error {
        publication_uncertain: true,
        ..e
    })?;
    let passed = output.success().is_ok();
    let cancelled = stop.load(std::sync::atomic::Ordering::Acquire) || output.code == 130;
    let mut finish = crate::services::args(&["transition", "--attempt", &attempt, "--state"]);
    if passed {
        finish.extend(crate::services::args(&[
            "verifying",
            "--then",
            "review",
            "--then",
            "done",
            "--reason-code",
            "council_complete",
        ]));
    } else {
        finish.extend(crate::services::args(&[
            if cancelled { "cancelled" } else { "failed" },
            "--reason-code",
            "council_incomplete",
        ]));
    }
    service
        .command(
            crate::NativeCommand::Events,
            repo,
            &finish,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .and_then(|v| v.success())
        .map_err(|e| Error {
            publication_uncertain: true,
            ..Error::new(format!(
                "council {attempt} requires lifecycle reconciliation: {}",
                e.message
            ))
        })?;
    output.success()?;
    plan["executes"] = true.into();
    plan["attempt_id"] = attempt.into();
    plan["exit"] = output.code.into();
    Ok(plan)
}

pub fn stopped_worker_proof(repo: &Path, worker: &Value, attempts: &[Value]) -> Result<Value> {
    crate::control::stopped(repo, worker, attempts)
}

#[derive(Debug, Clone, Default)]
pub struct SettleReviewRequest {
    pub owner: Option<String>,
    pub owner_attempt: Option<String>,
    pub explicit: Vec<String>,
    pub reason: Option<String>,
    pub apply: bool,
    pub legacy: bool,
    pub legacy_older_than: Option<f64>,
    pub standalone: bool,
    pub older_than: Option<f64>,
    pub aged_days: Option<f64>,
}

pub fn settle_review(
    repo: &Path,
    request: &SettleReviewRequest,
    service: &dyn crate::Services,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<Value> {
    use crate::services::{NativeCommand, query};
    use crate::value::{array, text};
    use std::collections::{BTreeMap, BTreeSet};

    crate::parent_only()?;
    if let Some(owner) = &request.owner {
        crate::identifier(owner)?;
    }
    if let Some(att) = &request.owner_attempt {
        crate::identifier(att)?;
    }
    for ident in &request.explicit {
        crate::identifier(ident)?;
    }
    require(
        !request.legacy || request.owner_attempt.is_some(),
        "--legacy requires --owner-attempt",
    )?;
    require(
        request.legacy_older_than.is_none() || request.legacy,
        "--legacy-older-than requires --legacy",
    )?;
    require(
        !request.standalone
            || (request.owner.is_none()
                && request.owner_attempt.is_none()
                && request.explicit.is_empty()
                && request.reason.is_none()
                && !request.legacy),
        "--standalone cannot be combined with owner or attempt selection",
    )?;
    if let Some(days) = request.aged_days {
        require(
            days.is_finite() && days >= 7.0,
            "--aged-days must be finite and at least 7",
        )?;
        require(
            request.older_than.is_none() && request.explicit.is_empty() && request.reason.is_none(),
            "--aged-days cannot be combined with --older-than, --attempt or --reason",
        )?;
    }
    require(
        request.older_than.is_none() || request.standalone,
        "--older-than requires --standalone",
    )?;
    require(
        !request.standalone || request.older_than.is_some() || request.aged_days.is_some(),
        "--standalone requires --older-than DAYS or --aged-days DAYS",
    )?;
    for (flag, val) in [
        ("--legacy-older-than", request.legacy_older_than),
        ("--older-than", request.older_than),
    ] {
        if let Some(hours_or_days) = val {
            require(
                hours_or_days.is_finite() && hours_or_days > 0.0,
                &format!("{flag} must be finite and positive"),
            )?;
        }
    }
    if !request.explicit.is_empty() {
        let reason = request.reason.as_deref().unwrap_or("");
        require(
            !reason.is_empty() && reason.len() <= 200 && !reason.chars().any(|c| (c as u32) < 32),
            "--attempt requires a one-line --reason of 1..200 characters",
        )?;
    } else {
        require(request.reason.is_none(), "--reason requires --attempt")?;
    }

    let store = Store::open(repo)?;
    let _tx = if request.apply {
        Some(store.transaction("panel-results", Duration::from_secs(10))?)
    } else {
        None
    };

    let attempts_val = query(
        service,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    let attempts = array(&attempts_val);
    let artifacts = crate::results::index_rows(repo)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(Error::new)?
        .as_secs_f64();

    let plan_path = store.repo().join(".oms/plan/tasks.json");
    let plan_exists = std::fs::symlink_metadata(&plan_path).is_ok();
    let plan = if plan_exists {
        store.json(Path::new(".oms/plan/tasks.json"), 4 * 1024 * 1024, true)
    } else {
        Ok(None)
    };
    let (plan_unreadable, plan_id) = match plan {
        Ok(Some(v)) => match v["plan_id"].as_str() {
            Some(id) if !id.is_empty() => (false, Some(id.to_owned())),
            _ => (true, None),
        },
        Ok(None) => (plan_exists, None),
        Err(_) => (true, None),
    };

    let mut parents = BTreeSet::new();
    let mut legacy_parents = BTreeSet::new();
    let legacy_hours = request
        .legacy
        .then(|| request.legacy_older_than.unwrap_or(24.0));
    if !request.standalone {
        for row in attempts
            .iter()
            .filter(|v| v["tool"] == "panel-main" || v["refs"]["panel_role"] == "main")
        {
            let aid = text(row, "attempt_id");
            if aid.is_empty() {
                continue;
            }
            if let Some(req_att) = &request.owner_attempt
                && &aid != req_att
            {
                continue;
            }
            let row_owner = row["refs"]["panel_owner"]
                .as_str()
                .filter(|v| !v.is_empty())
                .or_else(|| row["provider"].as_str())
                .unwrap_or_default();
            if let Some(req_owner) = &request.owner
                && row_owner != req_owner
            {
                continue;
            }
            let dead = crate::state::TERMINAL_STATES.contains(&text(row, "state").as_str());
            if !dead {
                parents.insert(aid);
            } else if let Some(hours) = legacy_hours {
                let age_hours = crate::metrics::stamp(&row["created_at"])
                    .map(|t| (now - t) / 3600.0)
                    .unwrap_or(0.0);
                if age_hours >= hours {
                    parents.insert(aid.clone());
                    legacy_parents.insert(aid);
                }
            }
        }
    }

    let mut decisions: BTreeMap<String, (Value, Value)> = BTreeMap::new();
    if request.aged_days.is_none() && !request.standalone {
        for row in &artifacts {
            if row["kind"] != "panel-result" {
                continue;
            }
            let task = text(row, "task_id");
            if task.is_empty() {
                continue;
            }
            if let Ok(bytes) = crate::results::indexed_artifact(&store, row)
                && let Ok(payload) = crate::decode(&bytes, 1024 * 1024)
            {
                let main_att = text(&payload, "main_attempt_id");
                if !main_att.is_empty() && parents.contains(&main_att) {
                    decisions.insert(task, (payload, row.clone()));
                }
            }
        }
    }

    let explicit_set: BTreeSet<&str> = request.explicit.iter().map(String::as_str).collect();
    let eligible_states: &[&str] = if request.aged_days.is_some() {
        &["working", "verifying", "blocked", "review"]
    } else {
        &["review"]
    };

    let mut report = serde_json::json!({
        "candidates": [],
        "pending": [],
        "errors": [],
        "applied": [],
        "superseded": [],
        "terminal": 0u64,
        "executes": request.apply,
    });
    let mut terminal_count = 0u64;

    for row in attempts {
        if row["refs"]["panel_role"] != "worker" || row["refs"]["panel_access"] != "write" {
            continue;
        }
        let ident = text(row, "attempt_id");
        let parent = text(row, "parent_attempt_id");
        let state = text(row, "state");
        if crate::state::TERMINAL_STATES.contains(&state.as_str()) {
            terminal_count += 1;
        }
        let explicit_chosen = explicit_set.contains(ident.as_str());
        if !request.standalone && !parents.contains(&parent) && !explicit_chosen {
            continue;
        }
        if !eligible_states.contains(&state.as_str()) && !explicit_chosen {
            continue;
        }
        let task = text(row, "task_id");
        let landed = artifacts.iter().any(|art| {
            art["kind"] == "land-patch"
                && (art["attempt_id"] == ident || (!task.is_empty() && art["task_id"] == task))
        });

        let mut target: Option<(&str, &str)> = None;
        let mut selection_reason: Option<String> = None;

        if landed {
            target = Some(("done", "landed_patch"));
            selection_reason = Some("landed_patch".into());
        } else if let Some(days) = request.aged_days {
            let stamp = crate::metrics::stamp(&row["updated_at"])
                .or_else(|| crate::metrics::stamp(&row["created_at"]));
            let age_days = stamp.map(|t| (now - t) / 86400.0).unwrap_or(0.0);
            if age_days >= days {
                target = Some(("cancelled", "aged_unsettled"));
                selection_reason =
                    Some(format!("aged {state} without owner decision ({days} days)"));
            }
        } else if request.standalone {
            if let Some(days) = request.older_than {
                let stamp = crate::metrics::stamp(&row["updated_at"])
                    .or_else(|| crate::metrics::stamp(&row["created_at"]));
                let age_days = stamp.map(|t| (now - t) / 86400.0).unwrap_or(0.0);
                if age_days >= days {
                    target = Some(("cancelled", "standalone_older"));
                    selection_reason = Some(format!("standalone review older than {days} days"));
                }
            }
        } else if explicit_chosen {
            target = Some(("cancelled", "owner_explicit_settle"));
            selection_reason = request.reason.clone();
        } else if let Some((payload, _)) = decisions.get(&task) {
            let same_plan = plan_id
                .as_ref()
                .is_none_or(|id| payload["plan_id"].as_str().is_none_or(|p| p == id));
            if same_plan {
                let superseded = crate::value::array(&payload["superseded_attempts"])
                    .iter()
                    .any(|v| v["attempt_id"] == ident);
                if superseded {
                    target = Some(("cancelled", "owner_superseded"));
                    selection_reason = Some("owner_superseded".into());
                    report["superseded"]
                        .as_array_mut()
                        .unwrap()
                        .push(ident.clone().into());
                } else {
                    match payload["outcome"].as_str() {
                        Some("accepted") => {
                            target = Some(("done", "owner_accepted"));
                            selection_reason = Some("owner_accepted".into());
                        }
                        Some("completed") => {
                            target = Some(("done", "owner_completed"));
                            selection_reason = Some("owner_completed".into());
                        }
                        Some("rejected") => {
                            target = Some(("cancelled", "owner_rejected"));
                            selection_reason = Some("owner_rejected".into());
                        }
                        _ => {}
                    }
                }
            }
        }

        if plan_unreadable && target.is_some() && !landed {
            target = None;
        }
        let stopped_ok = crate::control::stopped(repo, row, attempts).is_ok();
        let exit_proven = artifacts
            .iter()
            .any(|art| art["attempt_id"] == ident && art["exit"].is_i64())
            || row["exit"].is_i64();

        if let Some((dest_state, reason_code)) = target
            && eligible_states.contains(&state.as_str())
            && (stopped_ok
                || exit_proven
                || landed
                || legacy_parents.contains(&parent)
                || request.standalone)
        {
            report["candidates"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({
                    "attempt_id": ident,
                    "task_id": task,
                    "from_state": state,
                    "to_state": dest_state,
                    "reason_code": reason_code,
                    "reason": selection_reason,
                }));
            if request.apply {
                let apply_res = (|| -> Result<()> {
                    let seq = row["sequence"]
                        .as_u64()
                        .ok_or_else(|| Error::new("worker sequence unavailable"))?;
                    let key = format!(
                        "settle-review-{}",
                        crate::digest(
                            format!("{ident}:{state}:{dest_state}:{reason_code}").as_bytes()
                        )
                    );
                    let mut args = crate::services::args(&[
                        "transition",
                        "--attempt",
                        &ident,
                        "--state",
                        dest_state,
                        "--expected-state",
                        &state,
                        "--expected-seq",
                        &seq.to_string(),
                        "--actor",
                        "panel-review-settlement",
                        "--reason-code",
                        reason_code,
                        "--idempotency-key",
                        &key,
                    ]);
                    if let Some(reason_text) = &selection_reason {
                        args.extend(["--reason".into(), reason_text.into()]);
                    }
                    service
                        .command(NativeCommand::Events, repo, &args, stop)?
                        .success()
                })();
                match apply_res {
                    Ok(()) => report["applied"].as_array_mut().unwrap().push(ident.into()),
                    Err(e) => report["errors"]
                        .as_array_mut()
                        .unwrap()
                        .push(serde_json::json!({
                            "attempt_id": ident,
                            "error": crate::eventlog::clean(&e.message, 240),
                        })),
                }
            }
        } else {
            let pending_reason = if target.is_none() {
                if plan_unreadable {
                    "plan_unreadable"
                } else {
                    "rework"
                }
            } else if state == "blocked" {
                "aged_days_required"
            } else {
                "not_in_review"
            };
            let mut item = serde_json::json!({
                "attempt_id": ident,
                "task_id": task,
                "state": state,
                "reason": pending_reason,
            });
            if let Some(days) = request.aged_days {
                item["aged_days"] = days.into();
            } else if let Some(days) = request.older_than {
                item["older_than_days"] = days.into();
            }
            report["pending"].as_array_mut().unwrap().push(item);
        }
    }
    report["terminal"] = terminal_count.into();
    Ok(report)
}
