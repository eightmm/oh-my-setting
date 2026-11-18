//! Channel semantics; publication remains owned by the transactional native producer.
use crate::services::{ExternalOutput, PromotionRequest, ReleaseRequest};
use crate::storage::Runtime;
use crate::value::*;
use crate::{Result, require, status as failure};
pub fn manifest(rt: &Runtime<'_>) -> Result<Value> {
    let store = oms_command_plan::storage::Store::open(&rt.install_root)?
        .with_group_resolver(rt.services.group_resolver());
    let Some(raw) = store.optional("config/update-channels.json", crate::storage::MAX_STATE)? else {
        // A missing manifest has no reviewed stable promotion. Only this
        // synthetic default may be unconfigured; persisted manifests still
        // require both channel refs before they can authorize an update.
        return parse(
            br#"{
                "schema": 1,
                "channels": {
                    "stable": {
                        "version": null,
                        "ref": null,
                        "auto_apply": false,
                        "policy": "unconfigured; requires explicit stable promotion"
                    },
                    "edge": {
                        "version": "edge",
                        "ref": "origin/HEAD",
                        "auto_apply": false,
                        "policy": "follow the origin default branch"
                    }
                }
            }"#,
        );
    };
    let raw = parse(&raw)?;
    require(
        raw.get("schema").is_int(1) && raw.get("channels").object().is_ok(),
        "invalid update channel manifest",
    )?;
    for name in ["stable", "edge"] {
        require(
            !raw.get("channels").get(name).text("ref").is_empty(),
            format!("update channel {name} has no ref"),
        )?;
    }
    Ok(raw)
}
pub fn resolve(rt: &Runtime<'_>, channel: &str, fetch: bool) -> Result<Value> {
    require(
        ["stable", "edge"].contains(&channel),
        "channel must be stable or edge",
    )?;
    let m = manifest(rt)?;
    if fetch && !m.get("channels").get(channel).text("ref").is_empty() {
        rt.parent_write()?;
        rt.services.fetch_release(&rt.install_root)?;
    }
    let current = rt.services.resolve_revision(&rt.install_root, "HEAD")?;
    resolve_with(rt, channel, &m, current.as_deref())
}
fn resolve_with(
    rt: &Runtime<'_>,
    channel: &str,
    m: &Value,
    current: Option<&str>,
) -> Result<Value> {
    let row = m.get("channels").get(channel);
    let reference = row.text("ref");
    let mut candidates = Vec::new();
    if !reference.is_empty() {
        if channel == "edge" && !reference.starts_with("origin/") {
            candidates.push(format!("origin/{reference}"));
        }
        candidates.push(reference.to_owned());
    }
    let (mut resolved, mut selected) = (String::new(), String::new());
    for candidate in candidates {
        if let Some(commit) = rt.services.resolve_revision(&rt.install_root, &candidate)? {
            require(
                hex(&commit, 40) || hex(&commit, 64),
                "invalid resolved channel object id",
            )?;
            resolved = commit;
            selected = candidate;
            break;
        }
    }
    Ok(object([
        ("schema", number(1)),
        ("channel", string(channel)),
        ("configured_ref", optional(reference)),
        ("selected_ref", optional(&selected)),
        ("resolved_commit", optional(&resolved)),
        ("ready", b(!resolved.is_empty())),
        ("current_commit", current.map(string).unwrap_or(Value::Null)),
        ("version", row.get("version").clone()),
        ("policy", row.get("policy").clone()),
        ("auto_apply", b(row.get("auto_apply").truth())),
        ("manifest_digest", string(hash(m)?)),
        (
            "apply_command",
            if resolved.is_empty() {
                Value::Null
            } else if channel == "edge" {
                string("oms runtime release apply edge")
            } else {
                string(format!("oms update --ref {resolved}"))
            },
        ),
    ]))
}
pub fn status(rt: &Runtime<'_>) -> Result<Value> {
    let m = manifest(rt)?;
    let current = rt.services.resolve_revision(&rt.install_root, "HEAD")?;
    Ok(object([
        ("schema", number(1)),
        ("manifest", string("config/update-channels.json")),
        (
            "stable",
            resolve_with(rt, "stable", &m, current.as_deref())?,
        ),
        ("edge", resolve_with(rt, "edge", &m, current.as_deref())?),
    ]))
}
pub fn apply(
    rt: &Runtime<'_>,
    channel: &str,
    fetch: bool,
    no_tools: bool,
) -> Result<(Value, ExternalOutput)> {
    rt.parent_write()?;
    let target = resolve(rt, channel, fetch)?;
    if !target.get("ready").truth() {
        return Err(failure(
            3,
            format!("update channel is not resolvable: {channel}"),
        ));
    }
    let result = rt.services.apply_release(
        &rt.install_root,
        &ReleaseRequest {
            channel: channel.into(),
            target: target.text("resolved_commit").into(),
            selected_ref: target.text("selected_ref").into(),
            no_tools,
        },
    )?;
    let receipt = object([
        ("schema", number(1)),
        ("channel", string(channel)),
        ("target", target),
        ("exit", number(result.exit)),
        ("applied", b(result.exit == 0)),
        // Wave 4 will migrate this legacy compatibility label.
        ("transaction", string("scripts/update.sh")),
    ]);
    Ok((receipt, result))
}
pub fn promote(rt: &Runtime<'_>, commit: &str, version: &str, expected: &str) -> Result<Value> {
    rt.parent_write()?;
    require(
        (40..=64).contains(&commit.len())
            && commit
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "stable commit must be a full lowercase Git object id",
    )?;
    require(
        regex::Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)?$")
            .unwrap()
            .is_match(version),
        "stable version must be semantic-version shaped",
    )?;
    require(
        hex(expected, 64),
        "stable promotion requires the exact reviewed manifest digest",
    )?;
    let m = manifest(rt)?;
    if hash(&m)? != expected {
        return Err(failure(75, "update channel manifest changed since review"));
    }
    rt.services.promote_release(
        &rt.install_root,
        &PromotionRequest {
            commit: commit.into(),
            version: version.into(),
            expected_manifest_digest: expected.into(),
            manifest: m,
        },
    )
}
