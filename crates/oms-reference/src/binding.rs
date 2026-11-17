//! Admission reuse binds code, policy, environment and authenticated native
//! plan lineage. Unavailable optional context always selects fresh admission.
use crate::{
    Result, V,
    capture::{self, CaptureIndex},
    checkpoint,
    controller::Controller,
    digest, encoded, hex, obj, path_value,
    producer::{AllocationError, Producer},
    repository, storage, text,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

/// Exact shell order: verifier, ML, task, verifier-change, test-reduction,
/// floor-change reason, restructure and encoding-loss flags. Values stay text.
pub struct Parameters {
    pub values: [String; 8],
}
impl Parameters {
    pub fn task(&self) -> &str {
        &self.values[2]
    }
    fn value(&self) -> V {
        V::Arr(
            self.values
                .iter()
                .map(|value| value.as_str().into())
                .collect(),
        )
    }
}
pub struct PrefixCheck {
    pub id: String,
    pub verify: String,
}
impl PrefixCheck {
    fn value(&self) -> Result<V> {
        if self.id.is_empty() || self.verify.is_empty() || self.verify.contains('\0') {
            return Err("invalid batch prefix verifier".into());
        }
        Ok(obj([
            ("id", self.id.clone().into()),
            ("verify", self.verify.clone().into()),
        ]))
    }
}
pub struct AcceptedPrefix {
    pub check: PrefixCheck,
    pub panel_accept: V,
}
pub struct BatchPrefix {
    pub batches: Vec<V>,
    pub checks: Vec<PrefixCheck>,
}
pub struct PanelIntent {
    pub intent: V,
    pub receipt_sha256: String,
    pub has_complete: bool,
    pub has_abandoned: bool,
}

/// Read-only native producers own their actual dispatch grants, plan leases,
/// first-parent publication receipts and batch proofs. These methods must not
/// execute the verifiers they return or derive authority from room membership.
pub trait Context {
    fn effective_allowed_paths(
        &self,
        repo: &Path,
        task: &V,
        interrupted: &AtomicBool,
    ) -> Result<Vec<String>>;
    fn accepted_prefix(
        &self,
        repo: &Path,
        execution: &Path,
        intents: &[PanelIntent],
        interrupted: &AtomicBool,
    ) -> Result<Vec<AcceptedPrefix>>;
    fn goal_batch_prefix(
        &self,
        repo: &Path,
        execution: &Path,
        task: &str,
        interrupted: &AtomicBool,
    ) -> Result<BatchPrefix>;
}
pub struct Unavailable;
impl Context for Unavailable {
    fn effective_allowed_paths(&self, _: &Path, _: &V, _: &AtomicBool) -> Result<Vec<String>> {
        Err("native admission allowed-path producer unavailable".into())
    }
    fn accepted_prefix(
        &self,
        _: &Path,
        _: &Path,
        _: &[PanelIntent],
        _: &AtomicBool,
    ) -> Result<Vec<AcceptedPrefix>> {
        Err("native panel acceptance prefix producer unavailable".into())
    }
    fn goal_batch_prefix(
        &self,
        _: &Path,
        _: &Path,
        _: &str,
        _: &AtomicBool,
    ) -> Result<BatchPrefix> {
        Err("native goal batch prefix producer unavailable".into())
    }
}

pub struct Domain<'a> {
    pub harness_root: &'a Path,
    pub context: &'a dyn Context,
}
impl Domain<'_> {
    pub fn batch_prefix(
        &self,
        index: &CaptureIndex,
        execution: &Path,
        task: &str,
        interrupted: &AtomicBool,
    ) -> Result<V> {
        if task.is_empty() {
            return Ok(obj([
                ("batches", V::Arr(Vec::new())),
                ("checks", V::Arr(Vec::new())),
            ]));
        }
        crate::producer::parent_guard().map_err(|_| "batch prefix derivation is parent-only")?;
        checkpoint(interrupted)?;
        let accepted = self.context.accepted_prefix(
            index.repository(),
            execution,
            &panel_accept_intents(index.rows())?,
            interrupted,
        )?;
        let batch =
            self.context
                .goal_batch_prefix(index.repository(), execution, task, interrupted)?;
        if batch.batches.iter().any(|row| !matches!(row, V::Obj(_))) {
            return Err("invalid native batch prefix evidence".into());
        }
        let mut checks = batch
            .checks
            .iter()
            .map(PrefixCheck::value)
            .collect::<Result<Vec<_>>>()?;
        let mut value = obj([
            ("batches", V::Arr(batch.batches)),
            ("checks", V::Arr(Vec::new())),
        ]);
        if !accepted.is_empty() {
            let seen = batch
                .checks
                .iter()
                .map(|check| check.id.as_str())
                .collect::<BTreeSet<_>>();
            let mut records = Vec::new();
            for item in &accepted {
                if !matches!(&item.panel_accept, V::Obj(_)) {
                    return Err("invalid native panel acceptance prefix evidence".into());
                }
                let mut record = item.check.value()?;
                record.set("panel_accept", item.panel_accept.clone());
                records.push(record);
                if !seen.contains(item.check.id.as_str()) {
                    checks.push(item.check.value()?);
                }
            }
            value.set("panel_accepts", V::Arr(records));
        }
        value.set("checks", V::Arr(checks));
        index.check()?;
        checkpoint(interrupted)?;
        Ok(value)
    }
    pub fn admission_binding(
        &self,
        index: &CaptureIndex,
        execution: &Path,
        parameters: &Parameters,
        interrupted: &AtomicBool,
    ) -> Result<V> {
        let controller = Controller::capture_current()?;
        self.admission_binding_with_controller(
            index,
            execution,
            parameters,
            &controller,
            interrupted,
        )
    }
    pub fn admission_binding_with_controller(
        &self,
        index: &CaptureIndex,
        execution: &Path,
        parameters: &Parameters,
        controller: &Controller,
        interrupted: &AtomicBool,
    ) -> Result<V> {
        checkpoint(interrupted)?;
        let execution = fs::canonicalize(execution).map_err(|e| e.to_string())?;
        let harness_root = fs::canonicalize(self.harness_root).map_err(|e| e.to_string())?;
        let mut stamps = Vec::new();
        let harness = harness_inventory(&harness_root, &mut stamps, interrupted)?;
        let native = controller.digest()?.to_owned();
        let mut environment = BTreeMap::new();
        for (name, value) in std::env::vars_os() {
            if ["_", "SHLVL", "PWD", "OLDPWD", "CODEX_TOOL_CALL_ID"]
                .iter()
                .any(|skip| name == *skip)
            {
                continue;
            }
            environment.insert(name.as_bytes().to_vec(), V::Str(value.as_bytes().to_vec()));
        }
        let config = git_path(
            &execution,
            &["rev-parse", "--git-path", "config"],
            interrupted,
        )?;
        let common = git_path(&execution, &["rev-parse", "--git-common-dir"], interrupted)?;
        let mut allowed = Vec::new();
        let plan_digest = if !parameters.task().is_empty() {
            let path = index.repository().join(".oms/plan/tasks.json");
            let namespace = storage::Namespace::snapshot(index.repository(), &path)?;
            let (raw, info) = storage::regular(&path, 4 * 1024 * 1024, 1)?;
            namespace.check()?;
            let plan = V::parse(&raw)?;
            let task = plan.get("tasks").get(parameters.task());
            if !matches!(task, V::Obj(_)) || task.get("id").text() != Some(parameters.task()) {
                return Err("native admission plan task unavailable".into());
            }
            allowed =
                self.context
                    .effective_allowed_paths(index.repository(), task, interrupted)?;
            stamps.push(Stamp::File(path, Some(info), namespace));
            digest(&raw).into()
        } else {
            V::Null
        };
        let mut seeds = BTreeMap::new();
        for name in ["AGENTS.md", "CLAUDE.md", "GEMINI.md", "PROJECT.md"] {
            seeds.insert(
                name.as_bytes().to_vec(),
                optional(&execution.join(name), &mut stamps)?,
            );
        }
        let prefix = self.batch_prefix(index, &execution, parameters.task(), interrupted)?;
        let base = git_text(&execution, &["rev-parse", "HEAD"], interrupted)?;
        if !hex(&base, 40) && !hex(&base, 64) {
            return Err("invalid admission Git base".into());
        }
        let tree_ref = format!("{base}^{{tree}}");
        let tree = git_text(&execution, &["rev-parse", &tree_ref], interrupted)?;
        let value = obj([
            ("version", 1usize.into()),
            ("execution_repo", path_value(&execution)),
            ("base", base.clone().into()),
            ("tree", tree.into()),
            (
                "parameters_sha256",
                digest(&encoded(&V::Arr(vec![
                    parameters.value(),
                    V::Arr(allowed.into_iter().map(V::from).collect()),
                ]))?)
                .into(),
            ),
            (
                "environment_sha256",
                digest(&encoded(&V::Obj(environment))?).into(),
            ),
            (
                "harness_sha256",
                digest(&encoded(&V::Arr(vec![V::Arr(harness), native.into()]))?).into(),
            ),
            ("seeds", V::Obj(seeds)),
            ("plan_sha256", plan_digest),
            ("batch_prefix_sha256", digest(&encoded(&prefix)?).into()),
            ("git_config_sha256", optional(&config, &mut stamps)?),
            (
                "git_exclude_sha256",
                optional(&common.join("info/exclude"), &mut stamps)?,
            ),
        ]);
        for stamp in stamps {
            stamp.check()?;
        }
        controller.check()?;
        if git_text(&execution, &["rev-parse", "HEAD"], interrupted)? != base {
            return Err("admission inputs changed during binding".into());
        }
        index.check()?;
        checkpoint(interrupted)?;
        Ok(value)
    }
    pub fn current_admission(&self, index: &CaptureIndex, item: &V, binding: &V) -> Result<bool> {
        let certificate = index.certificate(item)?;
        if certificate.value().get("terminal").truth()
            || certificate
                .value()
                .get("objects")
                .get("publication")
                .truth()
        {
            return Ok(false);
        }
        let (proof, records) = capture::admission_evidence(index, item, certificate.value(), 0)?;
        if proof.get("binding") != binding
            || ["verify_sha256", "floor_sha256", "surface_sha256"]
                .iter()
                .any(|key| !proof.get(key).text().is_some_and(|value| hex(value, 64)))
        {
            return Ok(false);
        }
        let fields = [
            ("admission_binding_sha256", binding_digest(binding)?),
            ("verify_sha256", text(&proof, "verify_sha256")?.to_owned()),
            (
                "floor_verify_sha256",
                text(&proof, "floor_sha256")?.to_owned(),
            ),
            (
                "verification_surface_sha256",
                text(&proof, "surface_sha256")?.to_owned(),
            ),
        ];
        let raw =
            std::str::from_utf8(records.raw()).map_err(|_| "invalid admission report text")?;
        let header = raw.split("\n\n## Ladder\n").next().unwrap_or_default();
        for (key, value) in fields {
            let prefix = format!("- {key}: ");
            let expected = format!("{prefix}{value}");
            if header
                .lines()
                .filter(|line| line.starts_with(&prefix))
                .collect::<Vec<_>>()
                != vec![expected.as_str()]
            {
                return Ok(false);
            }
        }
        index
            .indexed_bytes(Path::new(text(
                certificate.value().get("objects").get("capture"),
                "path",
            )?))?
            .ok_or("native admission input ownership unavailable")?;
        records.check()?;
        certificate.check()?;
        index.check()?;
        Ok(true)
    }
    pub fn require_current_admission(
        &self,
        index: &CaptureIndex,
        capture: &Path,
        execution: &Path,
        parameters: &Parameters,
        interrupted: &AtomicBool,
    ) -> Result<()> {
        let controller = Controller::capture_current()?;
        self.require_current_admission_with_controller(
            index,
            capture,
            execution,
            parameters,
            &controller,
            interrupted,
        )
    }
    pub fn require_current_admission_with_controller(
        &self,
        index: &CaptureIndex,
        capture: &Path,
        execution: &Path,
        parameters: &Parameters,
        controller: &Controller,
        interrupted: &AtomicBool,
    ) -> Result<()> {
        let binding = self.admission_binding_with_controller(
            index,
            execution,
            parameters,
            controller,
            interrupted,
        )?;
        let item = index.find(Some(capture), None)?;
        if !self.current_admission(index, item, &binding)? {
            return Err("admission inputs changed; verification required".into());
        }
        controller.check()?;
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn bind_admission(
        &self,
        index: &CaptureIndex,
        capture: &Path,
        execution: &Path,
        before: &V,
        verify: &str,
        floor: &str,
        surface: &str,
        parameters: &Parameters,
        interrupted: &AtomicBool,
    ) -> Result<()> {
        let controller = Controller::capture_current()?;
        self.bind_admission_with_controller(
            index,
            capture,
            execution,
            before,
            verify,
            floor,
            surface,
            parameters,
            &controller,
            interrupted,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn bind_admission_with_controller(
        &self,
        index: &CaptureIndex,
        capture: &Path,
        execution: &Path,
        before: &V,
        verify: &str,
        floor: &str,
        surface: &str,
        parameters: &Parameters,
        controller: &Controller,
        interrupted: &AtomicBool,
    ) -> Result<()> {
        if before
            != &self.admission_binding_with_controller(
                index,
                execution,
                parameters,
                controller,
                interrupted,
            )?
        {
            return Err("admission inputs changed during verification".into());
        }
        let item = index.find(Some(capture), None)?;
        let certificate = index.certificate(item)?;
        let mut proof = capture::admission_proof(index, item, &certificate, 0)?;
        proof.set("binding", before.clone());
        proof.set("verify_sha256", digest(verify.as_bytes()));
        proof.set("floor_sha256", digest(floor.as_bytes()));
        proof.set("surface_sha256", digest(surface.as_bytes()));
        let mut value = certificate.value().clone();
        value.set("admission", proof);
        checkpoint(interrupted)?;
        controller.check()?;
        index.check()?;
        certificate.save(&value)
    }
}
impl Producer {
    /// Binding is an optimization. Missing/stale producer evidence takes the
    /// same fresh reservation/copy path as canonical intake; it is never ADMIT.
    #[allow(clippy::too_many_arguments)]
    pub fn intake(
        &mut self,
        domain: &Domain<'_>,
        source: &Path,
        landing: &str,
        expected: &str,
        execution: &Path,
        parameters: &Parameters,
        interrupted: &AtomicBool,
    ) -> std::result::Result<PathBuf, AllocationError> {
        match Controller::capture_current() {
            Ok(controller) => self.intake_with_controller(
                domain,
                source,
                landing,
                expected,
                execution,
                parameters,
                &controller,
                interrupted,
            ),
            Err(_) => self.allocate(source, landing, expected, execution, interrupted),
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn intake_with_controller(
        &mut self,
        domain: &Domain<'_>,
        source: &Path,
        landing: &str,
        expected: &str,
        execution: &Path,
        parameters: &Parameters,
        controller: &Controller,
        interrupted: &AtomicBool,
    ) -> std::result::Result<PathBuf, AllocationError> {
        let binding = domain
            .admission_binding_with_controller(
                &self.index,
                execution,
                parameters,
                controller,
                interrupted,
            )
            .ok();
        if let Some(binding) = binding {
            for item in self.index.reservations.iter().rev() {
                checkpoint(interrupted)?;
                if item.get("sha256").text() == Some(expected)
                    && domain
                        .current_admission(&self.index, item, &binding)
                        .unwrap_or(false)
                    && controller.check().is_ok()
                {
                    return capture::capture_path(item).map_err(AllocationError::from);
                }
            }
        }
        self.allocate(source, landing, expected, execution, interrupted)
    }
    pub fn refresh_admission(
        &mut self,
        capture: &Path,
        landing: &str,
        expected: &str,
        execution: &Path,
        interrupted: &AtomicBool,
    ) -> std::result::Result<PathBuf, AllocationError> {
        let item = self.index.find(Some(capture), None)?;
        let certificate = self.index.certificate(item)?;
        if certificate.value().get("admission").truth() {
            self.allocate(capture, landing, expected, execution, interrupted)
        } else {
            checkpoint(interrupted)?;
            Ok(capture.to_owned())
        }
    }
}
pub fn binding_digest(value: &V) -> Result<String> {
    Ok(digest(&encoded(value)?))
}

pub fn panel_accept_intents(log: &[V]) -> Result<Vec<PanelIntent>> {
    let fields = [
        "landing_id",
        "patch",
        "patch_sha",
        "base_sha",
        "task",
        "lease",
        "plan_receipt_sha",
        "plan_done_receipt_sha",
        "approval",
        "approval_version",
    ];
    let mut order = Vec::new();
    let mut intents = BTreeMap::new();
    let mut terminals: BTreeMap<&str, Vec<&V>> = BTreeMap::new();
    for row in log {
        let Some(id) = row
            .get("landing_id")
            .text()
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        match row.get("event").text() {
            Some("intent") => {
                if !intents.contains_key(id) {
                    order.push(id);
                    intents.insert(id, row);
                }
            }
            Some("complete" | "abandoned") => terminals.entry(id).or_default().push(row),
            _ => {}
        }
    }
    let canonical = |row: &V| -> Option<V> {
        let mut value = obj([("schema", 1usize.into())]);
        for key in fields {
            let text = row.default(key, "");
            text.text()?;
            value.set(key, text);
        }
        Some(value)
    };
    let mut result = Vec::new();
    for id in order {
        let intent = intents[id];
        if intent.get("panel_accept").text() != Some("true") {
            continue;
        }
        let expected = canonical(intent);
        let receipt = expected
            .as_ref()
            .map(|value| value.json(false).map(|raw| digest(&raw)))
            .transpose()?
            .unwrap_or_default();
        let mut complete = false;
        let mut abandoned = false;
        if let Some(expected) = expected {
            for terminal in terminals.get(id).into_iter().flatten() {
                if terminal.get("schema") == &1usize.into()
                    && canonical(terminal).as_ref() == Some(&expected)
                    && terminal.default("panel_accept", "") == intent.default("panel_accept", "")
                    && terminal.default("plan_id", "") == intent.default("plan_id", "")
                    && terminal.get("receipt_sha").text() == Some(&receipt)
                    && terminal.get("worktree") == intent.get("worktree")
                {
                    complete |= terminal.get("event").text() == Some("complete");
                    abandoned |= terminal.get("event").text() == Some("abandoned");
                }
            }
        }
        result.push(PanelIntent {
            intent: intent.clone(),
            receipt_sha256: receipt,
            has_complete: complete,
            has_abandoned: abandoned,
        });
    }
    Ok(result)
}

enum Stamp {
    File(PathBuf, Option<fs::Metadata>, storage::Namespace),
    Directory(PathBuf, fs::Metadata),
}
impl Stamp {
    fn check(&self) -> Result<()> {
        match self {
            Self::File(path, before, namespace) => {
                namespace.check()?;
                match (before, fs::symlink_metadata(path)) {
                    (Some(before), Ok(after)) if storage::generation(before, &after) => Ok(()),
                    (None, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    _ => Err("admission binding input changed".into()),
                }
            }
            Self::Directory(path, before) => {
                let after = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
                if !after.is_dir() || !storage::generation(before, &after) {
                    return Err("admission harness inventory changed".into());
                }
                Ok(())
            }
        }
    }
}
fn optional(path: &Path, stamps: &mut Vec<Stamp>) -> Result<V> {
    let parent = path.parent().ok_or("binding input has no parent")?;
    let mut root = parent;
    while !capture::exists(root)? {
        root = root.parent().ok_or("binding input namespace unavailable")?;
    }
    let root = fs::canonicalize(root).map_err(|e| e.to_string())?;
    let namespace = storage::Namespace::snapshot(&root, path)?;
    let (value, info) = if capture::exists(path)? {
        let (raw, info) = storage::regular(path, storage::MAX_STREAM, 1)?;
        (digest(&raw).into(), Some(info))
    } else {
        (V::Null, None)
    };
    namespace.check()?;
    stamps.push(Stamp::File(path.to_owned(), info, namespace));
    Ok(value)
}
fn harness_inventory(
    root: &Path,
    stamps: &mut Vec<Stamp>,
    interrupted: &AtomicBool,
) -> Result<Vec<V>> {
    let mut result = Vec::new();
    let mut total = 0usize;
    let mut stack = ["templates", "rules", "config", "scripts"]
        .iter()
        .map(|name| root.join(name))
        .collect::<Vec<_>>();
    while let Some(directory) = stack.pop() {
        checkpoint(interrupted)?;
        let before = fs::symlink_metadata(&directory).map_err(|e| e.to_string())?;
        if !before.is_dir() {
            return Err("redirected admission harness".into());
        }
        let namespace = storage::Namespace::snapshot(root, &directory.join(".namespace"))?;
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for entry in fs::read_dir(&directory).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() && entry.file_name() == "__pycache__" {
                continue;
            }
            if kind.is_dir() {
                dirs.push(entry.path());
            } else {
                files.push(entry.path());
            }
        }
        dirs.sort();
        files.sort();
        for path in files {
            let name = path
                .file_name()
                .ok_or("harness file has no name")?
                .as_bytes();
            if name.ends_with(b".pyc") || name.ends_with(b".pyo") {
                continue;
            }
            let (raw, info) = storage::regular(&path, storage::MAX_STREAM, 1)?;
            total = total
                .checked_add(raw.len())
                .ok_or("admission harness exceeds binding limit")?;
            if total > 64 * 1024 * 1024 || result.len() >= 8192 {
                return Err("admission harness exceeds binding limit".into());
            }
            result.push(V::Arr(vec![
                path_value(
                    path.strip_prefix(root)
                        .map_err(|_| "foreign harness file")?,
                ),
                V::Num((info.mode() & 0o7777).to_string()),
                digest(&raw).into(),
            ]));
            stamps.push(Stamp::File(
                path,
                Some(info),
                storage::Namespace::snapshot(root, &directory.join(".namespace"))?,
            ));
        }
        namespace.check()?;
        for path in dirs.into_iter().rev() {
            stack.push(path);
        }
        stamps.push(Stamp::Directory(directory, before));
    }
    Ok(result)
}
fn git_text(repo: &Path, args: &[&str], interrupted: &AtomicBool) -> Result<String> {
    let raw = repository::git(repo, args, interrupted)?;
    let raw = raw
        .into_iter()
        .filter(|byte| *byte != b'\r')
        .collect::<Vec<_>>();
    let start = raw
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(raw.len());
    let end = raw
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |n| n + 1);
    String::from_utf8(raw[start..end].to_vec())
        .map_err(|_| "invalid admission Git query text".into())
}
fn git_path(repo: &Path, args: &[&str], interrupted: &AtomicBool) -> Result<PathBuf> {
    let path = PathBuf::from(git_text(repo, args, interrupted)?);
    Ok(if path.is_absolute() {
        path
    } else {
        repo.join(path)
    })
}
