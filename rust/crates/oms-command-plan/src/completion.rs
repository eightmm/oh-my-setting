//! Parent-reviewed local completion, immutable evidence, and three-phase CAS.
use crate::engine::{history, lineage, marker_alive, marker_typed, parse_ts, plain, screen};
use crate::json::{self, Value, empty, list, number, object, string};
use crate::provenance::{self, History};
use crate::storage::{self, MAX_JSON, MAX_PATCH};
use crate::{Action, Plan, Request, Response, Result, Snapshot, error, require};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

#[derive(Debug, PartialEq, Eq)]
pub enum CompletionState {
    Verified,
    Unavailable,
    NotTyped,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    ResearchAccepted,
    MainResearchAccepted,
    SatisfiedBy,
}
impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::ResearchAccepted => "research-accepted",
            Self::MainResearchAccepted => "main-research-accepted",
            Self::SatisfiedBy => "satisfied-by",
        }
    }
    pub fn action(self) -> Action {
        match self {
            Self::ResearchAccepted => Action::AcceptResearch,
            Self::MainResearchAccepted => Action::AcceptMainResearch,
            Self::SatisfiedBy => Action::Satisfy,
        }
    }
    fn from_action(a: Action) -> Result<Self> {
        match a {
            Action::AcceptResearch => Ok(Self::ResearchAccepted),
            Action::AcceptMainResearch | Action::ApproveMainResearch => {
                Ok(Self::MainResearchAccepted)
            }
            Action::Satisfy => Ok(Self::SatisfiedBy),
            _ => Err(error("unsupported completion action")),
        }
    }
    fn parse(s: &str) -> Result<Self> {
        match s {
            "research-accepted" => Ok(Self::ResearchAccepted),
            "main-research-accepted" => Ok(Self::MainResearchAccepted),
            "satisfied-by" => Ok(Self::SatisfiedBy),
            _ => Err(error("unknown completion fields")),
        }
    }
}
pub(crate) fn relative(s: &str) -> Result<&str> {
    text(s, "repo-relative path", 300)?;
    require(
        !s.starts_with(['/', '\\'])
            && !s.contains(['\\', ':'])
            && s.split('/').all(|s| !matches!(s, "" | "." | "..")),
        "evidence paths must be normalized repo-relative paths",
    )?;
    Ok(s)
}
pub(crate) fn text<'a>(s: &'a str, label: &str, max: usize) -> Result<&'a str> {
    require(
        !s.trim().is_empty() && s.chars().count() <= max,
        format!("invalid {label}"),
    )?;
    plain(s, label)?;
    Ok(s)
}
pub(crate) fn reference(plan: &Plan<'_>, v: &Value) -> Result<Vec<u8>> {
    provenance::reference(
        plan,
        v,
        if v.text("path").ends_with(".patch") {
            MAX_PATCH
        } else {
            MAX_JSON
        },
    )
}
fn manifest_shape(manifest: &Value, envelope: &Value) -> Result<()> {
    let items = manifest.array()?;
    let paths = envelope.strings()?;
    require(
        (1..=256).contains(&items.len()) && (1..=256).contains(&paths.len()),
        "bounded explicit source envelope and manifest required",
    )?;
    let mut seen = BTreeSet::new();
    for item in items {
        json::keys(item, &["path", "state", "mode", "sha256"], &[])?;
        let rel = relative(item.get("path").str()?)?;
        require(
            !rel.starts_with(".oms/") && seen.insert(rel),
            "duplicate or state-directory source",
        )?;
        match item.text("state") {
            "file" => {
                json::digest(item.get("sha256"))?;
                require(
                    matches!(item.text("mode"), "100644" | "100755"),
                    "invalid source manifest mode",
                )?;
            }
            "absent" => require(
                item.get("mode") == &Value::Null && item.get("sha256") == &Value::Null,
                "invalid source manifest absence",
            )?,
            _ => return Err(error("unsupported source state")),
        }
    }
    for path in &paths {
        relative(path)?;
    }
    require(
        paths.iter().copied().collect::<BTreeSet<_>>() == seen && paths.len() == seen.len(),
        "source envelope and manifest differ",
    )
}
fn verifier(plan: &Plan<'_>, value: &Value, task: &Value, current: bool) -> Result<String> {
    json::keys(
        value,
        &["command", "files", "adopted", "review_summary"],
        &[],
    )?;
    let command = text(value.get("command").str()?, "verifier command", 4096)?;
    text(value.get("review_summary").str()?, "verifier review", 1000)?;
    require(
        matches!(value.get("adopted"), Value::Bool(_)),
        "verifier adoption must be boolean",
    )?;
    if task.get("verify").truth() {
        require(
            command == task.text("verify"),
            "verifier differs from actual task command",
        )?;
    } else {
        require(
            value.get("adopted") == &Value::Bool(true),
            "legacy empty verifier requires explicit reviewed adoption",
        )?;
    }
    let files = value.get("files").array()?;
    require(
        (1..=64).contains(&files.len()),
        "frozen verifier file manifest required",
    )?;
    let mut seen = BTreeSet::new();
    for item in files {
        json::keys(item, &["path", "state", "mode", "sha256"], &[])?;
        let path = relative(item.get("path").str()?)?;
        require(
            seen.insert(path)
                && item.text("state") == "file"
                && matches!(item.text("mode"), "100644" | "100755"),
            "invalid verifier manifest",
        )?;
        json::digest(item.get("sha256"))?;
        if current {
            let snap = plan
                .source
                .snapshot(path, 8 * 1024 * 1024, false)?
                .ok_or_else(|| error("verifier file missing"))?;
            require(
                json::sha(&snap.raw) == item.text("sha256")
                    && crate::repository::source_mode(snap.mode) == item.text("mode"),
                "verifier bytes or mode changed",
            )?;
        }
    }
    crate::floor::verifier_argv(command, &seen.into_iter().collect::<Vec<_>>())?;
    Ok(json::sha(command))
}
fn requirements(plan: &Plan<'_>, bundle: &Value, unavailable: Option<&Value>) -> Result<()> {
    let mapping = bundle.get("requirements").array()?;
    require(
        (1..=64).contains(&mapping.len()),
        "complete reviewed requirement/deliverable map required",
    )?;
    let mut ids = BTreeSet::new();
    let envelope = bundle.get("source_envelope").strings()?;
    for item in mapping {
        json::keys(
            item,
            &[
                "id",
                "obligation",
                "deliverable",
                "source_paths",
                "evidence",
            ],
            &[],
        )?;
        let name = text(item.get("id").str()?, "requirement id", 80)?;
        require(ids.insert(name), "duplicate requirement")?;
        text(
            item.get("obligation").str()?,
            "requirement obligation",
            1000,
        )?;
        text(
            item.get("deliverable").str()?,
            "requirement deliverable",
            1000,
        )?;
        let paths = item.get("source_paths").strings()?;
        require(
            !paths.is_empty() && paths.iter().all(|p| envelope.contains(p)),
            "each obligation needs specific source coverage within manifest",
        )?;
        let evidence = item.get("evidence").array()?;
        require(
            (1..=16).contains(&evidence.len()),
            "each obligation needs nonempty specific evidence",
        )?;
        for r in evidence {
            if unavailable == Some(r) {
                json::keys(r, &["path", "sha256"], &[])?;
                relative(r.get("path").str()?)?;
                json::digest(r.get("sha256"))?;
            } else {
                reference(plan, r)?;
            }
        }
    }
    let reviewed = bundle.get("reviewed_obligations").strings()?;
    require(
        reviewed.len() == ids.len() && reviewed.iter().copied().collect::<BTreeSet<_>>() == ids,
        "partial requirement map",
    )
}
fn review_contract(
    plan: &Plan<'_>,
    bundle: &Value,
    task: &Value,
    plan_id: &str,
    kind: Kind,
    unavailable: Option<&Value>,
) -> Result<()> {
    let mut fields = vec![
        "schema",
        "kind",
        "plan_id",
        "task_id",
        "task_sha256",
        "plan_sha256",
        "state",
        "lease_id",
        "owner",
        "original_contract",
        "review_summary",
        "reviewed_obligations",
        "requirements",
        "base_commit",
        "source_envelope",
        "source_manifest",
        "verifier",
    ];
    let optional = if kind == Kind::MainResearchAccepted {
        fields.extend(["main_research", "reviewer"]);
        vec!["legacy_reviewer_binding", "legacy_report_binding"]
    } else {
        fields.push(if kind == Kind::ResearchAccepted {
            "research"
        } else {
            "successor"
        });
        vec!["legacy_adoption"]
    };
    json::keys(bundle, &fields, &optional)?;
    require(
        bundle.get("schema").is_int(1) && bundle.text("kind") == kind.name(),
        "unsupported completion bundle version/kind",
    )?;
    require(
        lineage(bundle.text("plan_id"), "plan_")
            && lineage(bundle.text("lease_id"), "lease_")
            && bundle.text("plan_id") == plan_id
            && bundle.get("task_id") == task.get("id"),
        "exact immutable plan/task/lease identity required",
    )?;
    json::digest(bundle.get("plan_sha256"))?;
    require(
        crate::repository::commit(bundle.text("base_commit")),
        "full base commit identity required",
    )?;
    require(
        bundle.get("state") == task.get("state")
            && bundle.get("lease_id") == task.get("lease_id")
            && matches!(
                task.text("state"),
                "claimed" | "running" | "review" | "blocked"
            ),
        "original exact contract/state/lease mismatch",
    )?;
    require(
        json::receipt_digest(task)? == json::digest(bundle.get("task_sha256"))?
            && json::projection(task)? == *bundle.get("original_contract"),
        "original full task contract mismatch",
    )?;
    let owner = bundle.get("owner");
    json::keys(owner, &["provider", "participant", "parent_provider"], &[])?;
    for (name, max) in [
        ("provider", 80),
        ("parent_provider", 80),
        ("participant", 160),
    ] {
        text(owner.get(name).str()?, name, max)?;
    }
    let legacy =
        kind == Kind::MainResearchAccepted && task.text("claimed_by_participant").is_empty();
    require(
        !task.text("provider").is_empty()
            && owner.text("provider") == task.text("provider")
            && (legacy || owner.text("participant") == task.text("claimed_by_participant")),
        "current declared claim owner mismatch; owner inference is forbidden",
    )?;
    if legacy {
        let binding = bundle.get("legacy_reviewer_binding");
        json::keys(
            binding,
            &["participant", "original_contract", "review_summary"],
            &[],
        )?;
        require(
            binding.get("participant") == owner.get("participant")
                && binding.get("original_contract") == bundle.get("original_contract"),
            "legacy reviewer binding differs from original unbound contract",
        )?;
        text(
            binding.get("review_summary").str()?,
            "legacy reviewer approval summary",
            1000,
        )?;
    } else {
        require(
            bundle.opt("legacy_reviewer_binding").is_none(),
            "bound native claim cannot use legacy reviewer adoption",
        )?;
    }
    text(
        bundle.get("review_summary").str()?,
        "parent review summary",
        1000,
    )?;
    screen(&json::encoded(bundle)?)?;
    require(
        task.opt("completion_kind").is_none()
            && task.opt("completion_receipt").is_none()
            && !task.get("landing").truth()
            && !task.get("landed_commit").truth(),
        "original already contains completion or code landing fields",
    )?;
    manifest_shape(bundle.get("source_manifest"), bundle.get("source_envelope"))?;
    requirements(plan, bundle, unavailable)?;
    verifier(plan, bundle.get("verifier"), task, false)?;
    let manifest = bundle.get("source_manifest").array()?;
    require(
        bundle
            .get("verifier")
            .get("files")
            .array()?
            .iter()
            .all(|v| manifest.contains(v)),
        "frozen verifier is outside reviewed source manifest",
    )?;
    let mut covered = BTreeSet::new();
    for item in bundle.get("requirements").array()? {
        covered.extend(item.get("source_paths").strings()?);
    }
    for item in bundle.get("verifier").get("files").array()? {
        covered.insert(item.get("path").str()?);
    }
    require(
        covered
            == bundle
                .get("source_envelope")
                .strings()?
                .into_iter()
                .collect(),
        "reviewed obligation/verifier map omits declared source envelope",
    )
}
fn source(plan: &Plan<'_>, bundle: &Value, task: &Value) -> Result<Value> {
    let git = plan.git();
    git.guard()?;
    let base = bundle.get("base_commit").str()?;
    require(
        git.text(&["rev-parse", &format!("{base}^{{commit}}")])? == base,
        "base must be local commit",
    )?;
    git.ancestor(base, "HEAD")?;
    let mut identities = empty();
    let mut seen = BTreeSet::new();
    for item in bundle.get("source_manifest").array()? {
        let rel = item.get("path").str()?;
        seen.insert(rel);
        let snap = plan.source.snapshot(rel, 8 * 1024 * 1024, true)?;
        if item.text("state") == "absent" {
            require(
                snap.is_none(),
                format!("source deletion/absence changed: {rel}"),
            )?;
            identities.set(rel, Value::Null)?;
        } else {
            let snap = snap.ok_or_else(|| error(format!("source missing: {rel}")))?;
            require(
                json::sha(&snap.raw) == item.text("sha256")
                    && crate::repository::source_mode(snap.mode) == item.text("mode"),
                format!("source content/mode changed: {rel}"),
            )?;
            let mut identity = snap.generation;
            identity.set("mode", number(snap.mode))?;
            identities.set(rel, identity)?;
        }
    }
    let mut paths = BTreeSet::new();
    for raw in [
        git.run(&["ls-tree", "-r", "--name-only", "-z", base])?,
        git.run(&[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])?,
    ] {
        for path in raw.split(|b| *b == 0).filter(|s| !s.is_empty()) {
            paths.insert(
                std::str::from_utf8(path)
                    .map_err(|_| error("source path is not UTF-8"))?
                    .to_owned(),
            );
        }
    }
    let allowed = task.get("allowed_paths").strings()?;
    for path in paths {
        for pattern in &allowed {
            let pattern = crate::engine::clean_path(pattern)?;
            if oms_contract_markdown::scope::matches(&path, &pattern)
                .map_err(|e| error(e.to_string()))?
            {
                require(
                    seen.contains(path.as_str()),
                    "source manifest omits task allowed_paths content/deletions",
                )?;
            }
        }
    }
    let snapshot = git.snapshot()?;
    require(
        snapshot.get("clean") == &Value::Bool(true),
        "completion requires a fresh clean committed source snapshot",
    )?;
    Ok(object([
        ("head", snapshot.get("head").clone()),
        ("base_commit", string(base)),
        (
            "manifest_sha256",
            string(json::encoded_hash(bundle.get("source_manifest"))?),
        ),
        ("source_identities", identities),
    ]))
}
fn evidence_identities(plan: &Plan<'_>, bundle: &Value, extra: &[Value]) -> Result<Value> {
    let mut refs = Vec::new();
    for item in bundle.get("requirements").array()? {
        refs.extend(item.get("evidence").array()?.iter());
    }
    refs.extend(extra.iter());
    let mut identities = empty();
    for r in refs {
        let path = r.get("path").str()?;
        let snap = plan.store.snapshot(
            path,
            if path.ends_with(".patch") {
                MAX_PATCH
            } else {
                MAX_JSON
            },
            true,
        )?;
        let identity = if let Some(s) = snap {
            let mut v = s.generation;
            v.set("mode", number(s.mode))?;
            v
        } else {
            Value::Null
        };
        identities.set(path, identity)?;
    }
    Ok(identities)
}
fn indexed(plan: &Plan<'_>, event: &str, frozen: Option<&Value>) -> Result<Value> {
    let rows = plan.store.rows(".oms/artifacts/index.jsonl", false)?;
    let matches = rows
        .into_iter()
        .filter(|r| r.text("event_id") == event)
        .collect::<Vec<_>>();
    if matches.is_empty() {
        if let Some(row) = frozen {
            require(
                row.text("event_id") == event,
                "receipt lacks exact frozen indexed artifact identity",
            )?;
            return Ok(row.clone());
        }
    }
    require(
        matches.len() == 1,
        "indexed artifact identity unavailable or ambiguous",
    )?;
    Ok(matches[0].clone())
}
fn ancillary(plan: &Plan<'_>, row: &Value) -> Result<Value> {
    if !row.text("patch").is_empty() {
        let r = object([
            ("path", string(relative(row.get("patch").str()?)?)),
            ("sha256", string(json::digest(row.get("patch_sha256"))?)),
        ]);
        require(
            r.text("path").ends_with(".patch") && reference(plan, &r)?.is_empty(),
            "read producer ancillary patch must be exactly empty",
        )?;
        Ok(r)
    } else {
        require(
            !row.get("patch_sha256").truth(),
            "ancillary patch digest without path",
        )?;
        Ok(Value::Null)
    }
}
fn research(
    plan: &Plan<'_>,
    bundle: &Value,
    task: &Value,
    history: &History,
    historical: bool,
    frozen: Option<&Value>,
    unavailable: Option<&Value>,
) -> Result<Value> {
    let info = bundle.get("research");
    json::keys(
        info,
        &[
            "classification",
            "artifact",
            "index_event_id",
            "attempt_id",
            "parent_attempt_id",
        ],
        &[],
    )?;
    require(
        info.text("classification") == "research-only"
            && !task.get("patch").truth()
            && !task.get("landing").truth()
            && !task.get("landed_commit").truth(),
        "research requires explicitly reviewed research-only work without code landing",
    )?;
    let artifact = info.get("artifact");
    if unavailable != Some(artifact) {
        reference(plan, artifact)?;
    }
    require(
        task.text("artifact") == artifact.text("path"),
        "research artifact differs from original task evidence",
    )?;
    let row = indexed(plan, info.get("index_event_id").str()?, frozen)?;
    require(
        row.get("artifact") == artifact.get("path")
            && row.get("artifact_sha256") == artifact.get("sha256")
            && row.get("exit").is_int(0)
            && row.get("attempt_id") == info.get("attempt_id")
            && row.get("task_id") == task.get("id"),
        "artifact is not immutable successful indexed read result",
    )?;
    let ancillary = ancillary(plan, &row)?;
    let attempt = history.attempts.get(info.text("attempt_id"));
    let parent = history.attempts.get(info.text("parent_attempt_id"));
    let own = history
        .events
        .iter()
        .filter(|e| e.text("attempt_id") == info.text("attempt_id"))
        .cloned()
        .collect::<Vec<_>>();
    require(
        attempt.text("state") == "done"
            && attempt.get("terminal") == &Value::Bool(true)
            && attempt.get("task_id") == task.get("id")
            && attempt.get("parent_attempt_id") == info.get("parent_attempt_id")
            && attempt.get("provider") == row.get("provider")
            && own
                .first()
                .is_some_and(|e| e.get("refs").text("panel_access") == "read")
            && attempt.get("refs").text("panel_access") == "read"
            && own.iter().all(|e| {
                e.get("refs")
                    .opt("panel_access")
                    .is_none_or(|v| v.str() == Ok("read"))
            }),
        "exact terminal successful read-access attempt provenance is unproven",
    )?;
    let binding = provenance::read_parent(
        plan,
        history,
        parent,
        &own,
        task.text("claimed_by_participant"),
        bundle.get("owner").text("parent_provider"),
        historical,
    )?;
    let manifest = json::encoded_hash(bundle.get("source_manifest"))?;
    let pairs = [
        (row.get("plan_id"), bundle.get("plan_id")),
        (attempt.get("refs").get("plan_id"), bundle.get("plan_id")),
        (
            attempt.get("refs").get("task_sha256"),
            bundle.get("task_sha256"),
        ),
        (
            attempt.get("refs").get("source_manifest_sha256"),
            &string(&manifest),
        ),
    ];
    let bound =
        row.get("base_sha") == bundle.get("base_commit") && pairs.iter().all(|(a, b)| *a == *b);
    if !bound {
        let adoption = bundle.get("legacy_adoption");
        json::keys(
            adoption,
            &["artifact", "original_contract", "review_summary"],
            &[],
        )?;
        require(
            adoption.get("artifact") == artifact
                && adoption.get("original_contract") == bundle.get("original_contract"),
            "legacy adoption must bind exact original artifact and task obligation",
        )?;
        text(
            adoption.get("review_summary").str()?,
            "current legacy adoption review",
            1000,
        )?;
        if row.get("base_sha").truth() {
            let base = row.text("base_sha");
            require(
                (7..=64).contains(&base.len())
                    && base
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                    && plan
                        .git()
                        .text(&["rev-parse", &format!("{base}^{{commit}}")])?
                        == bundle.text("base_commit"),
                "conflicting legacy producer source base",
            )?;
        }
        require(
            pairs.iter().all(|(a, b)| !a.truth() || *a == *b),
            "conflicting producer binding cannot be adopted",
        )?;
    }
    require(
        bundle.get("requirements").array()?.iter().any(|r| {
            r.get("evidence")
                .array()
                .is_ok_and(|e| e.contains(artifact))
        }),
        "research artifact absent from requirement map",
    )?;
    Ok(object([
        ("index", row),
        ("attempt", attempt.clone()),
        (
            "parent",
            object([
                ("attempt_id", parent.get("attempt_id").clone()),
                ("provider", parent.get("provider").clone()),
                ("tool", parent.get("tool").clone()),
                ("participant", string(provenance::participant(&binding)?)),
            ]),
        ),
        (
            "attempt_events_sha256",
            string(json::encoded_hash(&Value::Array(own))?),
        ),
        ("ancillary_patch", ancillary),
    ]))
}
fn main_research(plan: &Plan<'_>, bundle: &Value, task: &Value, reviewer: &Value) -> Result<Value> {
    let info = bundle.get("main_research");
    json::keys(
        info,
        &[
            "classification",
            "provenance",
            "artifact",
            "index_event_id",
            "original_deliverable",
            "read_only_contract",
            "original_producer_status",
        ],
        &["failed_producer_review"],
    )?;
    let title = task.text("title");
    let product_words = regex::Regex::new(
        r"(?i)\b(implement|fix|repair|build|deploy|install|publish|push|merge)\b",
    )
    .map_err(|_| error("classification unavailable"))?;
    require(
        info.text("classification") == "research-only"
            && info.text("provenance") == "main-reviewed-external"
            && !task.get("patch").truth()
            && !task.get("landing").truth()
            && !task.get("landed_commit").truth()
            && [
                "researcher",
                "scholar",
                "analysis-worker",
                "advisor",
                "reviewer",
            ]
            .contains(&task.text("role"))
            && info.get("original_deliverable") == task.get("title")
            && info.text("read_only_contract") == "original-report-only"
            && !product_words.is_match(title),
        "main research cannot replace original product or landing obligation; main authorship provenance is unsupported",
    )?;
    let artifact = info.get("artifact");
    reference(plan, artifact)?;
    require(
        task.text("artifact") == artifact.text("path"),
        "main reviewed report differs from original task artifact",
    )?;
    let row = indexed(plan, info.get("index_event_id").str()?, None)?;
    require(
        row.get("artifact") == artifact.get("path")
            && row.get("artifact_sha256") == artifact.get("sha256"),
        "main reviewed report index bytes changed",
    )?;
    require(
        (!row.get("task_id").truth() || row.get("task_id") == task.get("id"))
            && (!row.get("plan_id").truth() || row.get("plan_id") == bundle.get("plan_id"))
            && row.get("exit").int().is_ok(),
        "foreign task/plan or malformed producer status cannot be adopted",
    )?;
    let status = object([
        ("kind", row.get("kind").clone()),
        ("exit", row.get("exit").clone()),
        ("attempt_id", row.get("attempt_id").clone()),
        ("task_id", row.get("task_id").clone()),
        ("plan_id", row.get("plan_id").clone()),
        ("base_sha", row.get("base_sha").clone()),
    ]);
    require(
        info.get("original_producer_status") == &status,
        "original producer facts must be preserved exactly",
    )?;
    if row.get("exit").int()? != 0 {
        text(
            info.get("failed_producer_review").str()?,
            "explicit failed producer review",
            1000,
        )?;
    } else {
        require(
            info.opt("failed_producer_review").is_none(),
            "failed producer review contradicts producer status",
        )?;
    }
    if !row.get("task_id").truth() || !row.get("plan_id").truth() {
        let b = bundle.get("legacy_report_binding");
        json::keys(
            b,
            &[
                "artifact",
                "index_event_id",
                "index_sha256",
                "original_contract",
                "review_summary",
            ],
            &[],
        )?;
        require(
            b.get("artifact") == artifact
                && b.get("index_event_id") == info.get("index_event_id")
                && b.text("index_sha256") == json::encoded_hash(&row)?
                && b.get("original_contract") == bundle.get("original_contract"),
            "legacy report binding must preserve exact unbound index and original task",
        )?;
        text(
            b.get("review_summary").str()?,
            "current unbound report review",
            1000,
        )?;
    } else {
        require(
            bundle.opt("legacy_report_binding").is_none(),
            "bound producer report cannot use legacy adoption",
        )?;
    }
    ancillary(plan, &row)?;
    require(
        bundle.get("requirements").array()?.iter().any(|r| {
            r.get("obligation") == task.get("title")
                && r.get("evidence")
                    .array()
                    .is_ok_and(|e| e.contains(artifact))
        }),
        "original READ deliverable and report must be covered explicitly",
    )?;
    reviewer.object()?;
    Ok(object([
        ("index", row),
        ("artifact", artifact.clone()),
        ("provenance", info.get("provenance").clone()),
        ("child_provenance_asserted", Value::Bool(false)),
        ("reviewer", reviewer.clone()),
    ]))
}
pub(crate) fn check_bundle(
    plan: &Plan<'_>,
    s: &Snapshot,
    bundle: &Value,
    task: &Value,
    kind: Kind,
    proof: &Value,
    reviewer: &Value,
) -> Result<Value> {
    review_contract(plan, bundle, task, s.plan.text("plan_id"), kind, None)?;
    require(
        plan.git().namespace()? == plan.state_git().namespace()?,
        "foreign execution/state repository",
    )?;
    let mut snapshot = source(plan, bundle, task)?;
    snapshot.set(
        "verifier_command_sha256",
        // Source already validated every frozen verifier entry in this same
        // snapshot; re-use that byte/mode proof instead of reopening it.
        string(verifier(plan, bundle.get("verifier"), task, false)?),
    )?;
    snapshot.set("repository_sha256", string(plan.state_git().namespace()?))?;
    let frozen = provenance::load(plan, proof, true)?;
    snapshot.set("provenance", proof.clone())?;
    let evidence = match kind {
        Kind::ResearchAccepted => {
            let current_history = provenance::live_history(plan, proof.text("room_id"))?;
            let current = research(plan, bundle, task, &current_history, false, None, None)?;
            let frozen_evidence = research(plan, bundle, task, &frozen, true, None, None)?;
            for name in [
                "attempt_id",
                "provider",
                "tool",
                "task_id",
                "parent_attempt_id",
                "base_sha",
                "state",
                "terminal",
                "refs",
            ] {
                require(
                    current.get("attempt").get(name) == frozen_evidence.get("attempt").get(name),
                    "relevant READ provenance changed during verification",
                )?;
            }
            for name in ["index", "parent", "ancillary_patch"] {
                require(
                    current.get(name) == frozen_evidence.get(name),
                    "READ producer bindings changed during verification",
                )?;
            }
            for event in current_history
                .events
                .iter()
                .skip(proof.get("lifecycle").get("rows").int()? as usize)
            {
                if event.text("attempt_id") == bundle.get("research").text("attempt_id") {
                    if let Some(refs) = event.opt("refs") {
                        for (k, v) in refs.object()? {
                            let name =
                                std::str::from_utf8(k).map_err(|_| error("invalid READ ref"))?;
                            require(
                                frozen_evidence.get("attempt").get("refs").get(name) == v,
                                "contradictory READ ref history",
                            )?;
                        }
                    }
                }
            }
            frozen_evidence
        }
        Kind::MainResearchAccepted => main_research(plan, bundle, task, reviewer)?,
        Kind::SatisfiedBy => {
            crate::product::satisfaction(plan, bundle, s.plan.get("tasks"), s.plan.text("plan_id"))?
        }
    };
    let mut extra = Vec::new();
    match kind {
        Kind::ResearchAccepted => {
            extra.push(bundle.get("research").get("artifact").clone());
            if evidence.get("ancillary_patch").truth() {
                extra.push(evidence.get("ancillary_patch").clone());
            }
        }
        Kind::MainResearchAccepted => {
            extra.push(bundle.get("main_research").get("artifact").clone())
        }
        Kind::SatisfiedBy => {
            if bundle.get("successor").text("product_kind") == "patch-land" {
                extra.push(object([
                    (
                        "path",
                        string(crate::product::legacy_relative(
                            plan,
                            evidence.get("intent").text("patch"),
                        )?),
                    ),
                    ("sha256", evidence.get("intent").get("patch_sha").clone()),
                ]));
                extra.push(object([
                    ("path", evidence.get("admission").get("artifact").clone()),
                    (
                        "sha256",
                        evidence.get("admission").get("artifact_sha256").clone(),
                    ),
                ]));
            }
        }
    }
    snapshot.set("evidence", evidence)?;
    snapshot.set(
        "evidence_identities",
        evidence_identities(plan, bundle, &extra)?,
    )?;
    if kind == Kind::MainResearchAccepted
        && bundle.get("reviewer").text("kind") == "codex-desktop-main"
    {
        let manifest = plan
            .services
            .controller_manifest(&plan.context.execution_repo)?;
        crate::approval::validate_controller(plan, &manifest)?;
        snapshot.set("controller_manifest", manifest)?;
        if !reviewer.object()?.is_empty() {
            let marker = crate::approval::record(
                plan,
                reviewer.get("approval_ref"),
                bundle,
                reviewer.get("bundle_sha256").str()?,
                true,
            )?;
            require(
                crate::approval::neutral(&snapshot)? == *marker.get("snapshot"),
                "approved full snapshot differs from current report/source/verifier/CAS evidence",
            )?;
        }
    }
    Ok(snapshot)
}
pub(crate) fn validate_verification(v: &Value, snapshot: &Value) -> Result<()> {
    json::keys(
        v,
        &[
            "exit",
            "command_sha256",
            "output_sha256",
            "output_bytes",
            "proof_sha256",
        ],
        &[],
    )?;
    json::digest(v.get("output_sha256"))?;
    require(
        v.get("exit").is_int(0)
            && v.get("output_bytes")
                .int()
                .is_ok_and(|n| (0..=1024 * 1024).contains(&n))
            && v.get("command_sha256") == snapshot.get("verifier_command_sha256"),
        "actual bounded verifier did not pass",
    )?;
    let mut proof = v.clone();
    proof.remove("proof_sha256");
    proof.set("snapshot_sha256", string(json::encoded_hash(snapshot)?))?;
    require(
        json::digest(v.get("proof_sha256"))? == json::encoded_hash(&proof)?,
        "verifier proof digest mismatch",
    )
}
pub(crate) fn capture_verifier(plan: &Plan<'_>, command: &str, snapshot: &Value) -> Result<Value> {
    let wall = timeout(
        &std::env::var("OMS_PEER_VERIFY_TIMEOUT").unwrap_or_else(|_| "10m".into()),
        true,
    )?;
    let result = plan
        .services
        .supervise(&crate::services::SupervisedCommand {
            argv: vec!["bash".into(), "-c".into(), command.into()],
            cwd: plan.context.execution_repo.clone(),
            wall,
            output_limit: 1024 * 1024,
            label: "completion-verifier",
            remove_environment: vec![],
            environment: vec![("GIT_OPTIONAL_LOCKS".into(), "0".into())],
        })?;
    require(
        result.exit == 0
            && result.runner_exit == 0
            && !result.timed_out
            && !result.output_limited
            && !result.launch_error
            && !result.supervision_error
            && result.output.len() <= 1024 * 1024,
        "actual verifier execution failed, timed out or exceeded bounded capture",
    )?;
    let mut v = object([
        ("exit", number(result.exit)),
        ("command_sha256", string(json::sha(command))),
        ("output_sha256", string(json::sha(&result.output))),
        ("output_bytes", number(result.output.len())),
    ]);
    let mut proof = v.clone();
    proof.set("snapshot_sha256", string(json::encoded_hash(snapshot)?))?;
    v.set("proof_sha256", string(json::encoded_hash(&proof)?))?;
    Ok(v)
}
pub(crate) fn timeout(value: &str, fractional: bool) -> Result<Duration> {
    let re = regex::Regex::new(if fractional {
        r"^([0-9]+(?:\.[0-9]+)?)([smhd]?)$"
    } else {
        r"^([1-9][0-9]*)([smh]?)$"
    })
    .map_err(|_| error("duration parser unavailable"))?;
    let captures = re
        .captures(value)
        .ok_or_else(|| error("timeout must be a positive finite duration up to 24h"))?;
    let amount = captures[1]
        .parse::<f64>()
        .map_err(|_| error("invalid timeout"))?;
    let seconds = amount
        * match &captures[2] {
            "s" | "" => 1.0,
            "m" => 60.0,
            "h" => 3600.0,
            "d" => 86400.0,
            _ => return Err(error("invalid timeout unit")),
        };
    require(
        seconds.is_finite() && seconds > 0.0 && seconds <= 86400.0,
        "timeout must be a positive finite duration up to 24h",
    )?;
    Duration::try_from_secs_f64(seconds).map_err(|_| error("invalid timeout"))
}
fn completed_task(receipt: &Value, reference: &Value) -> Result<Value> {
    let mut task = receipt.get("original_task").clone();
    let original = task.get("state").clone();
    history(
        &mut task,
        object([
            ("schema", number(1)),
            ("kind", receipt.get("completion_kind").clone()),
            ("ts", receipt.get("ts").clone()),
            ("original_state", original),
            ("completion_receipt", reference.clone()),
        ]),
    )?;
    task.set("state", string("done"))?;
    task.set("updated", receipt.get("ts").clone())?;
    task.set("completion_kind", receipt.get("completion_kind").clone())?;
    task.set("completion_receipt", reference.clone())?;
    Ok(task)
}
fn checked_receipt(plan: &Plan<'_>, reference: &Value) -> Result<Value> {
    require(
        reference.text("path")
            == format!(
                ".oms/plan/completions/{}.json",
                json::digest(reference.get("sha256"))?
            ),
        "completion receipt must be content-addressed in canonical namespace",
    )?;
    let receipt = json::parse(&provenance::reference(plan, reference, MAX_JSON)?)?;
    json::keys(
        &receipt,
        &[
            "schema",
            "kind",
            "completion_kind",
            "bundle_sha256",
            "bundle",
            "original_task",
            "original_plan_sha256",
            "snapshot",
            "verification",
            "ts",
            "bundle_raw",
        ],
        &["executor"],
    )?;
    require(
        receipt.get("schema").is_int(1)
            && receipt.text("kind") == "oms-plan-completion"
            && parse_ts(receipt.text("ts")).is_some(),
        "unsupported or malformed completion receipt",
    )?;
    receipt.get("original_task").object()?;
    let snapshot = receipt.get("snapshot");
    json::keys(
        snapshot,
        &[
            "head",
            "base_commit",
            "manifest_sha256",
            "source_identities",
            "verifier_command_sha256",
            "repository_sha256",
            "evidence",
            "evidence_identities",
            "provenance",
        ],
        &["controller_manifest"],
    )?;
    provenance::load(plan, snapshot.get("provenance"), false)?;
    let kind = Kind::parse(receipt.text("completion_kind"))?;
    let bundle = receipt.get("bundle");
    require(
        bundle.text("kind") == kind.name(),
        "completion receipt kind mismatch",
    )?;
    validate_verification(receipt.get("verification"), snapshot)?;
    require(
        receipt.get("verification").text("command_sha256")
            == json::sha(bundle.get("verifier").text("command"))
            && receipt.get("original_plan_sha256") == bundle.get("plan_sha256")
            && snapshot.get("base_commit") == bundle.get("base_commit"),
        "completion receipt lacks exact successful verification",
    )?;
    let raw = receipt.get("bundle_raw").str()?;
    require(
        json::sha(raw) == json::digest(receipt.get("bundle_sha256"))?
            && json::parse(raw.as_bytes())? == *bundle,
        "receipt reviewed bundle bytes mismatch",
    )?;
    Ok(receipt)
}
pub fn validate_done(
    plan: &Plan<'_>,
    state: &Value,
    task: &Value,
    allow_unverified: bool,
) -> Result<CompletionState> {
    let Some(kind) = task.opt("completion_kind") else {
        require(
            task.opt("completion_receipt").is_none(),
            "unknown completion fields",
        )?;
        return Ok(CompletionState::NotTyped);
    };
    let kind = Kind::parse(kind.str()?)?;
    require(
        task.text("state") == "done",
        "completion reference on nonterminal task",
    )?;
    let reference = task.get("completion_receipt");
    let receipt = checked_receipt(plan, reference)?;
    let bundle = receipt.get("bundle");
    let snapshot = receipt.get("snapshot");
    require(
        bundle.get("plan_id") == state.get("plan_id")
            && bundle.get("task_id") == task.get("id")
            && snapshot.text("repository_sha256") == plan.state_git().namespace()?
            && completed_task(&receipt, reference)? == *task,
        "forged, foreign or unreferenced task completion receipt",
    )?;
    let original = receipt.get("original_task");
    let recorded = snapshot.get("evidence");
    let mut frozen = None;
    let mut unavailable = None;
    if kind == Kind::ResearchAccepted {
        let info = bundle.get("research");
        let rows = plan.store.rows(".oms/artifacts/index.jsonl", false)?;
        if !rows
            .iter()
            .any(|r| r.get("event_id") == info.get("index_event_id"))
        {
            let saved = recorded.get("index");
            require(
                saved.get("event_id") == info.get("index_event_id"),
                "receipt lacks frozen indexed artifact identity",
            )?;
            frozen = Some(saved);
            let artifact = info.get("artifact");
            if reference_available(plan, artifact).is_err() {
                unavailable = Some(artifact);
            }
        }
    }
    review_contract(
        plan,
        bundle,
        original,
        state.text("plan_id"),
        kind,
        unavailable,
    )?;
    require(
        snapshot.text("manifest_sha256") == json::encoded_hash(bundle.get("source_manifest"))?,
        "receipt source manifest digest mismatch",
    )?;
    let actual = match kind {
        Kind::ResearchAccepted => {
            let history = provenance::load(plan, snapshot.get("provenance"), false)?;
            research(plan, bundle, original, &history, true, frozen, unavailable)?
        }
        Kind::MainResearchAccepted => {
            crate::approval::validate_reviewer(
                plan,
                bundle,
                recorded.get("reviewer"),
                receipt.text("bundle_sha256"),
                snapshot.get("provenance"),
                false,
            )?;
            let actual = main_research(plan, bundle, original, recorded.get("reviewer"))?;
            if bundle.get("reviewer").text("kind") == "codex-desktop-main" {
                let marker = crate::approval::record(
                    plan,
                    recorded.get("reviewer").get("approval_ref"),
                    bundle,
                    receipt.text("bundle_sha256"),
                    false,
                )?;
                require(
                    crate::approval::neutral(snapshot)? == *marker.get("snapshot"),
                    "accepted snapshot differs from actual main approval",
                )?;
                json::keys(receipt.get("executor"), &["kind", "uid"], &[])?;
                require(
                    receipt.get("executor").text("kind") == "local-uid"
                        && receipt
                            .get("executor")
                            .get("uid")
                            .int()
                            .is_ok_and(|n| n >= 0),
                    "invalid separate local executor record",
                )?;
            }
            actual
        }
        Kind::SatisfiedBy => {
            let successor = bundle.get("successor");
            let mut historical = successor.get("original_contract").clone();
            historical.set("state", string("done"))?;
            let mut tasks = empty();
            tasks.set(successor.get("task_id").str()?, historical)?;
            crate::product::satisfaction(plan, bundle, &tasks, state.text("plan_id"))?
        }
    };
    require(
        actual == *recorded,
        "immutable completion provenance changed",
    )?;
    if unavailable.is_some() {
        if allow_unverified {
            return Ok(CompletionState::Unavailable);
        }
        return Err(error(
            "evicted research artifact unavailable or digest changed",
        ));
    }
    Ok(CompletionState::Verified)
}
fn reference_available(plan: &Plan<'_>, r: &Value) -> Result<()> {
    reference(plan, r)?;
    Ok(())
}
fn prior(plan: &Plan<'_>, bundle_sha: &str) -> Result<Option<(Value, Value)>> {
    let names = plan.store.names(".oms/plan/completions", 4096, true)?;
    let mut found = None;
    for name in names {
        let name = name
            .to_str()
            .ok_or_else(|| error("malformed completion receipt namespace"))?;
        let hash = name
            .strip_suffix(".json")
            .ok_or_else(|| error("malformed completion receipt namespace"))?;
        require(
            json::hex(hash, 64),
            "malformed completion receipt namespace",
        )?;
        let reference = object([
            ("path", string(format!(".oms/plan/completions/{name}"))),
            ("sha256", string(hash)),
        ]);
        let receipt = checked_receipt(plan, &reference)?;
        if receipt.text("bundle_sha256") == bundle_sha {
            require(
                found.is_none(),
                "conflicting completion receipts for bundle",
            )?;
            found = Some((receipt, reference));
        }
    }
    Ok(found)
}
pub struct PreparedCompletion {
    bundle: Value,
    bundle_raw: Vec<u8>,
    bundle_sha256: String,
    bundle_path: String,
    original: Value,
    snapshot: Value,
    kind: Kind,
    prior: Option<(Value, Value)>,
    pin_id: String,
    approval_only: bool,
    completed: bool,
}
impl PreparedCompletion {
    pub fn snapshot(&self) -> &Value {
        &self.snapshot
    }
    pub fn bundle_sha256(&self) -> &str {
        &self.bundle_sha256
    }
    pub fn resumes(&self) -> bool {
        self.prior.is_some()
    }
}
fn cas(req: &Request, bundle: &Value) -> Result<()> {
    for (cli, field) in [
        ("expected-plan-sha256", "plan_sha256"),
        ("expected-task-sha256", "task_sha256"),
        ("expected-state", "state"),
        ("lease-id", "lease_id"),
    ] {
        require(
            req.value(cli)? == bundle.get(field).str()?,
            "expected CLI CAS differs from reviewed bundle",
        )?;
    }
    Ok(())
}
fn completion_room(plan: &Plan<'_>, req: &Request, bundle: &Value) -> Result<String> {
    let room = if matches!(
        req.action,
        Action::AcceptMainResearch | Action::ApproveMainResearch
    ) && (req.has("main-review-approval")
        || req.action == Action::ApproveMainResearch
        || plan.context.room_id.is_empty())
    {
        require(
            bundle.get("reviewer").text("kind") == "codex-desktop-main",
            "explicit Desktop review room required",
        )?;
        bundle.get("reviewer").get("room_id").str()?.to_owned()
    } else {
        plan.context.room_id.clone()
    };
    require(
        crate::engine::room_id(&room),
        "invalid completion room identity",
    )?;
    Ok(room)
}
pub fn execute(plan: &Plan<'_>, req: &Request) -> Result<Response> {
    plan.context.parent()?;
    require(
        plan.context.canonical(),
        "completion requires canonical active plan",
    )?;
    let path = relative(req.value("completion-bundle")?)?;
    let raw = plan.store.read(path, MAX_JSON)?;
    let bundle = json::parse(&raw)?;
    let room = completion_room(plan, req, &bundle)?;
    let prepared = plan.locked(true, Some(&room), || preflight(plan, req))?;
    let result = (|| {
        let verification = if prepared.resumes() {
            prepared
                .prior
                .as_ref()
                .unwrap()
                .0
                .get("verification")
                .clone()
        } else {
            verify(plan, req, &prepared)?
        };
        plan.locked(true, Some(&room), || {
            finalize(plan, req, &prepared, &verification)
        })
    })();
    if !prepared.pin_id.is_empty() {
        let cleanup = plan.locked(false, None, || {
            remove_pin(plan, &prepared.pin_id, result.is_ok())
        });
        if let Err(e) = cleanup {
            return Err(error(format!(
                "{}{}",
                if result.is_ok() {
                    "completion result already published; "
                } else {
                    ""
                },
                e.message
            )));
        }
    }
    result
}
pub(crate) fn preflight(plan: &Plan<'_>, req: &Request) -> Result<PreparedCompletion> {
    // The external adapter must use Plan::execute, or hold the same ordered
    // locks before calling these phase APIs. Prepared fields are private.
    plan.context.parent()?;
    require(
        plan.context.canonical(),
        "completion requires canonical active plan",
    )?;
    crate::retirement::cleanup(plan)?;
    let s = plan.load()?;
    s.require_contract()?;
    let task = s.task(req.value("id")?)?;
    let bundle_path = relative(req.value("completion-bundle")?)?.to_owned();
    let bundle_raw = plan.store.read(&bundle_path, MAX_JSON)?;
    let bundle_sha = req.value("expected-completion-bundle-sha256")?;
    require(
        json::hex(bundle_sha, 64) && json::sha(&bundle_raw) == bundle_sha,
        "bundle bytes changed",
    )?;
    let bundle = json::parse(&bundle_raw)?;
    cas(req, &bundle)?;
    let kind = Kind::from_action(req.action)?;
    let approval_only = req.action == Action::ApproveMainResearch;
    let prior = if approval_only {
        None
    } else {
        prior(plan, bundle_sha)?
    };
    let completed = task.text("state") == "done";
    if completed {
        require(
            !approval_only,
            "completed task cannot receive fresh approval",
        )?;
        validate_done(plan, &s.plan, task, false)?;
        let (receipt, reference) = prior.as_ref().ok_or_else(|| error("conflicting replay"))?;
        require(
            reference == task.get("completion_receipt")
                && receipt.get("bundle") == &bundle
                && receipt.text("completion_kind") == kind.name(),
            "conflicting completed replay",
        )?;
        if kind == Kind::MainResearchAccepted
            && bundle.get("reviewer").text("kind") == "codex-desktop-main"
        {
            require(
                req.text("main-review-approval")?
                    == receipt
                        .get("snapshot")
                        .get("evidence")
                        .get("reviewer")
                        .get("approval_ref")
                        .text("path")
                    && req.text("expected-main-review-approval-sha256")?
                        == receipt
                            .get("snapshot")
                            .get("evidence")
                            .get("reviewer")
                            .get("approval_ref")
                            .text("sha256"),
                "completed replay requires exact original main approval",
            )?;
        } else {
            crate::approval::completion_owner(
                plan,
                req,
                &bundle,
                receipt.get("original_task"),
                bundle_sha,
                Some(receipt.get("snapshot")),
                false,
            )?;
        }
        return Ok(PreparedCompletion {
            bundle_raw,
            bundle_sha256: bundle_sha.into(),
            bundle_path,
            bundle: bundle.clone(),
            original: receipt.get("original_task").clone(),
            snapshot: receipt.get("snapshot").clone(),
            kind,
            prior,
            pin_id: String::new(),
            approval_only,
            completed: true,
        });
    }
    require(
        s.raw
            .as_ref()
            .is_some_and(|raw| json::sha(raw) == bundle.text("plan_sha256")),
        "active plan bytes changed",
    )?;
    for marker in plan.worker_markers()? {
        if marker.get("task_id") == task.get("id") && marker.get("lease_id") == task.get("lease_id")
        {
            require(
                marker_typed(&marker) && !marker_alive(&marker),
                "live or unproven exact worker veto",
            )?;
        }
    }
    review_contract(plan, &bundle, task, s.plan.text("plan_id"), kind, None)?;
    let owner = crate::approval::completion_owner(
        plan,
        req,
        &bundle,
        task,
        bundle_sha,
        None,
        approval_only,
    )?;
    let room = owner.get("room_id").str()?;
    let proof = if let Some((receipt, _)) = &prior {
        receipt.get("snapshot").get("provenance").clone()
    } else if kind == Kind::MainResearchAccepted
        && bundle.get("reviewer").text("kind") == "codex-desktop-main"
        && !approval_only
    {
        crate::approval::runtime_record(plan, req, &bundle, bundle_sha)?
            .1
            .get("snapshot")
            .get("provenance")
            .clone()
    } else {
        provenance::capture(plan, room)?
    };
    require(
        proof.text("room_id") == room,
        "current caller room differs from captured authority",
    )?;
    let reviewer = owner.opt("reviewer").cloned().unwrap_or_else(empty);
    let snapshot = check_bundle(plan, &s, &bundle, task, kind, &proof, &reviewer)?;
    if kind == Kind::MainResearchAccepted && !approval_only {
        crate::approval::validate_reviewer(plan, &bundle, &reviewer, bundle_sha, &proof, true)?;
    }
    if let Some((receipt, _)) = &prior {
        require(
            receipt.get("bundle") == &bundle
                && receipt.get("original_task") == task
                && receipt.get("snapshot") == &snapshot
                && receipt.text("completion_kind") == kind.name(),
            "prior receipt preconditions changed",
        )?;
    }
    let pin_id = storage::random_hex(16)?;
    write_pin(
        plan,
        &pin_id,
        bundle.text("lease_id"),
        &snapshot,
        &bundle,
        &bundle_path,
    )?;
    Ok(PreparedCompletion {
        bundle_raw,
        bundle_sha256: bundle_sha.into(),
        bundle_path,
        bundle,
        original: task.clone(),
        snapshot,
        kind,
        prior,
        pin_id,
        approval_only,
        completed: false,
    })
}
pub(crate) fn verify(plan: &Plan<'_>, req: &Request, p: &PreparedCompletion) -> Result<Value> {
    if let Some((receipt, _)) = &p.prior {
        return Ok(receipt.get("verification").clone());
    }
    let before = refresh(plan, req, p)?;
    require(
        before == p.snapshot,
        "frozen inputs changed before actual verifier execution",
    )?;
    let result = capture_verifier(plan, p.bundle.get("verifier").text("command"), &before)?;
    let after = refresh(plan, req, p)?;
    require(
        before == after,
        "source/evidence/verifier changed during actual execution",
    )?;
    Ok(result)
}
fn refresh(plan: &Plan<'_>, req: &Request, p: &PreparedCompletion) -> Result<Value> {
    let s = plan.load()?;
    s.require_contract()?;
    require(
        plan.store.read(&p.bundle_path, MAX_JSON)? == p.bundle_raw
            && s.raw
                .as_ref()
                .is_some_and(|r| json::sha(r) == p.bundle.text("plan_sha256"))
            && s.task(p.original.text("id"))? == &p.original,
        "plan or bundle changed before completion finalization",
    )?;
    let owner = crate::approval::completion_owner(
        plan,
        req,
        &p.bundle,
        &p.original,
        &p.bundle_sha256,
        Some(&p.snapshot),
        p.approval_only,
    )?;
    require(
        owner.get("room_id") == p.snapshot.get("provenance").get("room_id"),
        "current completion room changed",
    )?;
    let reviewer = owner.opt("reviewer").cloned().unwrap_or_else(empty);
    let current = check_bundle(
        plan,
        &s,
        &p.bundle,
        &p.original,
        p.kind,
        p.snapshot.get("provenance"),
        &reviewer,
    )?;
    if p.kind == Kind::MainResearchAccepted && !p.approval_only {
        crate::approval::validate_reviewer(
            plan,
            &p.bundle,
            &reviewer,
            &p.bundle_sha256,
            current.get("provenance"),
            true,
        )?;
    }
    Ok(current)
}
pub(crate) fn finalize(
    plan: &Plan<'_>,
    req: &Request,
    p: &PreparedCompletion,
    verification: &Value,
) -> Result<Response> {
    plan.context.parent()?;
    crate::retirement::cleanup(plan)?;
    if p.completed {
        cas(req, &p.bundle)?;
        require(
            plan.store.read(&p.bundle_path, MAX_JSON)? == p.bundle_raw,
            "bundle bytes changed during completed replay",
        )?;
        let s = plan.load()?;
        let task = s.task(p.original.text("id"))?;
        let (receipt, reference) = p
            .prior
            .as_ref()
            .ok_or_else(|| error("missing completed receipt"))?;
        require(
            task.get("completion_receipt") == reference
                && checked_receipt(plan, reference)? == *receipt,
            "conflicting completed replay",
        )?;
        validate_done(plan, &s.plan, task, false)?;
        return Ok(Response::text(format!(
            "plan: completion already applied ({})",
            task.text("id")
        )));
    }
    let snapshot = refresh(plan, req, p)?;
    require(
        snapshot == p.snapshot,
        "evidence/source/verifier changed during execution",
    )?;
    validate_verification(verification, &snapshot)?;
    let mut s = plan.load()?;
    let original = s.task(p.original.text("id"))?.clone();
    require(original == p.original, "task changed during completion")?;
    for marker in plan.worker_markers()? {
        if marker.get("task_id") == original.get("id")
            && marker.get("lease_id") == original.get("lease_id")
        {
            require(
                marker_typed(&marker) && !marker_alive(&marker),
                "live or unproven exact worker veto",
            )?;
        }
    }
    if p.approval_only {
        return crate::approval::publish(
            plan,
            req,
            &p.bundle,
            &p.bundle_sha256,
            &snapshot,
            verification,
        );
    }
    let (receipt, reference) = if let Some(prior) = &p.prior {
        prior.clone()
    } else {
        let raw_text = std::str::from_utf8(&p.bundle_raw)
            .map_err(|_| error("completion bundle must be UTF-8"))?;
        let mut receipt = object([
            ("schema", number(1)),
            ("kind", string("oms-plan-completion")),
            ("completion_kind", string(p.kind.name())),
            ("bundle_sha256", string(&p.bundle_sha256)),
            ("bundle", p.bundle.clone()),
            ("bundle_raw", string(raw_text)),
            ("original_task", original.clone()),
            (
                "original_plan_sha256",
                string(json::sha(
                    s.raw.as_ref().ok_or_else(|| error("missing active plan"))?,
                )),
            ),
            ("snapshot", snapshot),
            ("verification", verification.clone()),
            ("ts", string(&plan.context.ts)),
        ]);
        if p.kind == Kind::MainResearchAccepted
            && p.bundle.get("reviewer").text("kind") == "codex-desktop-main"
        {
            receipt.set(
                "executor",
                object([
                    ("kind", string("local-uid")),
                    ("uid", number(rustix::process::getuid().as_raw())),
                ]),
            )?;
        }
        let (reference, _) = plan.store.immutable(
            ".oms/plan/completions",
            &json::encoded(&receipt)?,
            ".json",
            MAX_JSON,
        )?;
        (receipt, reference)
    };
    let task = completed_task(&receipt, &reference)?;
    s.put(original.text("id"), task)?;
    plan.save(&mut s)?;
    Ok(Response::text(format!(
        "plan: {} {}",
        original.text("id"),
        p.kind.name()
    )))
}
fn completion_refs(v: &Value, out: &mut Vec<Value>) -> Result<()> {
    match v {
        Value::Object(fields) => {
            if let Some(path) = v.opt("path") {
                if path.str().is_ok_and(|p| {
                    [
                        "completion-captures",
                        "completion-prefixes",
                        "completion-segments",
                        "completions",
                    ]
                    .iter()
                    .any(|d| p.starts_with(&format!(".oms/plan/{d}/")))
                }) {
                    out.push(object([
                        ("path", path.clone()),
                        ("sha256", v.get("sha256").clone()),
                    ]));
                }
            }
            for (k, v) in fields {
                completion_refs(&Value::String(k.clone()), out)?;
                completion_refs(v, out)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                completion_refs(item, out)?;
            }
        }
        Value::String(_) => {
            let p = v.str()?;
            for d in [
                "completion-captures",
                "completion-prefixes",
                "completion-segments",
                "completions",
            ] {
                if let Some(name) = p.strip_prefix(&format!(".oms/plan/{d}/")) {
                    if let Some((hash, suffix)) = name.rsplit_once('.') {
                        if json::hex(hash, 64) && ["json", "jsonl", "bin"].contains(&suffix) {
                            out.push(object([("path", string(p)), ("sha256", string(hash))]));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}
fn pin_ids(value: &Value) -> Result<Vec<String>> {
    json::keys(value, &["schema", "pins"], &[])?;
    let ids = value.get("pins").strings()?;
    require(
        value.get("schema").is_int(1)
            && ids.len() <= 8192
            && ids.iter().all(|s| json::hex(s, 32))
            && ids.iter().copied().collect::<BTreeSet<_>>().len() == ids.len(),
        "invalid completion pin lease index",
    )?;
    Ok(ids.iter().map(|s| s.to_string()).collect())
}
fn write_pin(
    plan: &Plan<'_>,
    id: &str,
    lease: &str,
    snapshot: &Value,
    bundle: &Value,
    path: &str,
) -> Result<()> {
    require(
        json::hex(id, 32) && lineage(lease, "lease_"),
        "invalid completion transaction identity or lease",
    )?;
    let mut objects = Vec::new();
    completion_refs(snapshot, &mut objects)?;
    completion_refs(bundle, &mut objects)?;
    completion_refs(&string(path), &mut objects)?;
    for r in &objects {
        relative(r.get("path").str()?)?;
        json::digest(r.get("sha256"))?;
    }
    let lease_path = format!(".oms/plan/completion-pin-leases/{lease}.json");
    let old = plan.store.optional(&lease_path, MAX_JSON)?;
    let mut ids = if let Some(raw) = &old {
        pin_ids(&json::parse(raw)?)?
    } else {
        vec![]
    };
    require(
        ids.len() < 8192 && !ids.iter().any(|s| s == id),
        "duplicate or oversized completion pin lease index",
    )?;
    ids.push(id.into());
    let raw = json::encoded(&object([
        ("schema", number(1)),
        ("pins", list(ids.iter().map(String::as_str))),
    ]))?;
    plan.store.mutate(
        &lease_path,
        old.as_deref().or(Some(&[])),
        &raw,
        MAX_JSON,
        true,
    )?;
    let pin_path = format!(".oms/plan/completion-pins/{id}.json");
    require(
        plan.store.optional(&pin_path, MAX_JSON)?.is_none(),
        "completion pin already exists",
    )?;
    let raw = json::encoded(&object([
        ("schema", number(1)),
        ("lease_id", string(lease)),
        ("objects", Value::Array(objects)),
    ]))?;
    plan.store.mutate(pin_path, Some(&[]), &raw, MAX_JSON, true)
}
fn remove_pin(plan: &Plan<'_>, id: &str, _published: bool) -> Result<()> {
    require(json::hex(id, 32), "invalid completion transaction identity")?;
    let path = format!(".oms/plan/completion-pins/{id}.json");
    if let Some(raw) = plan.store.optional(&path, MAX_JSON)? {
        let value = json::parse(&raw)?;
        json::keys(&value, &["schema", "lease_id", "objects"], &[])?;
        require(
            value.get("schema").is_int(1) && lineage(value.text("lease_id"), "lease_"),
            "invalid completion pin",
        )?;
        plan.store.remove(&path, &raw, None)?;
    }
    Ok(())
}
pub(crate) fn prepare_settled(
    plan: &Plan<'_>,
    leases: &BTreeSet<String>,
) -> Result<Vec<(String, Vec<u8>)>> {
    let mut selected = BTreeMap::new();
    for lease in leases {
        require(lineage(lease, "lease_"), "invalid completion pin lease")?;
        let path = format!(".oms/plan/completion-pin-leases/{lease}.json");
        if let Some(raw) = plan.store.optional(&path, MAX_JSON)? {
            for id in pin_ids(&json::parse(&raw)?)? {
                let pin_path = format!(".oms/plan/completion-pins/{id}.json");
                if let Some(pin) = plan.store.optional(&pin_path, MAX_JSON)? {
                    let v = json::parse(&pin)?;
                    require(v.text("lease_id") == lease, "completion pin lease mismatch")?;
                    selected.insert(pin_path, pin);
                }
            }
            selected.insert(path, raw);
        }
    }
    Ok(selected.into_iter().collect())
}

pub struct CommitLanding {
    pub landing: Value,
    pub receipt: Value,
    pub raw: Vec<u8>,
    pub name: String,
    pub directory: std::path::PathBuf,
}
pub fn commit_landing(
    plan: &Plan<'_>,
    commit: &str,
    expected: Option<&str>,
) -> Result<CommitLanding> {
    crate::product::commit_landing(plan, commit, expected)
}
pub fn landed_paths(plan: &Plan<'_>, proof: &CommitLanding) -> Result<Vec<String>> {
    crate::product::landed_paths(plan, proof)
}
