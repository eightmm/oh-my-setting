//! Retained hardlinks prove capture ownership. Journal rows, matching bytes,
//! room membership and filenames do not confer publication or GC authority.
use crate::{
    Result, V, digest, encoded, hex, obj, path_value,
    storage::{self, MAX_OBJECT, MAX_STREAM, Namespace},
    text,
};
use std::{
    collections::BTreeSet,
    fs::{self, Metadata},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

const JOURNAL: &str = ".oms/landings.jsonl";
const ROOTS: &[&str] = &[".oms/task", ".oms/tasks", ".oms/plan", ".oms/artifacts"];

/// One validated snapshot per command. Reuse it for aliases, capture reads and
/// admission checks; freshness compares the named generation without replaying
/// the ledger. A changed journal requires a new command snapshot.
pub struct CaptureIndex {
    pub(crate) repo: PathBuf,
    pub(crate) checkout: PathBuf,
    pub(crate) rows: Vec<V>,
    pub(crate) reservations: Vec<V>,
    namespace: Namespace,
    checkout_namespace: Namespace,
    journal: Option<Metadata>,
}
impl CaptureIndex {
    pub fn repository(&self) -> &Path {
        &self.repo
    }
    pub fn checkout(&self) -> &Path {
        &self.checkout
    }
    pub fn rows(&self) -> &[V] {
        &self.rows
    }
    pub fn reservations(&self) -> &[V] {
        &self.reservations
    }
    pub(crate) fn append_event(&mut self, event: &str, item: &V, extra: V) -> Result<V> {
        self.check()?;
        let mut row = obj([
            ("schema", 1usize.into()),
            ("event", event.into()),
            ("landing_id", item.get("landing_id").clone()),
            ("ts", crate::now().into()),
            ("managed_capture", item.clone()),
        ]);
        if let V::Obj(extra) = extra {
            for (key, value) in extra {
                if let V::Obj(ref mut fields) = row {
                    fields.insert(key, value);
                }
            }
        }
        let after = storage::append(&self.repo, Path::new(JOURNAL), &row, self.journal.as_ref())?;
        self.namespace.check()?;
        self.journal = Some(after);
        self.rows.push(row.clone());
        if event == "capture-reserved" {
            self.reservations.push(item.clone());
        }
        self.check()?;
        Ok(row)
    }
    pub fn load(repo: &Path) -> Result<Self> {
        let repo = fs::canonicalize(repo).map_err(|e| e.to_string())?;
        let path = repo.join(JOURNAL);
        let namespace = Namespace::snapshot(&repo, &path)?;
        let (raw, journal) = match fs::symlink_metadata(&path) {
            Ok(_) => {
                let (raw, info) = storage::regular(&path, MAX_STREAM, 1)?;
                (raw, Some(info))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
            Err(e) => return Err(e.to_string()),
        };
        namespace.check()?;
        let rows = parse_rows(&raw, "landing journal")?;
        let mut reservations = Vec::new();
        let mut generations = BTreeSet::new();
        for row in &rows {
            if row.get("event").text() != Some("capture-reserved") {
                continue;
            }
            let item = row.get("managed_capture");
            if !matches!(item, V::Obj(_))
                || item.get("version") != &1usize.into()
                || !item.get("bytes").integer(false)
                || item
                    .get("bytes")
                    .py()
                    .parse::<u64>()
                    .ok()
                    .is_none_or(|v| v > 2 * MAX_OBJECT as u64)
                || !item.get("directory").text().is_some_and(|v| !v.is_empty())
                || item.get("namespace").text() != Some(&digest(repo.as_os_str().as_bytes()))
                || !item.get("generation").text().is_some_and(|v| hex(v, 32))
                || !item.get("sha256").text().is_some_and(|v| hex(v, 64))
                || !item.get("landing_id").text().is_some_and(landing_id)
            {
                return Err("malformed managed capture reservation".into());
            }
            if !generations.insert(item.get("generation").py()) {
                return Err("duplicate managed capture reservation".into());
            }
            reservations.push(item.clone());
        }
        let checkout = repo.clone();
        let checkout_namespace = Namespace::snapshot(&checkout, &checkout.join(".namespace"))?;
        let result = Self {
            repo,
            checkout,
            rows,
            reservations,
            namespace,
            checkout_namespace,
            journal,
        };
        result.check()?;
        Ok(result)
    }
    pub fn load_checkout(repo: &Path, checkout: &Path) -> Result<Self> {
        let checkout = fs::canonicalize(checkout).map_err(|e| e.to_string())?;
        let checkout_namespace = Namespace::snapshot(&checkout, &checkout.join(".namespace"))?;
        let mut index = Self::load(repo)?;
        index.checkout = checkout;
        index.checkout_namespace = checkout_namespace;
        index.check()?;
        Ok(index)
    }
    pub fn check(&self) -> Result<()> {
        self.namespace.check()?;
        self.checkout_namespace.check()?;
        match (&self.journal, fs::symlink_metadata(self.repo.join(JOURNAL))) {
            (Some(before), Ok(after)) if storage::generation(before, &after) => Ok(()),
            (None, Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            _ => Err("managed capture index changed".into()),
        }
    }
    pub fn find(&self, capture: Option<&Path>, landing: Option<&str>) -> Result<&V> {
        let found = self
            .reservations
            .iter()
            .filter(|item| {
                capture.is_some_and(|path| capture_path(item).is_ok_and(|v| v == path))
                    || landing.is_some_and(|id| item.get("landing_id").text() == Some(id))
            })
            .collect::<Vec<_>>();
        if found.len() != 1 {
            return Err("unique native capture reservation unavailable".into());
        }
        Ok(found[0])
    }
    pub fn aliases(&self) -> Result<Vec<(PathBuf, BTreeSet<Vec<u8>>)>> {
        let mut aliases = Vec::new();
        for item in &self.reservations {
            for path in [
                capture_path(item)?,
                self.repo
                    .join(".oms/landing-patches")
                    .join(format!("{}.patch", text(item, "landing_id")?)),
            ] {
                let forms = reference_spellings(&self.repo, &path)?;
                aliases.push((path, forms));
            }
        }
        self.check()?;
        Ok(aliases)
    }
    pub fn certificate(&self, item: &V) -> Result<Certificate> {
        self.check()?;
        if !self
            .reservations
            .iter()
            .any(|registered| registered == item)
        {
            return Err("capture certificate has no registered reservation".into());
        }
        let root = PathBuf::from(text(item, "directory")?);
        if fs::canonicalize(&root).map_err(|e| e.to_string())? != root
            || root.file_name().and_then(|v| v.to_str())
                != Some(&format!(
                    "oms-landing-capture-{}",
                    text(item, "generation")?
                ))
        {
            return Err("capture directory moved or was redirected".into());
        }
        let directory = storage::private_directory(&root)?;
        let (raw, info) = storage::regular(&root.join("owner.json"), 16384, 1)?;
        let value = V::parse(&raw)?;
        if value.get("reservation") != item
            || value.get("directory") != &storage::identity(&directory)
            || !matches!(value.get("objects"), V::Obj(_))
        {
            return Err("capture generation owner mismatch".into());
        }
        validate_objects(&self.repo, item, &value)?;
        let certificate = Certificate {
            root,
            directory,
            raw,
            info,
            value,
        };
        certificate.check()?;
        self.check()?;
        Ok(certificate)
    }
    pub fn indexed_bytes(&self, input: &Path) -> Result<Option<Vec<u8>>> {
        let path = storage::absolute(input)?;
        if path.parent() == Some(self.repo.join(".oms/landing-patches").as_path()) {
            return self.read_patch(&path, MAX_OBJECT).map(Some);
        }
        let private_name = path
            .file_name()
            .and_then(|v| v.to_str())
            .and_then(|v| v.strip_suffix(".patch"))
            .is_some_and(landing_id)
            && path
                .parent()
                .and_then(|v| v.file_name())
                .is_some_and(|v| v.as_bytes().starts_with(b"oms-landing-capture-"));
        if private_name
            && self
                .reservations
                .iter()
                .any(|v| capture_path(v).is_ok_and(|p| p == path))
        {
            let item = self.find(Some(&path), None)?;
            let certificate = self.certificate(item)?;
            let object = certificate.value.get("objects").get("capture");
            let raw = object_bytes(item, object, 2)?;
            named_object(&path, object)?;
            certificate.check()?;
            self.check()?;
            return Ok(Some(raw));
        }
        self.check()?;
        Ok(None)
    }
    pub fn read_patch(&self, input: &Path, maximum: usize) -> Result<Vec<u8>> {
        // Callers reopen the original path physically; `..` after a symlinked
        // directory would resolve elsewhere than this lexical check.
        if input
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("patch evidence leaves repository".into());
        }
        let path = storage::absolute(&self.checkout.join(input))?;
        if path.parent() == Some(self.repo.join(".oms/landing-patches").as_path()) {
            let matching = self
                .reservations
                .iter()
                .filter(|v| {
                    path.file_name().and_then(|v| v.to_str())
                        == Some(&format!("{}.patch", v.get("landing_id").py()))
                })
                .collect::<Vec<_>>();
            if !matching.is_empty() {
                let namespace = Namespace::snapshot(&self.repo, &path)?;
                let named = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
                let mut candidates = Vec::new();
                for item in matching {
                    let certificate = self.certificate(item)?;
                    let object = certificate.value.get("objects").get("publication");
                    if object.get("path") == &path_value(&path) {
                        candidates.push((item, certificate));
                    }
                }
                if candidates.len() != 1 {
                    return Err("unique native publication ownership unavailable".into());
                }
                let (item, certificate) = &candidates[0];
                let object = certificate.value.get("objects").get("publication");
                let raw = object_bytes(item, object, 2)?;
                if raw.len() > maximum || storage::identity(&named) != *object.get("identity") {
                    return Err("landing publication replaced or oversized".into());
                }
                named_object(&path, object)?;
                certificate.check()?;
                namespace.check()?;
                self.check()?;
                return Ok(raw);
            }
        }
        let relative = path
            .strip_prefix(&self.checkout)
            .map_err(|_| "patch evidence leaves repository")?;
        let raw = storage::read_repo(&self.checkout, relative, maximum, false)?;
        self.check()?;
        Ok(raw)
    }

    /// Validates both the old packet and the exact proposed bytes. All
    /// arguments and attribution bytes participate in reference discovery.
    pub fn validate_write(
        &self,
        target: &Path,
        operation: &str,
        arguments: &[Vec<u8>],
        before: &[u8],
        proposed: &[u8],
        source_session: &[u8],
    ) -> Result<()> {
        let mut raw = operation.as_bytes().to_vec();
        for source in arguments
            .iter()
            .map(Vec::as_slice)
            .chain([before, proposed, source_session])
        {
            raw.push(b'\n');
            raw.extend_from_slice(source);
        }
        let mut found = false;
        for (path, forms) in self.aliases()? {
            if forms.iter().any(|form| contains(&raw, form)) {
                found = true;
                if self.indexed_bytes(&path)?.is_none() {
                    return Err("managed reference ownership unavailable".into());
                }
            }
        }
        if found && !supported(&self.repo, target)? {
            return Err("managed references require a scanned OMS evidence namespace".into());
        }
        self.check()
    }
    /// Compatibility validator: only designated note/section arguments are
    /// opened as files. Metadata paths remain literal reference candidates.
    pub fn validate(&self, target: &Path, values: &[Vec<u8>]) -> Result<()> {
        let before = if exists(target)? {
            storage::regular(target, MAX_STREAM, 1)?.0
        } else {
            Vec::new()
        };
        let operation = values.first().map(Vec::as_slice).unwrap_or_default();
        let position = match operation {
            b"agent_task_replace_section_unlocked" => Some(3),
            b"agent_task_append_bullet_unlocked"
            | b"agent_task_verify_finalize_unlocked"
            | b"agent_task_append_deduplicated_unlocked" => Some(4),
            _ => None,
        };
        let content = if let Some(position) = position {
            storage::regular(
                Path::new(std::ffi::OsStr::from_bytes(
                    values
                        .get(position)
                        .ok_or("missing reference content argument")?,
                )),
                MAX_STREAM,
                1,
            )?
            .0
        } else {
            Vec::new()
        };
        if operation == b"agent_task_append_deduplicated_unlocked" {
            dedupe_state(
                &before,
                &content,
                values.get(5).ok_or("missing task append dedupe key")?,
            )?;
        }
        self.validate_write(
            target,
            std::str::from_utf8(operation).map_err(|_| "invalid reference operation")?,
            values,
            &before,
            &content,
            b"",
        )
    }
    pub fn admit(&self, input: &Path, report: &Path) -> Result<bool> {
        let absolute = storage::absolute(input)?;
        let real = physical_target(input)?;
        let aliases = self.aliases()?;
        let known = aliases.iter().any(|(v, _)| *v == absolute || *v == real);
        if known {
            if absolute != real {
                return Err("redirected managed admission input".into());
            }
            if self.indexed_bytes(&absolute)?.is_none() {
                return Err("managed admission input ownership unavailable".into());
            }
            if !supported(&self.repo, report)? {
                return Err("managed admission requires an internal report".into());
            }
        }
        self.check()?;
        Ok(known)
    }
}

pub struct Certificate {
    root: PathBuf,
    directory: Metadata,
    raw: Vec<u8>,
    info: Metadata,
    pub(crate) value: V,
}
impl Certificate {
    pub fn value(&self) -> &V {
        &self.value
    }
    pub fn check(&self) -> Result<()> {
        if !storage::same(&self.directory, &storage::private_directory(&self.root)?)
            || !storage::generation(
                &self.info,
                &fs::symlink_metadata(self.root.join("owner.json")).map_err(|e| e.to_string())?,
            )
        {
            return Err("capture certificate changed".into());
        }
        Ok(())
    }
    pub(crate) fn save(&self, value: &V) -> Result<()> {
        self.check()?;
        if value.get("directory") != &storage::identity(&self.directory)
            || value.get("reservation") != self.value.get("reservation")
        {
            return Err("capture directory replaced".into());
        }
        let proposed = encoded(value)?;
        if proposed.len() > 16384 {
            return Err("capture certificate exceeds limit".into());
        }
        storage::replace(
            &self.root,
            Path::new("owner.json"),
            &self.raw,
            &proposed,
            false,
            Some(&self.info),
        )?;
        if !storage::same(&self.directory, &storage::private_directory(&self.root)?) {
            return Err("capture directory replaced during save".into());
        }
        Ok(())
    }
}

pub fn landing_id(value: &str) -> bool {
    value.strip_prefix("land-").is_some_and(|v| {
        (1..=150).contains(&v.len())
            && v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    })
}
pub fn capture_path(item: &V) -> Result<PathBuf> {
    Ok(Path::new(text(item, "directory")?).join(format!("{}.patch", text(item, "landing_id")?)))
}
pub(crate) fn object_bytes(item: &V, object: &V, links: u64) -> Result<Vec<u8>> {
    let (raw, info) = storage::regular(Path::new(text(object, "anchor")?), MAX_OBJECT, links)?;
    if storage::identity(&info) != *object.get("identity")
        || !object.get("size").integer(false)
        || object.get("size").py().parse::<usize>().ok() != Some(raw.len())
        || Some(digest(&raw).as_str()) != item.get("sha256").text()
    {
        return Err("native generation anchor changed".into());
    }
    Ok(raw)
}
fn validate_objects(repo: &Path, item: &V, value: &V) -> Result<()> {
    let V::Obj(objects) = value.get("objects") else {
        return Err("unknown capture object inventory".into());
    };
    let root = Path::new(text(item, "directory")?);
    for (name, object) in objects {
        let name = std::str::from_utf8(name).map_err(|_| "unknown capture object inventory")?;
        if !matches!(name, "capture" | "publication")
            || !matches!(object,V::Obj(fields) if fields.len()==4)
            || !object.get("size").integer(false)
            || object
                .get("size")
                .py()
                .parse::<u64>()
                .ok()
                .and_then(|v| v.checked_mul(2))
                != item.get("bytes").py().parse::<u64>().ok()
            || !matches!(object.get("identity"),V::Arr(v) if v.len()==2 && v.iter().all(|v|v.integer(false)))
            || object.get("anchor") != &path_value(&root.join(format!("{name}.anchor")))
        {
            return Err("capture object generation inventory is incomplete".into());
        }
        let path = Path::new(text(object, "path")?);
        if storage::absolute(path)? != path
            || (name == "capture" && path != capture_path(item)?)
            || (name == "publication" && (!path.starts_with(repo) || path == repo))
        {
            return Err("foreign capture object path".into());
        }
    }
    Ok(())
}
fn named_object(path: &Path, object: &V) -> Result<()> {
    let info = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !info.is_file() || storage::identity(&info) != *object.get("identity") || info.nlink() != 2 {
        return Err("indexed admission input was replaced".into());
    }
    Ok(())
}
pub fn supported(repo: &Path, input: &Path) -> Result<bool> {
    let physical = physical_target(input)?;
    Ok(ROOTS
        .iter()
        .any(|root| physical.starts_with(repo.join(root))))
}
fn physical_target(input: &Path) -> Result<PathBuf> {
    use std::{collections::VecDeque, ffi::OsString, path::Component};
    enum Step {
        Part(OsString),
        End(PathBuf),
    }
    let input = if input.is_absolute() {
        input.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(input)
    };
    let mut remaining = input
        .components()
        .map(|part| Step::Part(part.as_os_str().to_owned()))
        .collect::<VecDeque<Step>>();
    let mut resolved = PathBuf::new();
    let mut links = BTreeSet::new();
    while let Some(step) = remaining.pop_front() {
        let part = match step {
            Step::Part(part) => part,
            Step::End(path) => {
                links.remove(&path);
                continue;
            }
        };
        match Path::new(&part).components().next() {
            Some(Component::RootDir) => {
                resolved = PathBuf::from("/");
                continue;
            }
            Some(Component::ParentDir) => {
                resolved.pop();
                continue;
            }
            Some(Component::CurDir) | None => continue,
            _ => {}
        }
        let next = resolved.join(&part);
        match fs::symlink_metadata(&next) {
            Ok(info) if info.is_symlink() => {
                if !links.insert(next.clone()) {
                    return Err("reference path contains a symlink cycle".into());
                }
                let destination = fs::read_link(&next).map_err(|e| e.to_string())?;
                let parts = destination
                    .components()
                    .map(|part| part.as_os_str().to_owned())
                    .collect::<Vec<_>>();
                remaining.push_front(Step::End(next));
                for part in parts.into_iter().rev() {
                    remaining.push_front(Step::Part(part));
                }
            }
            Ok(_) => resolved = next,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => resolved = next,
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(resolved)
}
pub fn reference_spellings(repo: &Path, path: &Path) -> Result<BTreeSet<Vec<u8>>> {
    fn relative(repo: &Path, path: &Path) -> PathBuf {
        let a = repo.components().collect::<Vec<_>>();
        let b = path.components().collect::<Vec<_>>();
        let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
        let mut result = PathBuf::new();
        for _ in common..a.len() {
            result.push("..");
        }
        for part in &b[common..] {
            result.push(part.as_os_str());
        }
        if result.as_os_str().is_empty() {
            result.push(".");
        }
        result
    }
    let literal = path.as_os_str().as_bytes();
    let forward = literal
        .iter()
        .map(|b| if *b == b'\\' { b'/' } else { *b })
        .collect::<Vec<_>>();
    let mut result = BTreeSet::new();
    for raw in [
        literal.to_vec(),
        relative(repo, path).as_os_str().as_bytes().to_vec(),
        forward,
    ] {
        result.insert(raw.clone());
        result.insert(
            raw.iter()
                .flat_map(|b| {
                    if *b == b'/' {
                        vec![b'\\', b'/']
                    } else {
                        vec![*b]
                    }
                })
                .collect(),
        );
        let value = V::Str(raw);
        let unicode = value.json(false)?;
        result.insert(unicode[1..unicode.len() - 1].to_vec());
        let ascii = encoded(&value)?;
        result.insert(ascii[1..ascii.len() - 2].to_vec());
    }
    Ok(result)
}
pub fn dedupe_state(raw: &[u8], content: &[u8], key: &[u8]) -> Result<bool> {
    if !std::str::from_utf8(key).is_ok_and(|v| hex(v, 64)) {
        return Err("invalid task append dedupe key".into());
    }
    oms_command_task::packet::receipt(raw, content, key).map_err(|e| e.to_string())
}
pub(crate) fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}
pub(crate) fn parse_rows(raw: &[u8], label: &str) -> Result<Vec<V>> {
    if !raw.is_empty() && !raw.ends_with(b"\n") {
        return Err(format!("truncated {label}"));
    }
    let mut rows = Vec::new();
    for line in raw
        .split(|b| *b == b'\n')
        .filter(|v| v.iter().any(|b| !b.is_ascii_whitespace()))
    {
        let row = V::parse(line)?;
        if !matches!(row, V::Obj(_)) {
            return Err(format!("non-object {label} row"));
        }
        rows.push(row);
    }
    Ok(rows)
}

pub struct AdmissionRecords {
    pub(crate) relative: PathBuf,
    pub(crate) raw: Vec<u8>,
    pub(crate) row: V,
    report_info: Metadata,
    index_info: Metadata,
    report_namespace: Namespace,
    index_namespace: Namespace,
    repo: PathBuf,
}
impl AdmissionRecords {
    pub fn relative(&self) -> &Path {
        &self.relative
    }
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }
    pub fn row(&self) -> &V {
        &self.row
    }
    pub fn load(repo: &Path, report: &Path) -> Result<Self> {
        let absolute = storage::absolute(report)?;
        let relative = absolute
            .strip_prefix(repo.join(".oms/artifacts"))
            .map_err(|_| "admission witness requires canonical internal report")?;
        if relative.as_os_str().is_empty() {
            return Err("admission witness requires canonical internal report".into());
        }
        let relative = absolute
            .strip_prefix(repo)
            .map_err(|_| "foreign admission report")?
            .to_owned();
        let index = repo.join(".oms/artifacts/index.jsonl");
        let report_namespace = Namespace::snapshot(repo, &absolute)?;
        let index_namespace = Namespace::snapshot(repo, &index)?;
        let (raw, report_info) = storage::regular(&absolute, MAX_STREAM, 1)?;
        let (index_raw, index_info) = storage::regular(&index, MAX_STREAM, 1)?;
        if !index_raw.ends_with(b"\n") {
            return Err("incomplete native admission index".into());
        }
        let mut found = index_raw
            .split(|byte| *byte == b'\n')
            .filter(|line| line.iter().any(|byte| !byte.is_ascii_whitespace()))
            .map(V::parse)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|r| r.get("artifact") == &path_value(&relative))
            .collect::<Vec<_>>();
        if found.len() != 1 || found[0].get("artifact_sha256").text() != Some(&digest(&raw)) {
            return Err("unique unchanged native admission receipt unavailable".into());
        }
        let records = Self {
            relative,
            raw,
            row: found.remove(0),
            report_info,
            index_info,
            report_namespace,
            index_namespace,
            repo: repo.to_owned(),
        };
        records.check()?;
        Ok(records)
    }
    pub fn check(&self) -> Result<()> {
        self.report_namespace.check()?;
        self.index_namespace.check()?;
        if !storage::generation(
            &self.report_info,
            &fs::symlink_metadata(self.repo.join(&self.relative)).map_err(|e| e.to_string())?,
        ) || !storage::generation(
            &self.index_info,
            &fs::symlink_metadata(self.repo.join(".oms/artifacts/index.jsonl"))
                .map_err(|e| e.to_string())?,
        ) {
            return Err("native admission evidence changed".into());
        }
        Ok(())
    }
}

pub fn validate_admission_scope(
    capture: &Path,
    attempt: &str,
    records: &AdmissionRecords,
    created: &V,
    terminal: Option<&V>,
) -> Result<()> {
    let raw = std::str::from_utf8(&records.raw).map_err(|_| "invalid admission report text")?;
    let verdict = if records.row.get("exit") == &0usize.into() {
        "ADMIT"
    } else {
        "REJECT"
    };
    let patch = format!(
        "- patch: {}",
        capture.to_str().ok_or("invalid capture path text")?
    );
    if records.row.get("operation_id").text() != Some(attempt)
        || raw.lines().next() != Some(&format!("# Patch admission: {verdict}"))
        || raw
            .lines()
            .filter(|v| v.starts_with("- patch: "))
            .collect::<Vec<_>>()
            != vec![patch.as_str()]
    {
        return Err("admission producer/report scope differs from the actual local input".into());
    }
    let born = timestamp(created)?;
    let produced = timestamp(&records.row)?;
    if produced < born
        || terminal
            .map(timestamp)
            .transpose()?
            .is_some_and(|end| produced > end)
    {
        return Err("admission receipt is outside its actual operation lifetime".into());
    }
    records.check()
}
fn timestamp(value: &V) -> Result<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(text(value, "ts")?)
        .map_err(|_| "invalid admission receipt timestamp".into())
}

/// Called under the artifact index lock after durable report/index append.
/// Existing publication inputs never acquire a private outcome witness.
pub fn record_admission(
    index: &CaptureIndex,
    capture: &Path,
    attempt: &str,
    report: &Path,
    result: &str,
) -> Result<()> {
    if !index
        .reservations
        .iter()
        .any(|v| capture_path(v).is_ok_and(|p| p == capture))
    {
        index.check()?;
        return Ok(());
    }
    let item = index.find(Some(capture), None)?;
    let certificate = index.certificate(item)?;
    index
        .indexed_bytes(capture)?
        .ok_or("managed admission input ownership unavailable")?;
    let records = AdmissionRecords::load(&index.repo, report)?;
    let external = obj([
        (
            "name",
            path_value(Path::new(
                capture.file_name().ok_or("invalid capture input name")?,
            )),
        ),
        ("owned", false.into()),
        ("sha256", item.get("sha256").clone()),
    ]);
    if !matches!(result, "0" | "1")
        || records.row.get("kind").text() != Some("patch-admit")
        || !records.row.get("exit").integer(false)
        || records.row.get("exit").py() != result
        || records.row.get("patch_sha256") != item.get("sha256")
        || records.row.get("patch_external") != &external
    {
        return Err("native admission result does not match private input".into());
    }
    let (events, attempts) = oms_command_operations::load_projection(&index.repo)?;
    let created = owned_created(&events, &attempts, attempt, "patch-admit", capture)?;
    validate_admission_scope(capture, attempt, &records, created, None)?;
    let proof = obj([
        ("version", 1usize.into()),
        ("generation", item.get("generation").clone()),
        ("attempt", attempt.into()),
        ("created_sha256", digest(&encoded(created)?).into()),
        ("report", path_value(&records.relative)),
        ("report_sha256", digest(&records.raw).into()),
        ("index_event_id", records.row.get("event_id").clone()),
        ("index_row_sha256", digest(&encoded(&records.row)?).into()),
        ("exit", V::Num(result.into())),
    ]);
    if certificate.value.get("admission") != &V::Null
        && certificate.value.get("admission") != &proof
    {
        return Err("conflicting native admission witness".into());
    }
    records.check()?;
    index.check()?;
    certificate.check()?;
    let mut value = certificate.value.clone();
    value.set("admission", proof);
    certificate.save(&value)
}

pub fn admission_proof(
    index: &CaptureIndex,
    item: &V,
    certificate: &Certificate,
    expected: usize,
) -> Result<V> {
    certificate.check()?;
    let proof = admission_proof_value(index, item, &certificate.value, expected)?;
    certificate.check()?;
    Ok(proof)
}
pub(crate) fn admission_proof_value(
    index: &CaptureIndex,
    item: &V,
    value: &V,
    expected: usize,
) -> Result<V> {
    admission_evidence(index, item, value, expected).map(|(proof, _)| proof)
}
pub(crate) fn admission_evidence(
    index: &CaptureIndex,
    item: &V,
    value: &V,
    expected: usize,
) -> Result<(V, AdmissionRecords)> {
    let proof = value.get("admission");
    if !matches!(proof, V::Obj(_))
        || proof.get("version") != &1usize.into()
        || proof.get("generation") != item.get("generation")
        || proof.get("exit") != &expected.into()
    {
        return Err("native admission outcome witness missing".into());
    }
    let records = AdmissionRecords::load(&index.repo, &index.repo.join(text(proof, "report")?))?;
    if proof.get("report_sha256").text() != Some(&digest(&records.raw))
        || records.row.get("event_id") != proof.get("index_event_id")
        || proof.get("index_row_sha256").text() != Some(&digest(&encoded(&records.row)?))
        || records.row.get("kind").text() != Some("patch-admit")
        || !records.row.get("exit").integer(false)
        || records.row.get("exit") != &expected.into()
        || records.row.get("patch_sha256") != item.get("sha256")
    {
        return Err("native admission evidence changed".into());
    }
    let (events, attempts) = oms_command_operations::load_projection(&index.repo)?;
    let capture = Path::new(text(value.get("objects").get("capture"), "path")?);
    let attempt = text(proof, "attempt")?;
    let created = owned_created(&events, &attempts, attempt, "patch-admit", capture)?;
    let operation = attempts
        .iter()
        .find(|v| v.get("attempt_id").text() == Some(attempt))
        .ok_or("actual admission publisher has not completed")?;
    if proof.get("created_sha256").text() != Some(&digest(&encoded(created)?))
        || operation.get("state").text() != Some("done")
        || !matches!(operation.get("terminal"), V::Bool(true))
    {
        return Err("actual admission publisher has not completed".into());
    }
    let terminal = events
        .iter()
        .filter(|v| {
            v.get("attempt_id").text() == Some(attempt)
                && v.get("event_type").text() == Some("attempt.state_changed")
                && v.get("to_state").text() == Some("done")
        })
        .collect::<Vec<_>>();
    if terminal.len() != 1 {
        return Err("unique admission terminal producer event unavailable".into());
    }
    validate_admission_scope(capture, attempt, &records, created, Some(terminal[0]))?;
    index.check()?;
    Ok((proof.clone(), records))
}

pub(crate) fn owned_created<'a>(
    rows: &'a [V],
    attempts: &[V],
    attempt: &str,
    tool: &str,
    input: &Path,
) -> Result<&'a V> {
    let created = rows
        .iter()
        .filter(|v| {
            v.get("attempt_id").text() == Some(attempt)
                && v.get("event_type").text() == Some("attempt.created")
        })
        .collect::<Vec<_>>();
    let current = attempts
        .iter()
        .find(|v| v.get("attempt_id").text() == Some(attempt))
        .ok_or("native admission operation scope unavailable")?;
    if created.len() != 1
        || current.get("provider").text() != Some("local")
        || current.get("tool").text() != Some(tool)
        || current.get("parent_attempt_id").truth()
        || created[0].get("refs").get("reference_writer").text() != Some("landing-reference-v1")
        || created[0].get("refs").get("reference_input_sha256").text()
            != Some(&digest(storage::absolute(input)?.as_os_str().as_bytes()))
    {
        return Err("native admission operation scope unavailable".into());
    }
    Ok(created[0])
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|v| v == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::DirBuilder,
        io::ErrorKind,
        os::unix::fs::{DirBuilderExt, PermissionsExt},
    };

    struct TestRoot(PathBuf);
    impl TestRoot {
        fn new(name: &str) -> Self {
            let tmp = fs::canonicalize(std::env::temp_dir()).unwrap();
            let path = (0u32..)
                .find_map(|i| {
                    let candidate =
                        tmp.join(format!("oms-capture-{name}-{}-{i}", std::process::id()));
                    match DirBuilder::new().mode(0o700).create(&candidate) {
                        Ok(()) => Some(candidate),
                        Err(e) if e.kind() == ErrorKind::AlreadyExists => None,
                        Err(e) => panic!("create test root: {e}"),
                    }
                })
                .unwrap();
            Self(path)
        }
        fn mkdir(&self, rel: &str) -> PathBuf {
            let mut dir = self.0.clone();
            for part in Path::new(rel).components() {
                dir.push(part);
                if !dir.exists() {
                    DirBuilder::new().mode(0o700).create(&dir).unwrap();
                }
            }
            dir
        }
    }
    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn checkout_scoped_read_patch_accepts_worktree_artifacts_and_preserves_boundaries() {
        let root = TestRoot::new("checkout-read-patch");
        let state = root.mkdir("state");
        let checkout = root.mkdir("scratch/wt");
        let delegate_dir = root.mkdir("scratch/wt/.oms/artifacts/delegate");
        let patch_bytes =
            b"diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1,2 @@\n one\n+two\n";
        let checkout_patch = delegate_dir.join("worker.patch");
        fs::write(&checkout_patch, patch_bytes).unwrap();
        fs::set_permissions(&checkout_patch, fs::Permissions::from_mode(0o600)).unwrap();

        let state_only = CaptureIndex::load(&state).unwrap();
        assert_eq!(state_only.repository(), state.as_path());
        assert_eq!(state_only.checkout(), state.as_path());
        assert_eq!(
            state_only.read_patch(&checkout_patch, 4096).unwrap_err(),
            "patch evidence leaves repository"
        );

        let index = CaptureIndex::load_checkout(&state, &checkout).unwrap();
        assert_eq!(index.repository(), state.as_path());
        assert_eq!(index.checkout(), checkout.as_path());
        assert_eq!(
            index
                .read_patch(Path::new(".oms/artifacts/delegate/worker.patch"), 4096)
                .unwrap(),
            patch_bytes
        );
        assert_eq!(
            index.read_patch(&checkout_patch, 4096).unwrap(),
            patch_bytes
        );
        assert!(
            index
                .read_patch(Path::new(".oms/artifacts/delegate/worker.patch"), 8)
                .is_err()
        );

        let outside = root.mkdir("outside").join("escape.patch");
        fs::write(&outside, patch_bytes).unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
        let state_delegate = root
            .mkdir("state/.oms/artifacts/delegate")
            .join("main.patch");
        fs::write(&state_delegate, patch_bytes).unwrap();
        fs::set_permissions(&state_delegate, fs::Permissions::from_mode(0o600)).unwrap();
        let unreserved_landing = root.mkdir("state/.oms/landing-patches").join("plain.patch");
        fs::write(&unreserved_landing, patch_bytes).unwrap();
        fs::set_permissions(&unreserved_landing, fs::Permissions::from_mode(0o600)).unwrap();

        for escaped in [
            Path::new("../escape.patch"),
            outside.as_path(),
            state_delegate.as_path(),
            unreserved_landing.as_path(),
        ] {
            assert_eq!(
                index.read_patch(escaped, 4096).unwrap_err(),
                "patch evidence leaves repository",
                "{escaped:?}"
            );
        }

        let file_link = delegate_dir.join("link.patch");
        std::os::unix::fs::symlink(&outside, &file_link).unwrap();
        assert!(
            index
                .read_patch(Path::new(".oms/artifacts/delegate/link.patch"), 4096)
                .unwrap_err()
                .contains("unsafe or foreign repository input")
        );

        let pivot_target = root.mkdir("state/.oms/artifacts/delegate/subdir");
        std::os::unix::fs::symlink(&pivot_target, delegate_dir.join("pivot")).unwrap();
        fs::write(delegate_dir.join("p.patch"), patch_bytes).unwrap();
        fs::write(pivot_target.parent().unwrap().join("p.patch"), patch_bytes).unwrap();
        assert_eq!(
            index
                .read_patch(Path::new(".oms/artifacts/delegate/pivot/../p.patch"), 4096)
                .unwrap_err(),
            "patch evidence leaves repository"
        );

        let dir_link = checkout.join(".oms/artifacts/linked");
        std::os::unix::fs::symlink(outside.parent().unwrap(), &dir_link).unwrap();
        assert!(
            index
                .read_patch(Path::new(".oms/artifacts/linked/escape.patch"), 4096)
                .unwrap_err()
                .contains("unsafe or foreign reference namespace")
        );

        // Certified state-repo landing-patches reservation logic remains unchanged.
        let generation = "0123456789abcdef0123456789abcdef";
        let landing = "land-20261126T000000Z-0123456789abcdef";
        let cap_dir = root.mkdir(&format!("oms-landing-capture-{generation}"));
        let sha = digest(patch_bytes);
        let item = obj([
            ("version", 1usize.into()),
            ("generation", generation.into()),
            ("landing_id", landing.into()),
            ("namespace", digest(state.as_os_str().as_bytes()).into()),
            ("directory", path_value(&cap_dir)),
            ("bytes", V::Num((2 * patch_bytes.len()).to_string())),
            ("sha256", sha.into()),
        ]);
        storage::ensure_parent(&state, Path::new(JOURNAL)).unwrap();
        let mut mutable = CaptureIndex::load(&state).unwrap();
        mutable
            .append_event("capture-reserved", &item, obj([]))
            .unwrap();
        let dir_info = storage::private_directory(&cap_dir).unwrap();
        let cap_file = cap_dir.join(format!("{landing}.patch"));
        let cap_info = storage::create_private(
            &cap_dir,
            Path::new(&format!("{landing}.patch")),
            patch_bytes,
        )
        .unwrap();
        storage::link(&cap_file, &cap_dir, Path::new("capture.anchor"), &cap_info).unwrap();
        let pub_anchor = cap_dir.join("publication.anchor");
        let pub_info =
            storage::create_private(&cap_dir, Path::new("publication.anchor"), patch_bytes)
                .unwrap();
        let published = state
            .join(".oms/landing-patches")
            .join(format!("{landing}.patch"));
        let pub_rel = Path::new(".oms/landing-patches").join(format!("{landing}.patch"));
        storage::link(&pub_anchor, &state, &pub_rel, &pub_info).unwrap();
        let cert = obj([
            ("reservation", item),
            ("directory", storage::identity(&dir_info)),
            (
                "objects",
                obj([
                    (
                        "capture",
                        obj([
                            ("identity", storage::identity(&cap_info)),
                            ("size", patch_bytes.len().into()),
                            ("path", path_value(&cap_file)),
                            ("anchor", path_value(&cap_dir.join("capture.anchor"))),
                        ]),
                    ),
                    (
                        "publication",
                        obj([
                            ("identity", storage::identity(&pub_info)),
                            ("size", patch_bytes.len().into()),
                            ("path", path_value(&published)),
                            ("anchor", path_value(&pub_anchor)),
                        ]),
                    ),
                ]),
            ),
            ("terminal", V::Null),
        ]);
        storage::create_private(&cap_dir, Path::new("owner.json"), &encoded(&cert).unwrap())
            .unwrap();

        let reserved = CaptureIndex::load_checkout(&state, &checkout).unwrap();
        assert_eq!(
            reserved.indexed_bytes(&published).unwrap().as_deref(),
            Some(patch_bytes.as_slice())
        );
        assert!(reserved.read_patch(&published, 4).is_err());
    }
}
