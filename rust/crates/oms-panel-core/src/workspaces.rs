//! Managed workspace proofs and navigation through an explicitly owned tmux host.
use crate::terminals::{PrivateStore, SessionProof, session_proof, tmux};
use crate::{Error, Result, Services, Value, require};
use std::path::Path;
use std::sync::atomic::AtomicBool;
pub fn public_workspace(
    repo: &Path,
    row: &Value,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Value {
    let state = if workspace_proof(repo, row, service, stop).is_ok() {
        "running"
    } else if workspace_gone(repo, row, service, stop).unwrap_or(false) {
        "gone"
    } else {
        "unknown"
    };
    let archived = row["archived"] == true;
    serde_json::json!({
        "id": row["id"],
        "label": crate::eventlog::clean(row["label"].as_str().unwrap_or("Workspace"), 120),
        "kind": "workspace",
        "state": state,
        "archived": archived,
        "capabilities": {
            "focus": state == "running",
            "forget": state == "gone",
            "archive": state == "gone" && !archived,
            "restore": state == "gone" && archived,
        },
    })
}

pub fn list_workspaces(
    repo: &Path,
    archived: bool,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Vec<Value>> {
    crate::parent_only()?;
    let Some(store) = PrivateStore::open_optional(repo, "workspaces", archived)? else {
        return Ok(vec![]);
    };
    let mut rows = Vec::new();
    for id in store.identifiers()? {
        let row = store.record(&id)?;
        require(row["kind"] == "workspace", "invalid workspace record kind")?;
        rows.push(public_workspace(repo, &row, service, stop));
    }
    Ok(rows)
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
pub fn focus(repo: &Path, ident: &str, service: &dyn Services, stop: &AtomicBool) -> Result<Value> {
    crate::parent_only()?;
    let row = PrivateStore::open(repo, "workspaces", false, false)?.record(ident)?;
    let proof = workspace_proof(repo, &row, service, stop)?;
    let window = row["window"]
        .as_str()
        .ok_or_else(|| Error::new("workspace window unavailable"))?;
    let pane = row["pane"]
        .as_str()
        .ok_or_else(|| Error::new("workspace pane unavailable"))?;
    let commands =
        crate::terminals::switch_commands(service, repo, &proof.session, window, pane, stop)?;
    for cmd in &commands {
        workspace_proof(repo, &row, service, stop)?;
        tmux(service, repo, cmd, stop)?;
    }
    Ok(public_workspace(repo, &row, service, stop))
}

pub fn archive_workspace(
    repo: &Path,
    ident: &str,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::history::archive(repo, "workspaces", ident, |row| {
        workspace_gone(repo, row, service, stop)
    })
}

pub fn restore_workspace(
    repo: &Path,
    ident: &str,
    service: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    crate::history::restore(repo, "workspaces", ident, |row| {
        workspace_gone(repo, row, service, stop)
    })
}
