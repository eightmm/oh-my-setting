use crate::{
    receipt::Paths,
    runtime::{self as r, Result},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

#[derive(Clone, Default)]
pub(crate) struct Options {
    pub tool: String,
    pub action: String,
    pub flags: BTreeSet<String>,
    values: BTreeMap<String, Vec<String>>,
    paths: BTreeMap<String, Vec<OsString>>,
    pub raw: Vec<OsString>,
    pub claude_capabilities: Option<(bool, bool)>,
    pub hook_protected: bool,
}
impl Options {
    pub fn has(&self, k: &str) -> bool {
        self.flags.contains(k) || self.values.contains_key(k) || self.paths.contains_key(k)
    }
    pub fn get(&self, k: &str) -> Option<&str> {
        self.values
            .get(k)
            .and_then(|v| v.last())
            .map(String::as_str)
    }
    pub fn all(&self, k: &str) -> &[String] {
        self.values.get(k).map(Vec::as_slice).unwrap_or(&[])
    }
    pub fn path(&self, k: &str, cwd: &Path) -> Result<Option<PathBuf>> {
        self.paths
            .get(k)
            .and_then(|a| a.last())
            .map(|s| s.as_os_str())
            .or_else(|| self.get(k).map(OsStr::new))
            .map(|s| {
                r::leaf(&if Path::new(s).is_absolute() {
                    PathBuf::from(s)
                } else {
                    cwd.join(s)
                })
            })
            .transpose()
    }
    pub fn all_paths(&self, k: &str) -> &[OsString] {
        self.paths.get(k).map(Vec::as_slice).unwrap_or(&[])
    }
    pub fn preview(&self) -> bool {
        if std::env::var_os("OH_MY_SETTING_DRY_RUN").is_some_and(|s| s == "1") {
            return true;
        }
        self.raw
            .iter()
            .rev()
            .find_map(|s| {
                if s == "--plan" || s == "--dry-run" {
                    Some(true)
                } else if s == "--apply" {
                    Some(false)
                } else {
                    None
                }
            })
            .unwrap_or(false)
    }
}
fn spec(tool: &str) -> (&'static [&'static str], &'static [&'static str]) {
    match tool {
        "doctor" => (
            &[
                "--json",
                "--remediation-plan",
                "--repair",
                "--surfaces",
                "--integrations",
                "--tool-lock",
                "--contract",
                "--security",
                "--residue",
                "--sandbox",
                "--probe",
                "--live-models",
                "--strict-diversity",
                "--no-model-doctor",
                "--extensions",
                "--mcp",
                "--pin",
                "--providers",
                "--check",
            ],
            &["--repo", "--probe-tools", "--table", "--root"],
        ),
        "install" => (
            &[
                "--full",
                "--tools",
                "--connect-services",
                "--no-connect-services",
                "--auto-update",
                "--no-auto-update",
                "--machine-snapshot",
                "--slurm-snapshot",
                "--peer-permissions",
                "--star",
                "--no-star",
                "--plan",
            ],
            &[
                "--ref",
                "--profile",
                "--primary-provider",
                "--notion-data-source",
            ],
        ),
        "link" => (&["--plan"], &[]),
        "unlink" => (&["--dry-run"], &[]),
        "uninstall" => (
            &["--yes", "-y", "--purge", "--purge-dirty", "--dry-run"],
            &[],
        ),
        "install-profile" | "install-tools" => (
            &[
                "--plan",
                "--check",
                "--apply",
                "--reapply",
                "--add",
                "--upgrade",
                "--providers",
                "--allow-missing",
                "--full",
                "--dry-run",
            ],
            &["--profile", "--primary-provider", "--receipt", "--only"],
        ),
        "install-hooks" => (
            &["--quick", "--check-protection", "--remove", "--dry-run"],
            &["--repo"],
        ),
        "install-claude-hooks" => (
            &["--remove", "--print-expected", "--dry-run"],
            &["--settings", "--ownership-receipt"],
        ),
        "install-mcp" | "install-codex-plugin" => (&["--remove", "--dry-run"], &[]),
        "install-agy-plugin" => (&["--remove", "--probe-surfaces", "--dry-run"], &[]),
        "install-skills" => (&[], &[]),
        "install-autoupdate" => (&["--apply", "--check-only", "--dry-run"], &["--method"]),
        "uninstall-autoupdate" => (&["--dry-run"], &[]),
        "update" => (
            &[
                "--check",
                "--plan",
                "--prepare",
                "--apply-prepared",
                "--status",
                "--rollback",
                "--fallback-to-edge",
                "--tools",
                "--no-tools",
                "--no-doctor",
                "--probe-agy-surfaces",
                "--json",
            ],
            &["--ref", "--from", "--sha256"],
        ),
        "agy-hook" => (&[], &[]),
        "auto-update" => (
            &["--dry-run", "--apply", "--check-only", "--json"],
            &["--method"],
        ),
        "status" => (
            &["--verbose", "--json", "--fingerprint"],
            &["--repo", "--compare"],
        ),
        "snapshot" | "write-machine-snapshot" => (&["--check", "--dry-run"], &[]),
        "tick" => (
            &[
                "--dry-run",
                "--json",
                "--pause",
                "--resume",
                "--cancel",
                "--list",
            ],
            &["--repo", "--method", "--contract", "--every", "--until"],
        ),
        _ => (&[], &[]),
    }
}
fn usage(tool: &str) -> String {
    let (flags, values) = spec(tool);
    let actions = match tool {
        "auto-update" => " [check|apply|status|attention|install|remove]",
        "tick" => " [run|register|unregister|install|uninstall|status|schedule|refresh-catalogs]",
        _ => "",
    };
    let mut help = format!(
        "Usage: oms {tool}{actions} [options]\n\n{}\n  -h, --help\n\nNative lifecycle adapter. Unsupported operations return a nonzero result.\n",
        flags
            .iter()
            .map(|s| format!("  {s}"))
            .chain(values.iter().map(|s| format!("  {s} VALUE")))
            .collect::<Vec<_>>()
            .join("\n")
    );
    if tool == "update" {
        help.push_str(
            "\n--prepare builds or imports a payload without changing the active installation.\n\
             --apply-prepared applies the saved exact target without fetching a newer ref.\n\
             --status reports preparation and installation state without changing it.\n\
             --from ARCHIVE --sha256 DIGEST imports an explicitly trusted local bundle;\n\
             both options require --prepare. The digest is not a publisher signature.\n",
        );
    }
    help
}
pub(crate) fn parse(tool: &str, args: &[OsString]) -> Result<Options> {
    let (flags, values) = spec(tool);
    let mut out = Options {
        tool: tool.into(),
        raw: args.to_vec(),
        ..Default::default()
    };
    let mut args = args.iter();
    while let Some(raw) = args.next() {
        let bytes = raw.as_bytes();
        let split = bytes.iter().position(|b| *b == b'=');
        let (name, attached) = match split {
            Some(i) => (
                &bytes[..i],
                Some(OsString::from_vec(bytes[i + 1..].to_vec())),
            ),
            None => (bytes, None),
        };
        let s = std::str::from_utf8(name)
            .map_err(|_| r::Error(2, "option names and actions must be UTF-8".into()))?;
        if matches!(s, "--help" | "-h") && attached.is_none() {
            out.flags.insert("--help".into());
            continue;
        }
        if flags.contains(&s) && attached.is_none() {
            out.flags.insert(s.into());
            continue;
        }
        if values.contains(&s) {
            let value = attached
                .or_else(|| args.next().cloned())
                .ok_or_else(|| r::Error(2, format!("{s} requires a value")))?;
            if value.is_empty() || value.as_bytes().contains(&0) {
                return r::fail(format!("{s} requires a nonempty value"));
            }
            if matches!(
                s,
                "--repo"
                    | "--root"
                    | "--table"
                    | "--settings"
                    | "--ownership-receipt"
                    | "--receipt"
                    | "--compare"
                    | "--from"
            ) {
                let entries = out.paths.entry(s.into()).or_default();
                if !entries.is_empty() && s != "--repo" {
                    return r::fail(format!("{s} may be specified only once"));
                }
                entries.push(value);
            } else {
                let value = value
                    .into_string()
                    .map_err(|_| r::Error(2, format!("{s} requires a UTF-8 value")))?;
                let entries = out.values.entry(s.into()).or_default();
                if !entries.is_empty() && s != "--profile" {
                    return r::fail(format!("{s} may be specified only once"));
                }
                entries.push(value);
            }
            continue;
        }
        let s = raw
            .to_str()
            .ok_or_else(|| r::Error(2, "unknown non-UTF8 lifecycle argument".into()))?;
        if !s.starts_with('-')
            && out.action.is_empty()
            && matches!(tool, "tick" | "auto-update" | "agy-hook")
        {
            out.action = s.into();
            continue;
        }
        return r::fail(format!("unknown argument for {tool}: {s}"));
    }
    if out.has("--help") {
        return Ok(out);
    }
    for pair in [
        ["--fingerprint", "--compare"],
        ["--check", "--rollback"],
        ["--rollback", "--ref"],
        ["--rollback", "--fallback-to-edge"],
        ["--json", "--remediation-plan"],
        ["--apply", "--check-only"],
    ] {
        if out.has(pair[0]) && out.has(pair[1]) {
            return r::fail(format!("{} and {} are exclusive", pair[0], pair[1]));
        }
    }
    if tool == "update"
        && out.has("--plan")
        && ["--check", "--rollback", "--probe-agy-surfaces"]
            .iter()
            .any(|s| out.has(s))
    {
        return r::fail(
            "--plan cannot be combined with --check, --rollback or --probe-agy-surfaces",
        );
    }
    if tool == "update" {
        let modes = [
            "--check",
            "--plan",
            "--rollback",
            "--probe-agy-surfaces",
            "--prepare",
            "--apply-prepared",
            "--status",
        ];
        if modes.iter().filter(|s| out.has(s)).count() > 1 {
            return r::fail("update modes are exclusive");
        }
        if out.has("--status")
            && (out.flags.iter().any(|s| s != "--status" && s != "--json")
                || !out.values.is_empty()
                || !out.paths.is_empty())
        {
            return r::fail("--status accepts only --json");
        }
        if out.has("--apply-prepared")
            && ["--ref", "--fallback-to-edge", "--from", "--sha256"]
                .iter()
                .any(|s| out.has(s))
        {
            return r::fail("--apply-prepared uses the saved target and accepts no ref or bundle");
        }
        if out.has("--prepare")
            && ["--tools", "--no-tools", "--no-doctor"]
                .iter()
                .any(|s| out.has(s))
        {
            return r::fail("--prepare does not refresh tools or run the installation doctor");
        }
        if out.has("--from") != out.has("--sha256") {
            return r::fail("--from and --sha256 must be supplied together");
        }
        if out.has("--from") && !out.has("--prepare") {
            return r::fail("--from and --sha256 require --prepare");
        }
        if let Some(digest) = out.get("--sha256")
            && (digest.len() != 64
                || !digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        {
            return r::fail("--sha256 requires a lowercase SHA-256 digest");
        }
    }
    if tool == "doctor" {
        if out.has("--integrations")
            && ["--contract", "--live-models", "--strict-diversity"]
                .iter()
                .any(|s| out.has(s))
        {
            return r::fail("--integrations checks recorded wiring without live model probes");
        }
        let modes = [
            "--security",
            "--sandbox",
            "--surfaces",
            "--integrations",
            "--extensions",
            "--mcp",
            "--providers",
            "--tool-lock",
        ];
        if modes.iter().filter(|s| out.has(s)).count() > 1 {
            return r::fail("doctor standalone report modes are exclusive");
        }
        for (child, parent) in [
            ("--residue", "--security"),
            ("--probe", "--sandbox"),
            ("--pin", "--mcp"),
            ("--probe-tools", "--mcp"),
        ] {
            if out.has(child) && !out.has(parent) {
                return r::fail(format!("{child} requires {parent}"));
            }
        }
        if out.has("--repair") && modes.iter().any(|s| out.has(s)) {
            return r::fail("--repair cannot be combined with standalone reports");
        }
    }
    Ok(out)
}
pub(crate) fn execute(
    tool: &str,
    args: &[OsString],
    cwd: &Path,
    source: &Path,
    flag: &AtomicBool,
    services: &dyn crate::Services,
) -> Result<crate::Output> {
    if !crate::owns(tool, args) {
        return r::fail("command is not owned by the lifecycle crate");
    }
    let options = parse(tool, args)?;
    if options.has("--help") {
        return Ok(r::text(usage(tool)));
    }
    r::check(flag)?;
    r::native_host()?;
    if tool == "agy-hook" {
        return crate::agy_hook::run(cwd, source, &options, flag, services);
    }
    let mut paths = Paths::new(cwd, source)?;
    if tool != "tick"
        && let Some(repo) = options.path("--repo", &paths.cwd)?
    {
        paths.cwd = r::physical(&repo)?;
    }
    let mutation = !options.preview()
        && (matches!(
            tool,
            "install"
                | "link"
                | "unlink"
                | "uninstall"
                | "install-hooks"
                | "install-claude-hooks"
                | "install-mcp"
                | "install-codex-plugin"
                | "install-agy-plugin"
                | "install-autoupdate"
                | "uninstall-autoupdate"
        ) || matches!(tool, "install-tools" | "install-profile") && !options.has("--check")
            || tool == "doctor" && options.has("--repair"));
    if mutation {
        crate::update::borrowed(&paths)?;
    }
    match tool {
        "doctor" => crate::doctor::run(&paths, &options, flag, services),
        "status" => crate::doctor::status(&paths, &options, flag),
        "snapshot" | "write-machine-snapshot" => crate::snapshot::run(&paths, &options, flag),
        "update" | "auto-update" => crate::update::run(&paths, &options, flag, services),
        "tick" => crate::tick::run(&paths, &options, flag, services),
        "install-profile" | "install-tools" => crate::tools::run(&paths, &options, flag),
        "install-autoupdate" | "uninstall-autoupdate" => {
            crate::scheduler::run(&paths, &options, flag)
        }
        "install-hooks"
        | "install-claude-hooks"
        | "install-mcp"
        | "install-codex-plugin"
        | "install-agy-plugin" => crate::integrations::run(&paths, &options, flag, services),
        _ => crate::install::run(&paths, &options, flag, services),
    }
}
