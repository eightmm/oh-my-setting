//! Compile once from the disposable snapshot; bind only bytes actually appended.
use crate::{Context, Error, Result, V, native, safety, storage, text};
use oms_context::attribution::{LAYERS, Span};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

pub(crate) struct Compiled {
    pub bundle: String,
    pub spans: Vec<Span>,
    pub manifest_sha256: String,
    pub policy_sha256: String,
    pub bundle_sha256: String,
    pub debt: usize,
    pub missing: Vec<String>,
}
fn invalid() -> Error {
    Error::new(2, "compiled context does not match its manifest")
}
fn count(value: &V) -> Result<usize> {
    match value {
        V::Num(n) => n.parse().map_err(|_| invalid()),
        _ => Err(invalid()),
    }
}
impl Compiled {
    pub fn build(
        repo: &Path,
        snapshot: &Path,
        targets: &[OsString],
        context: &Context<'_>,
    ) -> Result<Self> {
        let source = storage::root(snapshot)?;
        let mut existing = Vec::new();
        for target in targets {
            crate::check(context.interrupted)?;
            match source.read_no_follow(
                Path::new(target),
                oms_safe_fs::ByteReadOptions {
                    missing_ok: false,
                    max_bytes: oms_runtime::storage::MAX_STATE,
                },
            ) {
                Ok(_) => existing.push(target.clone()),
                Err(oms_safe_fs::ByteError::Missing | oms_safe_fs::ByteError::Unsafe) => {}
                Err(_) => {
                    return Err(Error::new(
                        2,
                        "context target is unreadable, changed or oversized",
                    ));
                }
            }
        }
        let name = crate::id("delegate-")?;
        let bundle_path = PathBuf::from(format!(".oms/runtime/context/{name}.txt"));
        let manifest_path = PathBuf::from(format!(".oms/runtime/context/{name}.json"));
        let services = native::Services(context);
        // Snapshot state is deliberately local: parent --no-task/--no-memory
        // selections must not be silently reintroduced through shared state.
        let runtime = oms_runtime::storage::Runtime::open(
            snapshot,
            snapshot,
            context.source,
            &services,
            context.interrupted,
        )
        .map_err(|e| Error::new(e.code, e.message))?;
        let mut produced = oms_runtime::context::plan(
            &runtime,
            &oms_runtime::context::ContextRequest {
                targets: existing,
                explicit: vec![],
                required: vec![],
                max_bytes: 65536,
                bundle: Some(repo.join(&bundle_path)),
                manifest: Some(repo.join(&manifest_path)),
                allow_external: false,
                phase: "implementation".into(),
            },
        )
        .map_err(|e| Error::new(e.code, e.message))?;
        produced.remove("manifest_path");
        let mut expected = oms_runtime::value::sorted_pretty(&produced).map_err(|_| invalid())?;
        expected.push(b'\n');
        let root = storage::root(repo)?;
        let bounded = oms_safe_fs::ByteReadOptions {
            missing_ok: false,
            max_bytes: oms_runtime::storage::MAX_STATE,
        };
        let raw = root
            .read_no_follow(&manifest_path, bounded)
            .map_err(|_| invalid())?;
        if raw != expected {
            return Err(invalid());
        }
        oms_context::inspect_manifest(&raw, 100).map_err(|_| invalid())?;
        let manifest = V::parse(&raw)?;
        let bundle = root
            .read_no_follow(&bundle_path, bounded)
            .map_err(|_| invalid())?;
        safety::outbound_prompt(&bundle)?;
        let bundle_sha256 = storage::hash(&bundle);
        let attribution = manifest.get("request_attribution");
        let policy_sha256 = text(&manifest, "phase_rules_sha256").to_owned();
        let debt = count(manifest.get("context_debt"))?;
        if bundle.len() != count(manifest.get("selected_bytes"))?
            || bundle.len() > 65536
            || text(&manifest, "bundle_sha256") != bundle_sha256
            || text(attribution, "request_sha256") != bundle_sha256
            || text(attribution, "phase_policy_sha256") != policy_sha256
            || manifest.get("sufficient") != &V::Bool(debt == 0)
        {
            return Err(invalid());
        }
        let mut spans = Vec::new();
        let mut prefix = Sha256::new();
        let mut cursor = 0;
        for row in attribution.get("spans").array() {
            let start = count(row.get("start"))?;
            let end = count(row.get("end"))?;
            if start < cursor || end <= start || end > bundle.len() {
                return Err(invalid());
            }
            prefix.update(&bundle[cursor..end]);
            cursor = end;
            if text(row, "sha256") != storage::hash(&bundle[start..end])
                || text(row, "prefix_sha256") != format!("{:x}", prefix.clone().finalize())
            {
                return Err(invalid());
            }
            let layer = *LAYERS
                .iter()
                .find(|layer| **layer == text(row, "layer"))
                .ok_or_else(invalid)?;
            let source_range = if matches!(row.get("source_range"), V::Null) {
                None
            } else {
                let values = row.get("source_range").array();
                Some((count(&values[0])?, count(&values[1])?))
            };
            spans.push(Span {
                layer,
                start,
                end,
                sha256: text(row, "sha256").into(),
                prefix_sha256: text(row, "prefix_sha256").into(),
                source_sha256: row.get("source_sha256").text().map(str::to_owned),
                source_bytes: if matches!(row.get("source_bytes"), V::Null) {
                    None
                } else {
                    Some(count(row.get("source_bytes"))?)
                },
                source_range,
            });
        }
        Ok(Self {
            bundle: String::from_utf8(bundle).map_err(|_| invalid())?,
            spans,
            manifest_sha256: storage::hash(&raw),
            policy_sha256,
            bundle_sha256,
            debt,
            missing: manifest
                .get("missing_required")
                .array()
                .iter()
                .map(|v| v.text().unwrap_or_default().to_owned())
                .collect(),
        })
    }
    pub fn index(&self, row: &mut V) {
        row.set("context_manifest_digest", self.manifest_sha256.clone());
        row.set("context_bundle_sha256", self.bundle_sha256.clone());
        row.set("context_selected_bytes", self.bundle.len());
        row.set("context_debt", self.debt);
    }
}
