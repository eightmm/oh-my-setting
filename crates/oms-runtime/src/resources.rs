//! Explicit resource-policy actions; the native producer owns all custody.
use crate::parser::Request;
use crate::services::ResourceAction;
use crate::storage::Runtime;
use crate::{Result, error, require};

pub fn action(rt: &Runtime<'_>, request: &Request) -> Result<crate::value::Value> {
    if matches!(request.action.as_str(), "activate" | "hold") {
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
        "activate" => {
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
            ResourceAction::Activate {
                policy: rt.user_path(absolute.as_os_str())?,
            }
        }
        "hold" => {
            let digest = request.text("expected-policy-sha256", "")?;
            require(
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "--expected-policy-sha256 must be a lowercase SHA-256 digest",
            )?;
            ResourceAction::Hold {
                expected_policy_sha256: digest.into(),
            }
        }
        _ => return Err(error("unsupported resource action")),
    };
    rt.services
        .resource_action(&rt.repo, &rt.state_repo, &action)
}
