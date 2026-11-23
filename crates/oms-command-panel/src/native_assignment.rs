//! Opt-in native assignments use Plan, Room and lifecycle writers as owners.
//! A transport receipt is not provider consumption, task completion or isolation.
use super::*;
use oms_command_operations as events;
use oms_command_operations::value::{V, obj};
use oms_panel_core::services::ExecutionRequest;

const MAX_BINDING: usize = 64 * 1024;
// Codex leads launch without the provider policy, so the assignment carries it.
const LEAD_DUTIES: &str = "Integrate only; children: roles/disjoint scopes/checks, read-only research/review/verify, builders assigned paths, no delegate/commit/push/land/install/publish; simple: cheaper supported native model, escalate scope/risk, keep explicit user choice/account/isolation/spend.";

pub(super) const PRIMARY_TICKET_LAYOUT: &str = concat!(
    "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t",
    "#{pane_in_mode}\t#{pane_active}\t#{window_active}\t#{pane_width}\t.",
);

// Leave room for the prompt marker and composer margins. The exact post-paste
// check still owns the decision to send Enter.
const TICKET_MARGIN: usize = 8;

pub(super) struct PrimaryTicket<'a> {
    pub(super) prompt: String,
    path: PathBuf,
    bytes: Vec<u8>,
    lead: PaneIdentity,
    columns: usize,
    transaction: &'a Transaction<'a>,
}

impl<'a> PrimaryTicket<'a> {
    pub(super) fn prepare(
        panel: &Panel<'_>,
        custody: &CodexAssignment,
        transaction: &'a Transaction<'a>,
        turn: &str,
        payload: &str,
    ) -> Result<Option<Self>> {
        let columns = primary_ticket_columns(panel, &custody.lead)?;
        let limit = columns.saturating_sub(TICKET_MARGIN);
        if crate::render::width(payload) <= limit {
            return Ok(None);
        }
        let path = PathBuf::from(format!(".oms/artifacts/native-tasks/{turn}.json"));
        let absolute = custody.repo.join(&path);
        let absolute = absolute
            .to_str()
            .ok_or_else(|| Error::new("repository cannot be represented in a task reference"))?;
        let prompt = format!(
            "{} Turn {turn}. You own this provider's work. Read {} for the assignment; stop if unavailable.",
            crate::native_children::TASK_PREFIX,
            assignment_shell_word(absolute),
        );
        if !prompt.is_ascii() || crate::render::width(&prompt) > limit {
            return Ok(None);
        }
        let pane = |pane: &PaneIdentity| json!({"id": pane.id, "window": pane.window, "pid": pane.pid, "birth": pane.birth});
        let packet = json!({
            "schema": 1, "kind": "oms-primary-codex-task", "turn": turn,
            "repo": custody.repo, "room": panel.room, "provider": "codex",
            "main_attempt": panel.attempt, "main_participant": panel.participant,
            "main_owner": panel.owner, "session": panel.session_proof,
            "main_pane": pane(&custody.main), "lead_pane": pane(&custody.lead),
            "account": custody.binding, "payload": payload,
            "payload_sha256": oms_panel_core::digest(payload.as_bytes()),
        });
        let bytes = serde_json::to_vec(&packet).map_err(Error::new)?;
        // This private file needs the physical repository locator. Refuse any
        // secret the environment-aware scrubber would remove.
        for original in [payload.as_bytes(), bytes.as_slice()] {
            require(
                oms_panel_core::redact(original)?.as_bytes() == original,
                "Codex task reference content refused by scrubber",
            )?;
        }
        transaction.write(&path, &bytes, true, MAX_BINDING)?;
        let ticket = Self {
            prompt,
            path,
            bytes,
            lead: custody.lead.clone(),
            columns,
            transaction,
        };
        ticket.revalidate(panel)?;
        Ok(Some(ticket))
    }

    pub(super) fn revalidate(&self, panel: &Panel<'_>) -> Result<()> {
        require(
            primary_ticket_columns(panel, &self.lead)? == self.columns,
            "Codex task reference layout changed",
        )?;
        require(
            self.transaction
                .read(&self.path, MAX_BINDING, true)
                .is_ok_and(|bytes| bytes == self.bytes),
            "Codex task reference is missing or changed",
        )
    }
}

fn primary_ticket_columns(panel: &Panel<'_>, lead: &PaneIdentity) -> Result<usize> {
    let layout = panel.tmux(&[
        "display-message",
        "-p",
        "-t",
        &lead.id,
        PRIMARY_TICKET_LAYOUT,
    ])?;
    let fields: Vec<_> = layout.split('\t').collect();
    let [
        session,
        window,
        pane,
        pid,
        "0",
        "0",
        active,
        focused,
        columns,
        ".",
    ] = fields.as_slice()
    else {
        return Err(Error::new("Codex task reference layout is unavailable"));
    };
    require(
        *session == panel.session
            && *window == lead.window
            && *pane == lead.id
            && pid.parse::<u32>() == Ok(lead.pid)
            && matches!((*active, *focused), ("0", "0" | "1") | ("1", "0")),
        "Codex task reference pane changed",
    )?;
    columns
        .parse::<u16>()
        .ok()
        .filter(|columns| *columns > 0)
        .map(usize::from)
        .ok_or_else(|| Error::new("Codex task reference layout is unavailable"))
}

/// Codex shows a larger paste as an opaque placeholder that its composer proof
/// cannot read, so the whole prompt must fit the visible limit untruncated.
fn assignment_prompt(
    repo: &Path,
    room: &str,
    task: &Value,
    turn: &str,
    message: &str,
    brief: &str,
) -> Result<String> {
    let repo = repo
        .to_str()
        .ok_or_else(|| Error::new("repository cannot be represented in a Room locator"))?;
    // One shell variable keeps the state repository pinned without repeating it.
    let fixed = format!(
        "{} Turn {turn}. You own this provider's work. {LEAD_DUTIES} `R={}; oms agent-plan --repo \"$R\" evidence-snapshot --id {}; oms room show --repo \"$R\" --id {} --exact --json`. plan {}, lease {}, Room message {message}; obey paths/accept/verifier; report evidence/unverified/model/unknown/binding; receipt!=accept. Task: ",
        crate::native_children::TASK_PREFIX,
        assignment_shell_word(repo),
        assignment_shell_word(text(task, "id")),
        assignment_shell_word(room),
        text(task, "plan_id"),
        text(task, "lease_id"),
    );
    let space = CODEX_VISIBLE_PASTE_CHARS.saturating_sub(fixed.chars().count());
    require(
        space > 0,
        "native assignment binding alone fills the visible Codex prompt; shorten the repository path, Room or task id",
    )?;
    let over = brief.chars().count().saturating_sub(space);
    if over > 0 {
        return Err(Error::new(format!(
            "native assignment brief is {over} characters over the {space} that fit the {CODEX_VISIBLE_PASTE_CHARS}-character visible Codex prompt; shorten it"
        )));
    }
    Ok(fixed + brief)
}

fn operation_value(value: &Value) -> Result<V> {
    V::parse(&serde_json::to_vec(value).map_err(Error::new)?).map_err(Error::new)
}

fn task(panel: &Panel<'_>, id: &str) -> Result<Value> {
    let status = commands::query(
        panel.services,
        NativeCommand::Plan,
        panel.repo,
        &["status", "--json"],
        panel.stop,
    )?;
    require(
        status["contract"]["satisfied"] == true,
        "the current project contract blocks native assignment",
    )?;
    let mut task = commands::query(
        panel.services,
        NativeCommand::Plan,
        panel.repo,
        &["evidence-snapshot", "--id", id],
        panel.stop,
    )?;
    require(
        task["id"] == id && task["contract_stale"] == false,
        "native assignment task is unavailable or its contract is stale",
    )?;
    // A heartbeat or elapsed age must not mint another assignment for this lease.
    for field in [
        "claim_age_s",
        "task_sha256",
        "created",
        "updated",
        "claimed_at",
    ] {
        task.as_object_mut()
            .ok_or_else(|| Error::new("invalid canonical task"))?
            .remove(field);
    }
    Ok(task)
}

fn claimed(panel: &Panel<'_>, task: &Value) -> Result<()> {
    require(
        ["claimed", "running"].contains(&text(task, "state"))
            && task["claimed_by_participant"] == panel.attempt
            && task["provider"] == panel.owner
            && task["claim_expired"] == false
            && text(task, "plan_id")
                .strip_prefix("plan_")
                .is_some_and(|v| v.len() == 32 && v.bytes().all(|c| c.is_ascii_hexdigit()))
            && text(task, "lease_id")
                .strip_prefix("lease_")
                .is_some_and(|v| v.len() == 32 && v.bytes().all(|c| c.is_ascii_hexdigit()))
            && task["lease_epoch"].as_u64().is_some_and(|n| n > 0),
        "native assignment requires the current Main's unexpired canonical task claim",
    )
}

fn git(panel: &Panel<'_>, cwd: &Path, argument: &str) -> Result<String> {
    let mut request =
        ExecutionRequest::new(commands::strings(&["git", "rev-parse", argument]), cwd);
    request.timeout = Duration::from_secs(10);
    request.output_limit = 4096;
    let output = panel.services.execute(&request, panel.stop)?;
    output.success()?;
    let value = String::from_utf8(output.stdout).map_err(Error::new)?;
    let value = value.trim_end_matches(['\r', '\n']);
    require(
        !value.is_empty() && !value.chars().any(char::is_control),
        "native checkout identity is unavailable",
    )?;
    Ok(value.to_owned())
}

fn checkout(panel: &Panel<'_>, lead: &Pane) -> Result<Value> {
    let raw = panel.tmux(&[
        "display-message",
        "-p",
        "-t",
        &lead.id,
        "#{pane_current_path}",
    ])?;
    require(
        raw.len() <= 4096 && !raw.chars().any(char::is_control),
        "native cwd is unavailable",
    )?;
    let cwd = Path::new(&raw).canonicalize().map_err(Error::new)?;
    require(cwd.is_absolute(), "native cwd is unavailable")?;
    let worktree = PathBuf::from(git(panel, &cwd, "--show-toplevel")?)
        .canonicalize()
        .map_err(Error::new)?;
    let common = cwd
        .join(git(panel, &cwd, "--git-common-dir")?)
        .canonicalize()
        .map_err(Error::new)?;
    let expected = panel
        .repo
        .join(git(panel, panel.repo, "--git-common-dir")?)
        .canonicalize()
        .map_err(Error::new)?;
    require(
        common == expected,
        "native lead cwd is not a worktree of the selected repository",
    )?;
    let base = git(panel, &cwd, "HEAD")?;
    require(
        (base.len() == 40 || base.len() == 64) && base.bytes().all(|b| b.is_ascii_hexdigit()),
        "native checkout has no exact base SHA",
    )?;
    Ok(json!({"cwd": cwd, "worktree": worktree, "base_sha": base,
        "write_isolation": "unmanaged", "native_session": null}))
}

pub(super) struct Binding {
    task: Value,
    checkout: Value,
    attempt: String,
    intent_digest: String,
    digest: String,
    transport: &'static str,
}
impl Binding {
    pub(super) fn revalidate(&self, panel: &Panel<'_>) -> Result<()> {
        let current = task(panel, text(&self.task, "id"))?;
        claimed(panel, &current)?;
        require(
            current == self.task,
            "native assignment task, lease or criteria changed before input",
        )?;
        let operation = current_operation(panel, &self.attempt)?;
        require(
            operation["terminal"] == false
                && operation["refs"]["native_intent_sha256"] == self.intent_digest
                && operation["refs"]["native_transport"] == self.transport
                && (self.transport == "intent"
                    || operation["refs"]["native_binding_sha256"] == self.digest),
            "native assignment operation was cancelled or changed before input",
        )?;
        let lead = panel
            .leads
            .get("codex")
            .ok_or_else(|| Error::new("native lead disappeared"))?;
        require(
            checkout(panel, lead)? == self.checkout,
            "native assignment cwd or base changed before input",
        )
    }
}

fn annotate(panel: &Panel<'_>, binding: &Binding, key: &str, refs: Value) -> Result<()> {
    let attempt = &binding.attempt;
    let candidate = events::new_event(
        attempt,
        0usize.into(),
        "attempt.usage",
        obj([
            (
                "actor",
                obj([
                    ("kind", "owner".into()),
                    ("name", "panel-native-assignment".into()),
                ]),
            ),
            ("idempotency_key", key.into()),
            ("refs", operation_value(&refs)?),
        ]),
    )
    .map_err(Error::new)?;
    events::append_lifecycle_checked(panel.repo, candidate, panel.stop, |_, current| {
        let current =
            current.ok_or_else(|| "native assignment operation disappeared".to_owned())?;
        if current.get("terminal").truth()
            || current.get("refs").get("native_intent_sha256").py() != binding.intent_digest
            || current.get("refs").get("native_transport").py() != binding.transport
            || (binding.transport != "intent"
                && current.get("refs").get("native_binding_sha256").py() != binding.digest)
        {
            return Err("native assignment operation was cancelled or changed".into());
        }
        Ok(())
    })
    .map_err(Error::new)?;
    Ok(())
}

fn current_operation(panel: &Panel<'_>, attempt: &str) -> Result<Value> {
    commands::query(
        panel.services,
        NativeCommand::Events,
        panel.repo,
        &["show", "--attempt", attempt, "--json"],
        panel.stop,
    )
}

fn result(attempt: &str, binding: &str, message: &str, status: &str, repeated: bool) -> Value {
    json!({"schema": 1, "status": status, "attempt_id": attempt,
        "binding": binding, "room_message_id": message, "reused": repeated,
        "consumption": "unconfirmed", "task_completion": "unconfirmed",
        "write_isolation": "unmanaged", "child_cleanup": "unsupported"})
}

/// Importing a report never sends native input or settles the native execution.
pub(super) fn register_result(
    panel: &Panel<'_>,
    options: &Options,
    host: &dyn AccountHost,
    attempts: &AttemptCache,
    tx: &Transaction<'_>,
) -> Result<Value> {
    let lead = panel
        .leads
        .get("codex")
        .ok_or_else(|| Error::new("native result requires the original ordinary Codex lead"))?;
    let capture = oms_panel_core::native_results::Capture::read(
        panel.repo,
        Path::new(&options.required("native-result")?),
    )?;
    let artifact = format!(
        ".oms/lifecycle/attempts/{}/native-assignment.json",
        capture.attempt()
    );
    let raw = tx.read(Path::new(&artifact), MAX_BINDING, false)?;
    require(
        oms_panel_core::digest(&raw) == capture.binding_digest(),
        "native result assignment digest changed",
    )?;
    let packet: Value = serde_json::from_slice(&raw).map_err(Error::new)?;
    let operation = current_operation(panel, capture.attempt())?;
    require(
        packet["schema"] == 1
            && packet["kind"] == "oms-native-assignment"
            && packet["provider"] == "codex"
            && packet["attempt_id"] == capture.attempt()
            && packet["parent_attempt_id"] == panel.attempt
            && packet["room"] == panel.room
            && packet["task"]["id"] == options.required("task-id")?
            && operation["tool"] == "panel-native-assignment"
            && operation["provider"] == "codex"
            && operation["parent_attempt_id"] == panel.attempt
            && operation["task_id"] == packet["task"]["id"]
            && operation["refs"]["native_assignment_schema"] == "1"
            && operation["refs"]["native_binding_sha256"] == capture.binding_digest()
            && operation["refs"]["plan_id"] == packet["task"]["plan_id"]
            && operation["refs"]["lease_id"] == packet["task"]["lease_id"]
            && operation["refs"]["panel_room_id"] == panel.room,
        "native result does not belong to this task, Main, lease or Room",
    )?;
    let transport = match text(&operation["refs"], "native_transport") {
        "submitted" => "submitted",
        "sending" => "sending",
        "unknown" => "unknown",
        _ => return Err(Error::new("native result requires a retained send intent")),
    };
    let mut intent = packet.clone();
    intent
        .as_object_mut()
        .ok_or_else(|| Error::new("invalid native result binding"))?
        .remove("attempt_id");
    let intent_digest = oms_panel_core::digest(&serde_json::to_vec(&intent).map_err(Error::new)?);
    require(
        operation["refs"]["native_intent_sha256"] == intent_digest,
        "native result intent digest changed",
    )?;
    let binding = Binding {
        task: packet["task"].clone(),
        checkout: packet["checkout"].clone(),
        attempt: capture.attempt().into(),
        intent_digest,
        digest: capture.binding_digest().into(),
        transport,
    };
    let revalidate = || -> Result<()> {
        let fresh = panel.reload()?;
        let same_pane = |left: &Pane, right: &Pane| {
            left.id == right.id && left.window == right.window && left.pid == right.pid
        };
        require(
            fresh.room == panel.room
                && same_pane(&fresh.main, &panel.main)
                && fresh
                    .leads
                    .get("codex")
                    .is_some_and(|current| same_pane(current, lead))
                && fresh.session_proof == panel.session_proof
                && fresh.attempt == panel.attempt
                && fresh.participant == panel.participant
                && fresh.owner == panel.owner,
            "native result Main, Codex lead, or session changed",
        )?;
        fresh.require_codex_assignment_main(fresh.repo, attempts)?;
        fresh.execute_with_host_cached(
            "codex",
            options.get("account"),
            None,
            LeadLaunch::default(),
            LeadExecution {
                host,
                attempts,
                transaction: Some(tx),
            },
        )?;
        let custody = fresh.codex_assignment_custody(host)?;
        let pane =
            |p: &PaneIdentity| json!({"id":p.id,"window":p.window,"pid":p.pid,"birth":p.birth});
        let (_, _, verification) = binding_summary(custody.binding.as_ref());
        require(
            packet["session"] == serde_json::to_value(&fresh.session_proof).map_err(Error::new)?
                && packet["main_pane"] == pane(&custody.main)
                && packet["lead_pane"] == pane(&custody.lead)
                && packet["account"]
                    == serde_json::to_value(&custody.binding).map_err(Error::new)?
                && packet["account_verification"]
                    == serde_json::to_value(verification).map_err(Error::new)?,
            "native result session generation or account changed",
        )?;
        binding.revalidate(&fresh)?;
        tx.check()
    };
    revalidate()?;
    capture.validate_scope(&packet)?;
    let previous = oms_panel_core::native_results::read(&Store::open(panel.repo)?, &operation)?;
    require(
        previous
            .as_ref()
            .map_or(capture.previous_digest().is_empty(), |previous| {
                previous["artifact_sha256"] == capture.previous_digest()
                    || previous["input_sha256"] == capture.input_digest()
            }),
        "native result predecessor changed; retain and review the current report",
    )?;
    let retained = capture.retain(tx, &packet, &artifact)?;
    let digest = text(&retained, "sha256");
    let prior = text(&operation["refs"], "native_result_sha256");
    require(
        prior == digest || prior == capture.previous_digest(),
        "native result predecessor changed; retain and review the current report",
    )?;
    revalidate()?;
    let previous_operation = operation_value(&operation)?;
    let previous_refs = previous_operation.get("refs");
    let event = events::new_event(
        capture.attempt(),
        0usize.into(),
        "attempt.usage",
        obj([
            (
                "actor",
                obj([
                    ("kind", "owner".into()),
                    ("name", "panel-native-result".into()),
                ]),
            ),
            ("idempotency_key", format!("native-result-{digest}").into()),
            (
                "refs",
                operation_value(&json!({"native_result_schema":"1",
            "native_result_path":retained["path"],"native_result_sha256":digest}))?,
            ),
        ]),
    )
    .map_err(Error::new)?;
    let mut append_authorized = false;
    let appended = events::append_lifecycle_checked(panel.repo, event, panel.stop, |_, current| {
        let current = current.ok_or_else(|| "native result assignment disappeared".to_owned())?;
        let refs = current.get("refs");
        if current.get("terminal").truth()
            || ["tool", "provider", "parent_attempt_id", "task_id"]
                .iter()
                .any(|key| current.get(key) != previous_operation.get(key))
            || refs.get("native_binding_sha256").py() != binding.digest
            || refs.get("native_intent_sha256").py() != binding.intent_digest
            || refs.get("native_transport").py() != transport
            || [
                "native_assignment_schema",
                "plan_id",
                "lease_id",
                "panel_room_id",
                "native_result_schema",
                "native_result_path",
                "native_result_sha256",
            ]
            .iter()
            .any(|key| refs.get(key) != previous_refs.get(key))
        {
            return Err("native result assignment or predecessor changed".into());
        }
        append_authorized = true;
        Ok(())
    });
    appended.map_err(|error| Error {
        publication_uncertain: append_authorized,
        ..Error::new(error)
    })?;
    let confirm = || -> Result<()> {
        revalidate()?;
        let current = current_operation(panel, capture.attempt())?;
        require(
            current["refs"]["native_result_schema"] == "1"
                && current["refs"]["native_result_path"] == retained["path"]
                && current["refs"]["native_result_sha256"] == digest,
            "native result publication reference changed",
        )
    };
    confirm().map_err(|e| Error {
        publication_uncertain: true,
        message: format!(
            "native result may be recorded; reconcile its retained reference: {}",
            e.message
        ),
        ..e
    })?;
    Ok(
        json!({"schema":1,"status":"reported","attempt_id":capture.attempt(),"result":retained,
        "reused":prior == digest,"transport":transport,"consumption":"unconfirmed",
        "task_completion":"unconfirmed","write_isolation":"unmanaged","child_cleanup":"unsupported",
        "effective_permissions":"unknown",
        "acceptance_eligible":false}),
    )
}

pub(super) fn run(
    panel: &Panel<'_>,
    options: &Options,
    host: &dyn AccountHost,
    attempts: &AttemptCache,
    tx: &Transaction<'_>,
) -> Result<Value> {
    let id = options.required("task-id")?;
    let text_task = options.required("task")?;
    validate_task(&text_task)?;
    require(
        oms_scrub::scan_secret(text_task.as_bytes(), &[])
            .map_err(|_| Error::new("native assignment sensitive-content check unavailable"))?
            .is_empty(),
        "native assignment contains sensitive content",
    )?;
    // Canonical ids have fixed widths, so the exact prompt length is known before
    // any account, Plan, attempt, Room or tmux step.
    let zeros = "0".repeat(32);
    let widths =
        json!({"id": id, "plan_id": format!("plan_{zeros}"), "lease_id": format!("lease_{zeros}")});
    assignment_prompt(
        panel.repo,
        panel.room,
        &widths,
        &zeros[..20],
        &format!("msg-{zeros}"),
        &text_task,
    )?;
    require(
        panel.leads.contains_key("codex"),
        "open the ordinary Codex lead before binding a task",
    )?;
    // This existing path checks an explicitly selected account without sending.
    panel.execute_with_host_cached(
        "codex",
        options.get("account"),
        None,
        LeadLaunch::default(),
        LeadExecution {
            host,
            attempts,
            transaction: Some(tx),
        },
    )?;
    let custody = panel.codex_assignment_custody(host)?;
    panel.require_codex_assignment_main(panel.repo, attempts)?;
    let lead = &panel.leads["codex"];
    let location = checkout(panel, lead)?;
    let mut canonical = task(panel, &id)?;
    let history = attempts.attempts(panel.repo, panel.services, panel.stop)?;
    require(
        !crate::rows(&history).iter().any(|attempt| {
            attempt["tool"] == "panel-native-assignment"
                && attempt["task_id"] == id
                && attempt["refs"]["plan_id"] == canonical["plan_id"]
                && attempt["terminal"] == false
                && (attempt["refs"]["lease_id"] != canonical["lease_id"]
                    || attempt["parent_attempt_id"] != panel.attempt)
        }),
        "an earlier native assignment for this task remains live or unconfirmed; reconcile its existing attempt before a new lease or Main can dispatch",
    )?;
    if canonical["state"] == "ready" {
        let provider = canonical["assignment"]["provider"]
            .as_str()
            .unwrap_or(&panel.owner);
        require(
            provider == panel.owner,
            "task route cannot be claimed by this Main; use the canonical task owner",
        )?;
        panel
            .services
            .command(
                NativeCommand::Plan,
                panel.repo,
                &commands::strings(&["claim", "--id", &id, "--provider", &panel.owner]),
                panel.stop,
            )?
            .success()?;
        canonical = task(panel, &id)?;
    }
    claimed(panel, &canonical)?;
    let mut binding = Binding {
        task: canonical.clone(),
        checkout: location,
        attempt: String::new(),
        intent_digest: String::new(),
        digest: String::new(),
        transport: "intent",
    };
    let key = oms_panel_core::digest(
        &serde_json::to_vec(&json!({
            "main": panel.attempt, "plan": canonical["plan_id"], "task": id,
            "lease": canonical["lease_id"], "room": panel.room,
        }))
        .map_err(Error::new)?,
    );
    let turn = &key[..20];
    let message = format!("msg-{}", &key[..32]);
    let prompt = assignment_prompt(
        panel.repo, panel.room, &canonical, turn, &message, &text_task,
    )?;
    let payload_digest = oms_panel_core::digest(prompt.as_bytes());
    let (_, _, account_verification) = binding_summary(custody.binding.as_ref());
    let mut packet = json!({"schema": 1, "kind": "oms-native-assignment", "task": canonical,
        "parent_attempt_id": panel.attempt, "room": panel.room, "provider": "codex",
        "session": panel.session_proof, "main_pane": {"id": custody.main.id,
            "window": custody.main.window, "pid": custody.main.pid, "birth": custody.main.birth},
        "lead_pane": {"id": custody.lead.id, "window": custody.lead.window,
            "pid": custody.lead.pid, "birth": custody.lead.birth},
        "account": custody.binding, "account_verification": account_verification,
        "checkout": binding.checkout, "room_message_id": message,
        "payload_sha256": payload_digest, "payload": prompt,
        "result_contract": "preserve task/lease/attempt binding; existing review and acceptance are separate"});
    let raw = serde_json::to_vec(&packet).map_err(Error::new)?;
    require(
        raw.len() <= MAX_BINDING,
        "native assignment binding exceeds 64 KiB",
    )?;
    let intent_digest = oms_panel_core::digest(&raw);
    let attempt = events::create_attempt(
        panel.repo,
        &events::StartRequest {
            provider: "codex".into(),
            tool: "panel-native-assignment".into(),
            task_id: id,
            parent_attempt_id: panel.attempt.clone(),
            idempotency_key: format!("native-{key}"),
            refs: operation_value(&json!({"native_assignment_schema": "1",
            "native_intent_sha256": intent_digest, "native_payload_sha256": payload_digest,
            "plan_id": canonical["plan_id"], "lease_id": canonical["lease_id"],
            "panel_room_id": panel.room, "panel_owner": panel.owner,
            "panel_native_scheduling": "unmanaged", "native_transport": "intent"}))?,
            ..Default::default()
        },
        panel.stop,
    )
    .map_err(Error::new)?;
    packet["attempt_id"] = attempt.clone().into();
    let raw = serde_json::to_vec(&packet).map_err(Error::new)?;
    require(
        raw.len() <= MAX_BINDING,
        "native assignment binding exceeds 64 KiB",
    )?;
    let binding_digest = oms_panel_core::digest(&raw);
    binding.attempt = attempt.clone();
    binding.intent_digest = intent_digest;
    binding.digest = binding_digest.clone();
    let artifact = format!(".oms/lifecycle/attempts/{attempt}/native-assignment.json");
    tx.write(Path::new(&artifact), &raw, true, MAX_BINDING)?;
    let prior = current_operation(panel, &attempt)?;
    match text(&prior["refs"], "native_transport") {
        "submitted" => return Ok(result(&attempt, &artifact, &message, "submitted", true)),
        "sending" | "unknown" => return Ok(result(&attempt, &artifact, &message, "unknown", true)),
        "intent" => {}
        _ => return Err(Error::new("invalid native assignment transport state")),
    }
    binding.revalidate(panel)?;
    tx.check()?;
    let mut join = commands::strings(&[
        "join",
        "--id",
        panel.room,
        "--participant",
        &attempt,
        "--provider",
        "codex",
        "--role",
        "worker",
        "--parent",
        &panel.attempt,
        "--label",
        "Native task assignment",
    ]);
    for path in crate::rows(&canonical["allowed_paths"]) {
        let path = path
            .as_str()
            .ok_or_else(|| Error::new("invalid task scope"))?;
        join.extend(commands::strings(&["--owns", path]));
    }
    panel
        .services
        .command(NativeCommand::Room, panel.repo, &join, panel.stop)?
        .success()?;
    let notice = format!(
        "Native assignment {attempt}; task {}; plan {}; lease {}; binding {binding_digest}; payload {payload_digest}. Evidence: {artifact}. Input submission and consumption are separate observations.",
        text(&canonical, "id"),
        text(&canonical, "plan_id"),
        text(&canonical, "lease_id")
    );
    panel
        .services
        .command(
            NativeCommand::Room,
            panel.repo,
            &commands::strings(&[
                "send",
                "--id",
                panel.room,
                "--participant",
                &panel.attempt,
                "--to",
                &attempt,
                "--kind",
                "note",
                "--message-id",
                &message,
                "--text",
                &notice,
            ]),
            panel.stop,
        )?
        .success()?;
    binding.revalidate(panel)?;
    tx.check()?;
    // A known busy input has no transport side effect. Keep the resumable
    // intent; send_checked repeats this observation after the durable boundary.
    panel.require_provider_input("codex")?;
    // This durable boundary precedes set-buffer, paste and Enter. If the caller
    // disappears here, the next call only reports unknown; it cannot resend.
    annotate(
        panel,
        &binding,
        "native-send-intent",
        json!({
            "native_transport": "sending", "native_binding": artifact,
            "native_room_message": message, "native_binding_sha256": binding_digest,
        }),
    )?;
    binding.transport = "sending";
    let sent = panel.send_checked(
        "codex",
        &prompt,
        Proof::Turn(turn),
        Some((&custody, host, attempts)),
        Some(AssignmentEvidence::Bound(&binding)),
    );
    let state = if sent.is_ok() { "submitted" } else { "unknown" };
    let annotation = annotate(
        panel,
        &binding,
        "native-send-observed",
        json!({"native_transport": state}),
    );
    let state = if annotation.is_ok() { state } else { "unknown" };
    let mut output = result(&attempt, &artifact, &message, state, false);
    if sent.is_err() || annotation.is_err() {
        output["reconcile_required"] = true.into();
        output["reason"] = if let Err(error) = sent {
            error.message.into()
        } else {
            "input receipt could not be recorded".into()
        };
    }
    Ok(output)
}

// The canonical flow fixture lives in orchestrator.rs; these pin the prompt budget.
#[cfg(test)]
mod tests {
    use super::*;

    fn canonical() -> Value {
        json!({"id": "native-one", "plan_id": format!("plan_{}", "a".repeat(32)),
            "lease_id": format!("lease_{}", "b".repeat(32))})
    }

    fn build(repo: &str, brief: &str) -> Result<String> {
        let turn = "c".repeat(20);
        let message = format!("msg-{}", "d".repeat(32));
        assignment_prompt(
            Path::new(repo),
            "room-eeeeeeeeeeee",
            &canonical(),
            &turn,
            &message,
            brief,
        )
    }

    #[test]
    fn native_assignment_prompt_fits_ordinary_brief_with_exact_binding() {
        let repo = format!("/{}", "r".repeat(27));
        let brief = "Review the claimed source against its acceptance criteria and verifier, then report findings with file and line anchors, the checks you ran and any blockers to Overall Main now.";
        assert!(brief.len() >= 170);
        let prompt = build(&repo, brief).unwrap();
        assert!(prompt.chars().count() <= CODEX_VISIBLE_PASTE_CHARS);
        assert!(prompt.starts_with(&format!(
            "{} Turn {}. You own this provider's work. {LEAD_DUTIES} ",
            crate::native_children::TASK_PREFIX,
            "c".repeat(20)
        )));
        assert!(prompt.ends_with(&format!(" Task: {brief}")));
        for exact in [
            format!("plan plan_{},", "a".repeat(32)),
            format!("lease lease_{},", "b".repeat(32)),
            format!("Room message msg-{};", "d".repeat(32)),
        ] {
            assert_eq!(prompt.matches(&exact).count(), 1, "{exact}");
        }
        let lower = prompt.to_ascii_lowercase();
        for duty in [
            "roles",
            "disjoint scopes",
            "checks",
            "read-only",
            "assigned paths",
            "cheaper supported native model",
            "explicit user choice",
            "escalate",
            "scope",
            "risk",
            "account",
            "isolation",
            "spend",
            "model",
            "unknown",
        ] {
            assert!(lower.contains(duty), "{duty}");
        }
        for alias in [
            "haiku", "sonnet", "opus", "flash", "mini", "gpt-", "gemini-", "claude-", "o3", "o4",
        ] {
            assert!(
                !lower.contains(alias),
                "bound prompt must not hardcode unauthenticated model alias {alias:?}: {prompt}"
            );
        }
        // The one copyable reader line must keep unusual repository bytes intact;
        // octal-escaped bytes cost visible characters, hence the shorter brief.
        for repo in [repo, "/odd dir/it's `x` $HOME".to_owned()] {
            let prompt = build(&repo, "x").unwrap();
            let readers = prompt.split('`').nth(1).unwrap();
            let out = std::process::Command::new("bash")
                .args([
                    "-c",
                    &format!("oms() {{ printf '%s|' \"$@\"; echo; }}; {readers}"),
                ])
                .output()
                .unwrap();
            assert!(out.status.success());
            assert_eq!(
                String::from_utf8(out.stdout).unwrap(),
                format!(
                    "agent-plan|--repo|{repo}|evidence-snapshot|--id|native-one|\nroom|show|--repo|{repo}|--id|room-eeeeeeeeeeee|--exact|--json|\n"
                )
            );
        }
    }

    #[test]
    fn native_assignment_prompt_rejects_oversized_briefs_without_truncation() {
        let repo = format!("/{}", "r".repeat(27));
        let fixed = build(&repo, "").unwrap().chars().count();
        assert!(fixed <= 818, "{fixed}");
        let space = CODEX_VISIBLE_PASTE_CHARS - fixed;
        assert!(space >= 170, "{space}");
        // Counted as visible characters, not bytes.
        let full = "é".repeat(space);
        assert_eq!(
            build(&repo, &full).unwrap().chars().count(),
            CODEX_VISIBLE_PASTE_CHARS
        );
        let error = build(&repo, &format!("{full}x")).unwrap_err().message;
        assert!(
            error.contains(&format!("1 characters over the {space}")),
            "{error}"
        );
        let error = build(&format!("/{}", "r".repeat(CODEX_VISIBLE_PASTE_CHARS)), "x")
            .unwrap_err()
            .message;
        assert!(error.contains("binding alone"), "{error}");

        let tight_repo = format!("/{}", "r".repeat(29));
        let mut tight_task = canonical();
        tight_task["id"] = json!("tight-native-170");
        let tight_turn = "c".repeat(20);
        let tight_message = format!("msg-{}", "d".repeat(32));
        let tight_fixed = assignment_prompt(
            Path::new(&tight_repo),
            "room-eeeeeeeeeeee",
            &tight_task,
            &tight_turn,
            &tight_message,
            "",
        )
        .unwrap()
        .chars()
        .count();
        let tight_space = CODEX_VISIBLE_PASTE_CHARS - tight_fixed;
        assert!(tight_space >= 174, "{tight_space}");
        let tight_prompt = assignment_prompt(
            Path::new(&tight_repo),
            "room-eeeeeeeeeeee",
            &tight_task,
            &tight_turn,
            &tight_message,
            &"b".repeat(170),
        )
        .unwrap();
        assert!(tight_prompt.chars().count() <= CODEX_VISIBLE_PASTE_CHARS);
    }
}
