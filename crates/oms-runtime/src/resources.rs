//! Explicit resource-policy actions; the native producer owns all custody.
use crate::parser::Request;
use crate::services::ResourceAction;
use crate::storage::Runtime;
use crate::{Result, error, require};

pub fn action(rt: &Runtime<'_>, request: &Request) -> Result<crate::value::Value> {
    if !matches!(request.action.as_str(), "host" | "status") {
        rt.parent_write()?;
        require(
            std::env::var_os("OMS_HARNESS_DELEGATE_DEPTH")
                .is_none_or(|value| value.is_empty() || value == "0"),
            "a delegated descendant cannot mutate resource policy",
        )?;
    }
    let action = match request.action.as_str() {
        "host" => ResourceAction::Host,
        "status" => ResourceAction::Status,
        "activate" | "reconcile" => {
            let selected = request
                .os("policy")
                .ok_or_else(|| error("--policy is required"))?;
            require(!selected.is_empty(), "--policy must name an explicit file")?;
            let path = rt.expand_user(selected)?;
            let absolute = if path.is_absolute() {
                path
            } else {
                std::fs::canonicalize(&rt.cwd).map_err(error)?.join(path)
            };
            let policy = rt.user_path(absolute.as_os_str())?;
            if request.action == "reconcile" {
                ResourceAction::Reconcile {
                    policy,
                    expected_snapshot_sha256: digest(request, "expected-snapshot-sha256")?,
                    reason: reason(request)?,
                }
            } else {
                ResourceAction::Activate { policy }
            }
        }
        "hold" => ResourceAction::Hold {
            expected_policy_sha256: digest(request, "expected-policy-sha256")?,
        },
        "retire-team" => ResourceAction::RetireTeam {
            team_id: identifier(request, "team-id")?,
            spec_sha256: digest(request, "team-spec-sha256")?,
            expected_revision: revision(request)?,
        },
        "discard-write" => ResourceAction::DiscardWrite {
            operation_id: identifier(request, "operation-id")?,
            expected_revision: revision(request)?,
            reason: reason(request)?,
        },
        _ => return Err(error("unsupported resource action")),
    };
    rt.services
        .resource_action(&rt.repo, &rt.state_repo, &action)
}

fn digest(request: &Request, key: &str) -> Result<String> {
    let value = request.text(key, "")?;
    require(
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        format!("--{key} must be a lowercase SHA-256 digest"),
    )?;
    Ok(value.into())
}
fn identifier(request: &Request, key: &str) -> Result<String> {
    let value = request.text(key, "")?;
    require(
        !value.is_empty()
            && value.len() <= 160
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b)),
        format!("--{key} must be an exact resource identifier"),
    )?;
    Ok(value.into())
}
fn revision(request: &Request) -> Result<u64> {
    request
        .text("expected-revision", "")?
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0 && *n < u64::MAX)
        .ok_or_else(|| error("--expected-revision must be a positive resource revision"))
}
fn reason(request: &Request) -> Result<String> {
    let value = request.text("reason", "")?;
    require(
        !value.trim().is_empty()
            && value.chars().count() <= 300
            && !value.chars().any(char::is_control),
        "--reason must contain 1..300 characters without control bytes",
    )?;
    Ok(value.into())
}
