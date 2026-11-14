use crate::{Error, Result, notebook, util::*};
use oms_graph_python::{Parser, Session};
use oms_safe_fs::{Policy, Root};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub const GRAPH_LIMIT: usize = 256 * 1024 * 1024;
const SCHEMA: u32 = 2;
const VERSION: u32 = 9;
const EXCLUDES: &[&str] = &[
    ".git",
    ".oms",
    "node_modules",
    "vendor",
    "dist",
    "build",
    "target",
    "__pycache__",
];
#[derive(Clone, Debug)]
pub struct BuildOptions {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub max_bytes: i64,
    pub force: bool,
}
impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            include: vec![],
            exclude: vec![],
            max_bytes: 2 * 1024 * 1024,
            force: false,
        }
    }
}
impl BuildOptions {
    fn manifest(value: &Value) -> Self {
        Self {
            include: array(&value["discovery"], "include")
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            exclude: array(&value["discovery"], "exclude")
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            max_bytes: value["discovery"]["max_bytes"]
                .as_i64()
                .unwrap_or(2 * 1024 * 1024),
            force: false,
        }
    }
}
pub fn test_path(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    parts[..parts.len().saturating_sub(1)].iter().any(|p| {
        [
            "test",
            "tests",
            "testing",
            "__tests__",
            "e2e",
            "integration-tests",
            "integration_tests",
        ]
        .contains(&p.to_lowercase().as_str())
    }) || [
        "test_*.py",
        "*_test.py",
        "*_spec.py",
        "test-*.sh",
        "*-test.sh",
        "*_test.sh",
        "*-smoke.sh",
    ]
    .iter()
    .any(|p| glob(p, parts.last().copied().unwrap_or("")))
}
fn language(path: &str) -> Option<(&'static str, u32)> {
    if path.to_lowercase().ends_with(".ipynb") {
        Some(("notebook", 1))
    } else {
        oms_graph::select(path).language_version()
    }
}
fn excluded(path: &str) -> bool {
    path.split('/').any(|p| EXCLUDES.contains(&p))
}
fn walk(root: &Path, rel: &Path, files: &mut BTreeSet<String>, cancel: &AtomicBool) -> Result<()> {
    crate::util::check(cancel)?;
    for entry in std::fs::read_dir(root.join(rel))? {
        let entry = entry?;
        let path = rel.join(entry.file_name());
        let label = path.to_string_lossy().replace('\\', "/");
        if entry.file_type()?.is_dir() && !excluded(&label) {
            walk(root, &path, files, cancel)?;
        } else {
            files.insert(label);
        }
    }
    Ok(())
}
struct Discovery {
    files: BTreeMap<String, Vec<u8>>,
    skipped: Vec<Value>,
}
fn discover(repo: &Path, options: &BuildOptions, cancel: &AtomicBool) -> Result<Discovery> {
    let mut candidates = BTreeSet::new();
    match command(
        repo,
        "git",
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
        cancel,
    ) {
        Ok((0, raw, _)) => {
            for p in raw.split(|b| *b == 0).filter(|p| !p.is_empty()) {
                candidates.insert(String::from_utf8_lossy(p).replace('\\', "/"));
            }
        }
        _ => walk(repo, Path::new(""), &mut candidates, cancel)?,
    }
    let root =
        Root::open(repo).map_err(|e| Error::new(format!("cannot open source root: {e:?}")))?;
    let mut files = BTreeMap::new();
    let mut skipped = vec![];
    for path in candidates {
        crate::util::check(cancel)?;
        let name = path.rsplit('/').next().unwrap_or(&path);
        let mut reason = if excluded(&path) || !safe_path(&path) {
            "excluded"
        } else if [
            ".env", ".env.*", "*.pem", "*.key", "id_rsa*", "*.p12", "*.pfx",
        ]
        .iter()
        .any(|p| glob(p, name) || glob(p, &path))
        {
            "secret-name"
        } else if options.exclude.iter().any(|p| glob(p, &path))
            || (!options.include.is_empty() && !options.include.iter().any(|p| glob(p, &path)))
        {
            "excluded"
        } else {
            ""
        };
        if reason.is_empty() {
            if options.max_bytes < 0 {
                reason = "too-large";
            } else {
                match root.read(
                    &path,
                    options.max_bytes.min(64 * 1024 * 1024) as usize,
                    Policy::source(),
                ) {
                    Ok(data) => {
                        if data.iter().take(8192).any(|b| *b == 0) {
                            reason = "binary";
                        } else if language(&path).is_none() {
                            reason = "unparsed";
                        } else {
                            files.insert(path.clone(), data);
                        }
                    }
                    Err(oms_safe_fs::Error::Oversize) => reason = "too-large",
                    Err(_) => {
                        reason = if repo.join(&path).is_symlink() {
                            "symlink"
                        } else {
                            "excluded"
                        }
                    }
                }
            }
        }
        if !reason.is_empty() {
            skipped.push(json!({"path":path,"reason":reason}));
        }
    }
    Ok(Discovery { files, skipped })
}
fn coverage(found: &Discovery) -> Value {
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut extensions: BTreeMap<String, usize> = BTreeMap::new();
    for row in &found.skipped {
        *reasons.entry(s(row, "reason").into()).or_default() += 1;
        if s(row, "reason") == "unparsed" {
            let extension = Path::new(s(row, "path"))
                .extension()
                .map(|s| format!(".{}", s.to_string_lossy().to_lowercase()))
                .unwrap_or_else(|| "<none>".into());
            *extensions.entry(extension).or_default() += 1;
        }
    }
    json!({"parsed":found.files.len(),"unparsed":reasons.get("unparsed").copied().unwrap_or(0),"unparsed_by_extension":extensions,"skipped_by_reason":reasons})
}
struct Python<'a> {
    session: Option<Session>,
    helper: Option<PathBuf>,
    cancel: &'a AtomicBool,
}
impl Parser for Python<'_> {
    fn parse(
        &mut self,
        path: &str,
        text: &str,
        digest: &str,
    ) -> std::result::Result<oms_graph_python::ParseResult, oms_graph_python::Error> {
        if self.session.is_none() {
            let helper = self.helper.as_deref().ok_or_else(|| {
                oms_graph_python::Error::HelperUnavailable(
                    "packaged helper path was not supplied".into(),
                )
            })?;
            self.session = Some(Session::start(helper, Default::default())?);
        }
        let cancel = oms_graph_python::CancelFlag::new();
        let done = (std::sync::Mutex::new(false), std::sync::Condvar::new());
        struct Completion<'a>(&'a (std::sync::Mutex<bool>, std::sync::Condvar));
        impl Drop for Completion<'_> {
            fn drop(&mut self) {
                *self.0.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
                self.0.1.notify_all();
            }
        }
        std::thread::scope(|scope| {
            let monitor = scope.spawn(|| {
                let mut finished = done.0.lock().unwrap_or_else(|e| e.into_inner());
                while !*finished {
                    if self.cancel.load(Ordering::SeqCst) {
                        cancel.cancel();
                        break;
                    }
                    finished = done
                        .1
                        .wait_timeout(finished, std::time::Duration::from_millis(20))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            });
            let completion = Completion(&done);
            let result = self
                .session
                .as_mut()
                .unwrap()
                .parse_cancellable(path, text, digest, &cancel);
            drop(completion);
            let _ = monitor.join();
            result
        })
    }
}
fn helper(explicit: Option<&Path>) -> Result<Option<PathBuf>> {
    let path = explicit.map(Path::to_path_buf).or_else(|| {
        std::env::var_os("OMS_GRAPH_PYTHON_HELPER")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    });
    if let Some(ref path) = path {
        if !path.is_absolute() {
            return Err(Error::new("Python parser helper path must be absolute"));
        }
        let info = std::fs::symlink_metadata(path)?;
        use std::os::unix::fs::MetadataExt;
        if !info.is_file()
            || info.mode() & 0o022 != 0
            || info.mode() & 0o111 == 0
            || (info.uid() != 0 && info.uid() != rustix::process::geteuid().as_raw())
        {
            return Err(Error::new(
                "Python parser helper is not a trusted executable",
            ));
        }
    }
    Ok(path)
}
fn extraction(path: &str, data: &[u8], python: &mut impl Parser) -> Result<Value> {
    let (lang, version) =
        language(path).ok_or_else(|| Error::new(format!("no parser for {path}")))?;
    let digest = sha(data);
    let text = String::from_utf8_lossy(data);
    let mut parsed = if lang == "notebook" {
        notebook::parse(path, &text, &digest, python).map_err(|e| Error::new(e.to_string()))?
    } else {
        oms_graph::parse(path, &text, &digest, python)
            .map_err(|e| Error::new(format!("parser failed: {e:?}")))?
    };
    let kind = if test_path(path) { "test" } else { "file" };
    let id = format!("{kind}:{path}");
    if kind == "test" {
        for edge in &mut parsed.edges {
            if s(edge, "source") == format!("file:{path}") {
                edge["source"] = json!(id);
            }
        }
        for reference in &mut parsed.refs {
            if s(reference, "from") == format!("file:{path}") {
                reference["from"] = json!(id);
            }
        }
        if lang != "notebook" {
            let pattern =
                regex::Regex::new(r"[A-Za-z0-9_][A-Za-z0-9_.-]*(?:/[A-Za-z0-9_][A-Za-z0-9_.-]*)+")
                    .unwrap();
            for (line, text) in text.lines().enumerate() {
                for m in pattern.find_iter(text) {
                    if text[..m.start()]
                        .chars()
                        .next_back()
                        .is_none_or(|c| !c.is_ascii_alphanumeric() && !"_./$\\-".contains(c))
                    {
                        parsed.refs.push(json!({"from":id,"relation":"tests","kind":"path","value":m.as_str().trim_end_matches(['.',',',':',';','`','\'', '"']),"line":line+1}));
                    }
                }
            }
        }
    }
    parsed.nodes.insert(0,json!({"id":id,"kind":kind,"name":path.rsplit('/').next().unwrap_or(path),"path":path,"language":lang,"source_digest":digest,"summary":null,"crux":null,"metadata":{}}));
    Ok(
        json!({"path":path,"sha256":digest,"bytes":data.len(),"parser":lang,"parser_version":version,"nodes":parsed.nodes,"edges":parsed.edges,"refs":parsed.refs}),
    )
}
pub fn build(
    repo: &Path,
    state: &Path,
    options: &BuildOptions,
    cancel: &AtomicBool,
    explicit_helper: Option<&Path>,
) -> Result<Value> {
    let found = discover(repo, options, cancel)?;
    build_from(repo, state, options, cancel, explicit_helper, found)
}
fn build_from(
    repo: &Path,
    state: &Path,
    options: &BuildOptions,
    cancel: &AtomicBool,
    explicit_helper: Option<&Path>,
    found: Discovery,
) -> Result<Value> {
    let mut settings = oms_lock::Settings::from_env().map_err(|e| Error::new(e.to_string()))?;
    settings.cwd = repo.to_path_buf();
    let lock = oms_lock::Lock::for_file(&state.join("manifest.json"), settings)
        .map_err(|e| Error::new(e.to_string()))?;
    let guard = lock.try_acquire().map_err(|e| Error::new(e.to_string()))?;
    let mut python = Python {
        session: None,
        helper: helper(explicit_helper)?,
        cancel,
    };
    let mut files = BTreeMap::new();
    let mut extractions = vec![];
    let mut cached = 0;
    for (path, data) in &found.files {
        crate::util::check(cancel)?;
        let digest = sha(data);
        let (lang, version) = language(path).unwrap();
        let key = sha(format!("{path}\n{digest}\n{version}\n{SCHEMA}"));
        let cache_path = state.join("cache").join(format!("{key}.json"));
        let old = if options.force {
            None
        } else {
            read_json(&cache_path, 8 * 1024 * 1024)
                .ok()
                .flatten()
                .filter(|v| {
                    v["path"] == *path
                        && v["sha256"] == digest
                        && v["nodes"].is_array()
                        && v["edges"].is_array()
                        && v["refs"].is_array()
                })
        };
        let was_cached = old.is_some();
        let payload = match old {
            Some(v) => {
                cached += 1;
                v
            }
            None => {
                let v = extraction(path, data, &mut python)?;
                let raw = bytes(&v, false)?;
                if raw.len() <= 8 * 1024 * 1024 {
                    write(&cache_path, &raw)?;
                }
                v
            }
        };
        files.insert(
            path.clone(),
            json!({"sha256":digest,"bytes":data.len(),"parser":lang,"cached":was_cached}),
        );
        extractions.push(oms_graph_resolve::Extraction {
            path: path.clone(),
            sha256: digest,
            parser: lang.into(),
            nodes: array(&payload, "nodes").to_vec(),
            edges: array(&payload, "edges").to_vec(),
            refs: array(&payload, "refs").to_vec(),
        });
    }
    let (nodes, edges) =
        oms_graph_resolve::resolve(&extractions).map_err(|e| Error::new(e.to_string()))?;
    let revision_source = files
        .iter()
        .map(|(path, v)| format!("{path}\t{}", s(v, "sha256")))
        .collect::<Vec<_>>()
        .join("\n")
        + &format!("\n{VERSION}\n{SCHEMA}");
    let revision = sha(revision_source);
    let graph = json!({"schema":SCHEMA,"revision":revision,"nodes":nodes,"edges":edges});
    let mut include = options.include.clone();
    include.sort();
    let mut exclude = options.exclude.clone();
    exclude.sort();
    let manifest = json!({"schema":SCHEMA,"revision":revision,"generated_at":now(),"parser_version":VERSION,"discovery":{"include":include,"exclude":exclude,"max_bytes":options.max_bytes},"files":files,"skipped":found.skipped,"coverage":coverage(&found),"stats":{"files":files.len(),"nodes":nodes.len(),"edges":edges.len(),"cached":cached,"parsed":files.len()-cached}});
    guard.check().map_err(|e| Error::new(e.to_string()))?;
    if state.starts_with(repo.join(".oms")) {
        create_marker(repo)?;
    }
    write_json(&state.join("graph.json"), &graph)?;
    super::sqlite::write(state, &graph)?;
    write_json(&state.join("manifest.json"), &manifest)?;
    let index = super::index::Graph::new(graph)?;
    let brief = super::index::orientation(
        &index,
        &manifest,
        repo.file_name().and_then(|s| s.to_str()).unwrap_or(""),
    );
    write_json(
        &state.join("orientation.json"),
        &json!({"schema":1,"revision":revision,"generated_at":now(),"text":brief}),
    )?;
    Ok(manifest)
}
pub fn load(state: &Path) -> Result<Value> {
    read_json(&state.join("graph.json"), GRAPH_LIMIT)?
        .filter(Value::is_object)
        .ok_or_else(|| Error::new("project graph has not been built; run: oms graph project build"))
}
pub fn check(repo: &Path, state: &Path, cancel: &AtomicBool) -> Result<Value> {
    Ok(freshness(repo, state, cancel)?.0)
}
fn freshness(
    repo: &Path,
    state: &Path,
    cancel: &AtomicBool,
) -> Result<(Value, Option<Discovery>, Option<Value>)> {
    let Some(manifest) =
        read_json(&state.join("manifest.json"), GRAPH_LIMIT)?.filter(Value::is_object)
    else {
        return Ok((
            json!({"present":false,"fresh":false,"revision":"","stale":[],"missing":[],"new":[],"coverage":{"parsed":0,"unparsed":0,"unparsed_by_extension":{},"skipped_by_reason":{}}}),
            None,
            None,
        ));
    };
    let found = discover(repo, &BuildOptions::manifest(&manifest), cancel)?;
    let current: BTreeSet<&str> = found.files.keys().map(String::as_str).collect();
    let known: BTreeSet<&str> = manifest["files"]
        .as_object()
        .map(|v| v.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let stale: Vec<&str> = current
        .intersection(&known)
        .copied()
        .filter(|p| sha(&found.files[*p]) != s(&manifest["files"][*p], "sha256"))
        .collect();
    let missing: Vec<&str> = known.difference(&current).copied().collect();
    let new: Vec<&str> = current.difference(&known).copied().collect();
    let mut outdated = json!({});
    for (key, expected) in [("parser_version", VERSION), ("schema", SCHEMA)] {
        if manifest[key] != expected {
            outdated[key] = json!({"built":manifest[key],"current":expected});
        }
    }
    let valid = load(state).is_ok_and(|g| {
        g["schema"] == SCHEMA
            && g["revision"] == manifest["revision"]
            && !s(&g, "revision").is_empty()
            && g["nodes"].is_array()
            && g["edges"].is_array()
    });
    let status = json!({"present":true,"fresh":stale.is_empty()&&missing.is_empty()&&new.is_empty()&&outdated.as_object().unwrap().is_empty()&&valid,"revision":manifest["revision"],"stale":stale,"missing":missing,"new":new,"outdated":outdated,"coverage":coverage(&found)});
    Ok((status, Some(found), Some(manifest)))
}
pub fn ensure(
    repo: &Path,
    state: &Path,
    max_files: i64,
    cancel: &AtomicBool,
    helper: Option<&Path>,
) -> Result<Value> {
    let (status, found, manifest) = freshness(repo, state, cancel)?;
    if status["fresh"] == true {
        return Ok(json!({"action":"fresh","revision":status["revision"]}));
    }
    let present = status["present"] == true;
    let options = manifest
        .as_ref()
        .map(BuildOptions::manifest)
        .unwrap_or_default();
    let found = match found {
        Some(found) => found,
        None => discover(repo, &options, cancel)?,
    };
    if !present && found.files.len() as i128 > max_files as i128 {
        return Err(Error::new(format!(
            "project graph auto-build skipped: {} files exceed the {max_files}-file bound; run: oms graph project build",
            found.files.len()
        )));
    }
    let manifest = build_from(repo, state, &options, cancel, helper, found)?;
    Ok(
        json!({"action":if present {"refreshed"} else {"built"},"revision":manifest["revision"],"stats":manifest["stats"],"skipped":array(&manifest,"skipped").len()}),
    )
}
