//! Write-free lifecycle previews always come from the owning native producer.
use crate::cli::Args;
use crate::planning::invoke;
use crate::storage::Context;
use crate::{Output, Result, require, value};
use std::ffi::OsString;
use std::time::Duration;

fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.into())
}
fn on_path(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            std::fs::metadata(dir.join(name))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}
#[derive(Default)]
struct Preview {
    lines: Vec<String>,
    changes: usize,
    current: usize,
    unseen: usize,
}
impl Preview {
    fn emit(&mut self, step: &str, status: &str, detail: &str) {
        match status {
            "change" => self.changes += 1,
            "current" => self.current += 1,
            "not-previewed" => self.unseen += 1,
            _ => {}
        }
        self.lines.push(format!("{step}: {status}: {detail}"));
    }
    fn producer(&mut self, ctx: &Context<'_>, step: &str, tool: &str, args: Vec<OsString>) {
        match invoke(ctx, tool, args, Duration::from_secs(60)) {
            Ok(out) if out.code == 0 => {
                for line in String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter(|line| !line.is_empty())
                {
                    if let Some(detail) = line.strip_prefix("would ") {
                        self.emit(step, "change", detail);
                    } else if let Some(detail) = line.strip_prefix("not previewed: ") {
                        self.emit(step, "not-previewed", detail);
                    } else if line.contains(" current: ") || line.starts_with("kept: ") {
                        self.emit(step, "current", line);
                    } else {
                        self.emit(step, "note", line);
                    }
                }
            }
            Ok(out) => self.emit(
                step,
                "not-previewed",
                &format!("its preview exited {}", out.code),
            ),
            Err(_) => self.emit(
                step,
                "not-previewed",
                "native producer preview service unavailable",
            ),
        }
    }
}
pub(crate) fn execute(ctx: &Context<'_>, args: &Args) -> Result<Output> {
    require(
        args.flag("install") != args.flag("update"),
        "install-plan needs exactly one of --install or --update",
    )?;
    let update = args.flag("update");
    let mut plan = Preview::default();
    plan.lines.push(format!(
        "{} plan: nothing is written by this command",
        if update { "update" } else { "install" }
    ));
    plan.emit("checkout", "not-previewed", "fetching the configured ref needs the network and writes the checkout; steps below describe the current checkout");
    plan.emit(
        "python-runtime",
        "not-previewed",
        "runtime ensure may download the locked interpreter and precompile the checkout",
    );
    if update {
        plan.emit(
            "tools",
            if env("OMS_PLAN_TOOLS", "providers") == "skip" {
                "skip"
            } else {
                "not-previewed"
            },
            if env("OMS_PLAN_TOOLS", "providers") == "skip" {
                "tool refresh is off (--no-tools)"
            } else {
                "provider and tool refresh runs each tool's own installer"
            },
        );
    } else if env("OMS_PLAN_TOOLS_FULL", "0") == "1" {
        plan.emit(
            "tools",
            "not-previewed",
            "--full installs the whole tool footprint and may add a PATH line",
        );
    } else {
        let mut args = vec![
            "--plan".into(),
            "--primary-provider".into(),
            env("OMS_PLAN_PRIMARY_PROVIDER", "auto").into(),
        ];
        for profile in env("OMS_PLAN_CAPABILITIES", "core").split(',') {
            args.extend(["--profile".into(), profile.into()]);
        }
        match invoke(ctx, "install-profile", args, Duration::from_secs(60)) {
            Ok(out) if out.code == 0 => {
                match value::parse(&out.stdout) {
                    Ok(row) => {
                        plan.emit(
                            "tools",
                            "note",
                            &format!(
                                "profiles {}; primary provider {}; managed tools: {}",
                                row["expanded"],
                                value::text(&row, "primary_provider"),
                                row["managed_tools"]
                            ),
                        );
                        plan.emit("tools", "not-previewed", "whether each tool is installed, and PATH changes made by its installer");
                    }
                    Err(_) => plan.emit(
                        "tools",
                        "not-previewed",
                        "install-profile printed an unreadable plan",
                    ),
                }
            }
            _ => plan.emit(
                "tools",
                "not-previewed",
                "install-profile native plan service unavailable",
            ),
        }
    }
    plan.producer(ctx, "links", "link", vec!["--plan".into()]);
    if !update {
        plan.emit(
            "services",
            if env("OH_MY_SETTING_CONNECT_SERVICES", "0") == "0" {
                "skip"
            } else {
                "not-previewed"
            },
            "service login is not executed by the plan",
        );
    }
    if env("OH_MY_SETTING_CLAUDE_HOOKS", "1") == "1" {
        plan.producer(
            ctx,
            "claude-hooks",
            "install-claude-hooks",
            vec!["--print-expected".into()],
        );
        plan.emit("claude-hooks", "not-previewed", "settings merge, timeout convergence and removal of retired rows belong to the installer");
    } else {
        plan.emit(
            "claude-hooks",
            if update { "not-previewed" } else { "skip" },
            if update {
                "hooks are off; update removes only OMS hook rows"
            } else {
                "Claude hooks are off"
            },
        );
    }
    let codex = env("OH_MY_SETTING_CODEX_PLUGIN", "auto");
    let codex = codex == "1" || codex == "auto" && on_path("codex");
    plan.emit(
        "codex-plugin",
        if codex { "not-previewed" } else { "skip" },
        "plugin registration and managed Codex configuration require the provider CLI",
    );
    plan.emit(
        "mcp",
        if env("OH_MY_SETTING_MCP", "1") == "0" {
            "skip"
        } else {
            "not-previewed"
        },
        "MCP registration through provider CLIs is not started by this plan",
    );
    let agy = env("OH_MY_SETTING_AGY_PLUGIN", "auto");
    let agy = agy == "1" || agy != "0" && on_path("agy");
    plan.emit(
        "agy-plugin",
        if agy { "not-previewed" } else { "skip" },
        "Antigravity plugin import requires its provider CLI",
    );
    if env("OH_MY_SETTING_PEER_PERMISSIONS", "1") == "1" && on_path("agy") {
        plan.producer(
            ctx,
            "permissions",
            "provider-permissions",
            vec![
                "--print".into(),
                "--profile".into(),
                "delegate".into(),
                "--allow-toolchains".into(),
            ],
        );
    } else {
        plan.emit(
            "permissions",
            "skip",
            "permission grants are off or agy is not on PATH",
        );
    }
    let slurm = env("OH_MY_SETTING_GENERATE_SLURM", "0");
    let slurm = slurm == "1" || slurm == "auto" && on_path("sinfo");
    plan.emit(
        "slurm-snapshot",
        if slurm { "change" } else { "skip" },
        "write the private Slurm reference when enabled",
    );
    plan.emit(
        "machine-snapshot",
        if env("OH_MY_SETTING_GENERATE_MACHINE", "auto") == "0" {
            "skip"
        } else {
            "change"
        },
        "rewrite the private machine snapshot when enabled",
    );
    plan.emit(
        "doctor",
        "note",
        "doctor runs after the steps above and changes nothing",
    );
    if env("OH_MY_SETTING_AUTO_UPDATE", "1") == "1" {
        let mut args = vec!["--dry-run".into()];
        if env("OH_MY_SETTING_AUTO_UPDATE_MODE", "") == "check" {
            args.push("--check-only".into());
        }
        plan.producer(ctx, "auto-update", "install-autoupdate", args);
    } else {
        plan.emit(
            "auto-update",
            if update { "not-previewed" } else { "skip" },
            "auto-update is off; update may remove an OMS timer or cron block",
        );
    }
    plan.lines.push(format!("plan: {} change(s), {} already current, {} step part(s) not previewed; nothing was written", plan.changes, plan.current, plan.unseen));
    Ok(Output::text(plan.lines.join("\n")))
}
