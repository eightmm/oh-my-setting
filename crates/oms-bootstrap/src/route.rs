use crate::{Discovery, Error, SourceInventory, ValidatedPayload, discover, hex};
use std::ffi::{OsStr, OsString};
use std::io::IsTerminal;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

pub type CommandOutput = (i32, Vec<u8>, Vec<u8>);

/// Implement in the parent's native main. Use its registry for public names
/// and navigation and its existing pure ownership selectors for migration.
/// No second inventory of public commands belongs in this crate.
pub trait CommandAdapter {
    fn is_public(&self, name: &OsStr) -> bool;
    fn owns_native(&self, args: &[OsString]) -> bool;
    fn navigation(&self, args: &[OsString], source: &Path) -> CommandOutput;
}

#[derive(Debug, Clone, Copy)]
pub struct Terminal {
    pub stdin: bool,
    pub stdout: bool,
}
impl Terminal {
    pub fn current() -> Self {
        Self {
            stdin: std::io::stdin().is_terminal(),
            stdout: std::io::stdout().is_terminal(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Navigation {
    Usage,
    Version,
    Help,
    List,
}
pub struct LegacyCommand {
    pub source: Discovery,
    pub args: Vec<OsString>,
    pub tool: OsString,
    pub target: PathBuf,
    pub replace_process: bool,
}
impl LegacyCommand {
    /// This returns a checked script path only. The owner must establish its
    /// Bash 5 contract before executing it. No lifecycle producer is
    /// implemented by a fallback in this library.
    pub fn checked_target(&self) -> Result<PathBuf, Error> {
        self.source.check()?;
        let mut name = self.tool.clone();
        name.push(".sh");
        let path = Path::new("scripts").join(name);
        if !self.source.root.exists(&path)? {
            return Err(unknown_tool(&self.tool));
        }
        let file = self.source.root.file(&path, false)?;
        file.check()?;
        Ok(file.path())
    }
}

pub enum Route {
    Navigation {
        kind: Navigation,
        args: Vec<OsString>,
    },
    Native {
        args: Vec<OsString>,
    },
    Legacy {
        args: Vec<OsString>,
        tool: OsString,
        replace_process: bool,
    },
}
pub struct Prepared {
    source: Discovery,
    payload: Option<ValidatedPayload>,
    route: Route,
}
impl Prepared {
    pub fn source(&self) -> &Discovery {
        &self.source
    }
    pub fn payload(&self) -> Option<&ValidatedPayload> {
        self.payload.as_ref()
    }
    pub fn route(&self) -> &Route {
        &self.route
    }
    /// The parent calls this immediately before in-process native execution.
    /// The embedding main was authenticated by its trampoline before startup;
    /// this confirms it is also the payload selected by current source state.
    pub fn native_args(&self) -> Result<&[OsString], Error> {
        let Route::Native { args } = &self.route else {
            return Err(Error::integrity("frontdoor route is not native"));
        };
        let payload = self.payload.as_ref().ok_or_else(|| {
            Error::integrity("native payload unavailable; run oms doctor --repair")
        })?;
        payload.recheck(&self.source)?;
        payload.authenticates_current_process()?;
        Ok(args)
    }
    pub fn navigation(&self, adapter: &impl CommandAdapter) -> Result<CommandOutput, Error> {
        let Route::Navigation { args, .. } = &self.route else {
            return Err(Error::integrity("frontdoor route is not navigation"));
        };
        self.source.check()?;
        if let Some(payload) = &self.payload {
            payload.recheck(&self.source)?;
        }
        Ok(adapter.navigation(args, self.source.source_root()))
    }
    pub fn into_legacy(self) -> Result<LegacyCommand, Error> {
        let Route::Legacy {
            args,
            tool,
            replace_process,
        } = self.route
        else {
            return Err(Error::integrity("frontdoor route is not legacy"));
        };
        let mut name = tool.clone();
        name.push(".sh");
        let target = self.source.scripts().join(name);
        let command = LegacyCommand {
            source: self.source,
            args,
            tool,
            target,
            replace_process,
        };
        command.checked_target()?;
        Ok(command)
    }
    /// For a separate trusted launcher, selection happens before exec. The
    /// payload action's exit status is final and never controls fallback.
    pub fn into_native_exec(self) -> Result<crate::CheckedExec, Error> {
        let Route::Native { args } = self.route else {
            return Err(Error::integrity("frontdoor route is not native"));
        };
        let payload = self.payload.ok_or_else(|| {
            Error::integrity("native payload unavailable; run oms doctor --repair")
        })?;
        Ok(payload.into_exec(self.source, args))
    }
}

pub struct Frontdoor;
impl Frontdoor {
    /// Must run before unrelated Python activation, including for COPY.
    /// A migrated invocation never calls an interpreter during preparation.
    pub fn prepare(
        entry: &Path,
        args: &[OsString],
        terminal: Terminal,
        adapter: &impl CommandAdapter,
        inventory: &SourceInventory<'_>,
    ) -> Result<Prepared, Error> {
        platform()?;
        let source = discover(entry, None)?;
        let route = select(args, terminal, adapter)?;
        let payload = if matches!(route, Route::Navigation { .. } | Route::Native { .. }) {
            ValidatedPayload::load(&source, inventory)?
        } else {
            None
        };
        // Missing payloads retain the staged rollout's legacy behavior. Help
        // and list can always use the authenticated embedding registry.
        let route = if payload.is_none() {
            if let Route::Native { args } = route {
                legacy(&args)?
            } else {
                route
            }
        } else {
            route
        };
        let prepared = Prepared {
            source,
            payload,
            route,
        };
        if let Route::Legacy { tool, .. } = &prepared.route {
            let mut name = tool.clone();
            name.push(".sh");
            let path = Path::new("scripts").join(name);
            if !prepared.source.root.exists(&path)? {
                return Err(unknown_tool(tool));
            }
            prepared.source.root.file(&path, false)?;
        }
        Ok(prepared)
    }
}

fn select(
    args: &[OsString],
    terminal: Terminal,
    adapter: &impl CommandAdapter,
) -> Result<Route, Error> {
    let first = args.first().map(|v| v.as_bytes()).unwrap_or(b"");
    if first.is_empty() {
        return if terminal.stdin && terminal.stdout {
            // Bare interactive `oms` is `oms panel`: honor native ownership.
            let panel = vec![OsString::from("panel")];
            if adapter.owns_native(&panel) {
                Ok(Route::Native { args: panel })
            } else {
                legacy(&panel)
            }
        } else {
            Ok(Route::Navigation {
                kind: Navigation::Usage,
                args: vec![OsString::from("--help")],
            })
        };
    }
    let navigation = match first {
        b"-h" | b"--help" => Some(Navigation::Usage),
        b"--version" => Some(Navigation::Version),
        b"help" => Some(Navigation::Help),
        b"list" => Some(Navigation::List),
        _ => None,
    };
    if let Some(kind) = navigation {
        return Ok(Route::Navigation {
            kind,
            args: args.to_vec(),
        });
    }
    if !first
        .iter()
        .all(|v| v.is_ascii_alphanumeric() || matches!(v, b'.' | b'_' | b'-'))
    {
        let mut output = b"error: invalid tool name: ".to_vec();
        output.extend_from_slice(args[0].as_bytes());
        return Err(Error::bytes(2, output));
    }
    let tool = OsStr::from_bytes(first.strip_suffix(b".sh").unwrap_or(first));
    if !adapter.is_public(tool) {
        return Err(unknown_tool(tool));
    }
    let mut normalized = args.to_vec();
    normalized[0] = tool.to_owned();
    if adapter.owns_native(&normalized) {
        Ok(Route::Native { args: normalized })
    } else {
        legacy(&normalized)
    }
}
pub fn unknown_tool(tool: &OsStr) -> Error {
    let mut output = b"error: unknown tool: ".to_vec();
    output.extend_from_slice(tool.as_bytes());
    output.extend_from_slice(b" (run: oms list --all)");
    Error::bytes(2, output)
}
fn legacy(args: &[OsString]) -> Result<Route, Error> {
    let tool = args
        .first()
        .ok_or_else(|| Error::integrity("missing legacy tool"))?
        .clone();
    let replace_process = matches!(tool.as_bytes(), b"panel" | b"harness");
    Ok(Route::Legacy {
        args: args[1..].to_vec(),
        tool,
        replace_process,
    })
}

fn platform() -> Result<(), Error> {
    fn windows(value: &OsStr) -> bool {
        let bytes = value.as_bytes();
        matches!(bytes, b"windows" | b"Windows" | b"WINDOWS")
            || [
                b"MINGW".as_slice(),
                b"MSYS",
                b"CYGWIN",
                b"msys",
                b"cygwin",
                b"win32",
            ]
            .iter()
            .any(|prefix| bytes.starts_with(prefix))
    }
    if std::env::var_os("OMS_PLATFORM_OVERRIDE").is_some_and(|v| windows(&v))
        || std::env::var_os("OSTYPE").is_some_and(|v| windows(&v))
        || !matches!(std::env::consts::OS, "linux" | "macos")
    {
        return Err(Error::discovery(
            "native Windows (Git Bash/MSYS2/Cygwin) is not supported; install inside WSL2 with the repository and home on the Linux filesystem (not /mnt/c).",
        ));
    }
    Ok(())
}

/// Private arguments are parsed as OS strings, never shell or JSON text.
/// Parsing grants no authority: callers must then discover/validate the root.
#[derive(Debug)]
pub enum PrivateInvocation {
    Frontdoor {
        entry: PathBuf,
        args: Vec<OsString>,
    },
    Select {
        root: PathBuf,
        revision: String,
        args: Vec<OsString>,
    },
    Dispatch {
        root: PathBuf,
        revision: String,
        args: Vec<OsString>,
    },
}
impl PrivateInvocation {
    pub fn parse(args: &[OsString]) -> Result<Option<Self>, Error> {
        let Some(mode) = args.first().map(|v| v.as_bytes()) else {
            return Ok(None);
        };
        if mode == b"--oms-frontdoor" {
            if args.len() < 3 || args[2] != "--" || args[1].is_empty() {
                return Err(Error::integrity(
                    "runtime-core: invalid private frontdoor invocation",
                ));
            }
            return Ok(Some(Self::Frontdoor {
                entry: PathBuf::from(&args[1]),
                args: args[3..].to_vec(),
            }));
        }
        if !matches!(mode, b"--oms-select" | b"--oms-dispatch") {
            return Ok(None);
        }
        if args.len() < 5
            || args[3] != "--"
            || !Path::new(&args[1]).is_absolute()
            || !hex(args[2].as_bytes(), 40)
        {
            return Err(Error::integrity(
                "runtime-core: invalid private dispatch invocation",
            ));
        }
        let root = PathBuf::from(&args[1]);
        let revision = args[2].to_str().expect("hex revision").to_owned();
        let args = args[4..].to_vec();
        Ok(Some(if mode == b"--oms-select" {
            Self::Select {
                root,
                revision,
                args,
            }
        } else {
            Self::Dispatch {
                root,
                revision,
                args,
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Panel(bool);
    impl CommandAdapter for Panel {
        fn is_public(&self, _: &OsStr) -> bool {
            true
        }
        fn owns_native(&self, args: &[OsString]) -> bool {
            self.0 && args == [OsString::from("panel")]
        }
        fn navigation(&self, _: &[OsString], _: &Path) -> CommandOutput {
            (0, Vec::new(), Vec::new())
        }
    }
    const TTY: Terminal = Terminal {
        stdin: true,
        stdout: true,
    };

    #[test]
    fn bare_interactive_entry_honors_native_panel_ownership() {
        match select(&[], TTY, &Panel(true)).unwrap() {
            Route::Native { args } => assert_eq!(args, [OsString::from("panel")]),
            _ => panic!("owned panel must be native"),
        }
        match select(&[], TTY, &Panel(false)).unwrap() {
            Route::Legacy {
                tool,
                replace_process,
                ..
            } => {
                assert_eq!(tool, "panel");
                assert!(replace_process);
            }
            _ => panic!("unowned panel stays legacy"),
        }
        let pipe = Terminal {
            stdin: true,
            stdout: false,
        };
        assert!(matches!(
            select(&[], pipe, &Panel(true)).unwrap(),
            Route::Navigation {
                kind: Navigation::Usage,
                ..
            }
        ));
    }
}
