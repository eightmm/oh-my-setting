//! Managed workspace proofs and navigation through an explicitly owned tmux host.
use crate::terminals::{PrivateStore, SessionProof, session_proof, tmux};
use crate::{Error, Result, Services, Value, require};
use std::path::Path;
use std::sync::atomic::AtomicBool;
pub fn list_workspaces(repo: &Path, archived: bool) -> Result<Vec<Value>> {
    PrivateStore::open(repo, "workspaces", archived, false)?.list()
}
pub fn workspace_gone(
    repo: &Path,
    row: &Value,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<bool> {
    require(
        row["repo"]
            == std::fs::canonicalize(repo)
                .map_err(Error::new)?
                .to_string_lossy()
                .as_ref()
            && row["kind"] == "workspace"
            && crate::terminals::target_id(&row["session"], '$'),
        "invalid workspace binding",
    )?;
    let origin: SessionProof =
        serde_json::from_value(row["origin_proof"].clone()).map_err(Error::new)?;
    if !crate::terminals::server_present(repo, &origin, service, stop)? {
        return Ok(true);
    }
    require(
        session_proof(service, repo, &origin.session, stop)? == origin,
        "workspace origin ownership changed",
    )?;
    let mut request = crate::services::ExecutionRequest::new(
        crate::services::args(&[
            "tmux",
            "has-session",
            "-t",
            row["session"].as_str().unwrap(),
        ]),
        repo,
    );
    request.timeout = std::time::Duration::from_secs(3);
    let output = service.execute(&request, stop)?;
    require(
        output.cleanup_complete && output.output_complete && !output.limited,
        "workspace absence probe incomplete",
    )?;
    Ok(output.code != 0 && String::from_utf8_lossy(&output.stderr).contains("can't find"))
}
pub fn forget_workspace(
    repo: &Path,
    ident: &str,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::history::forget_record(
        repo,
        "workspaces",
        ident,
        |row| workspace_gone(repo, row, service, stop),
        &["record"],
    )
}
pub fn workspace_proof(
    repo: &Path,
    row: &Value,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<SessionProof> {
    require(
        row["repo"]
            == std::fs::canonicalize(repo)
                .map_err(Error::new)?
                .to_string_lossy()
                .as_ref()
            && row["kind"] == "workspace",
        "workspace binding changed",
    )?;
    let expected: SessionProof =
        serde_json::from_value(row["session_proof"].clone()).map_err(Error::new)?;
    let current = session_proof(service, repo, &expected.session, stop)?;
    require(current == expected, "workspace host identity changed")?;
    Ok(current)
}
pub fn open_workspace(
    repo: &Path,
    origin: &str,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::parent_only()?;
    let original = session_proof(service, repo, origin, stop)?;
    let store = PrivateStore::open(repo, "workspaces", false, true)?;
    if let Some(row) = store
        .list()?
        .into_iter()
        .find(|v| v["origin"] == origin && workspace_proof(repo, v, service, stop).is_ok())
    {
        return Ok(row);
    }
    let row = store.allocate("term","workspace",&serde_json::json!({"origin":origin,"origin_proof":original,"state":"starting","label":"Workspace"}))?;
    let ident = row["id"].as_str().unwrap();
    let name = format!("oms-workspace-{}", &ident[5..17]);
    let mut argv = crate::services::args(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{session_id}\t#{window_id}\t#{pane_id}",
        "-s",
        &name,
        "-n",
        "Controls",
        "-c",
    ]);
    argv.push(repo.into());
    let created = tmux(service, repo, &argv, stop)?;
    let fields: Vec<_> = created.split('\t').collect();
    require(
        fields.len() == 3,
        "workspace launch unconfirmed; preserve starting record",
    )?;
    tmux(
        service,
        repo,
        &[
            "set-option".into(),
            "-t".into(),
            fields[0].into(),
            "@oms_panel_repo".into(),
            repo.into(),
        ],
        stop,
    )?;
    let proof = session_proof(service, repo, fields[0], stop)?;
    let mut changed = row.clone();
    changed["session_proof"] = serde_json::to_value(proof).map_err(Error::new)?;
    changed["session"] = fields[0].into();
    changed["window"] = fields[1].into();
    changed["pane"] = fields[2].into();
    changed["state"] = "running".into();
    store.update(ident, &row, &changed)?;
    Ok(changed)
}
pub fn focus(repo: &Path, ident: &str, service: &dyn Services, stop: &AtomicBool) -> Result<()> {
    crate::parent_only()?;
    let row = PrivateStore::open(repo, "workspaces", false, false)?.record(ident)?;
    let proof = workspace_proof(repo, &row, service, stop)?;
    tmux(
        service,
        repo,
        &crate::services::args(&["switch-client", "-t", &proof.session]),
        stop,
    )?;
    Ok(())
}
