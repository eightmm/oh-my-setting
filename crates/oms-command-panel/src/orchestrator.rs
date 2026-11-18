//! Persistent native provider windows beneath the room's interactive main.
use crate::{Error, Output, Result, Services, cli::Options, commands, json, require};
use oms_panel_core::{NativeCommand, Store, Value, terminals};
use serde_json::json;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub const PROVIDERS: [&str; 3] = ["codex", "claude", "antigravity"];
pub const MAIN_WINDOW: &str = "Overall Main";

const STARTUP_PROMPT: &str = "You are this project's provider orchestrator under Overall Main. \
Wait for an assignment from Overall Main. For each assignment, give this provider's native \
subagents or team tools bounded, nonoverlapping work and clear completion checks when the \
work can be split. Coordinate their results, review the changes, and run the relevant checks \
before reporting back. Report the result, verification evidence, and remaining blockers in \
this session. When the native child launcher offers a full-permission option, select it \
for work authorized by the assignment. Keep each child within its assigned scope, leave \
OMS parent-only actions to Overall Main, and preserve the project's approval requirements. \
Report effective child permissions as unverified when the provider does not confirm them.";

fn orchestrator_command(provider: &str, options: &Options) -> Result<Vec<OsString>> {
    let (program, default_model, permission_flag, prompt_option) = match provider {
        "codex" => (
            "codex",
            "gpt-6.2-sol",
            "--dangerously-bypass-approvals-and-sandbox",
            "--",
        ),
        "claude" => (
            "claude",
            "opus",
            "--dangerously-skip-permissions",
            "--append-system-prompt",
        ),
        "antigravity" => (
            "agy",
            "gemini-pro-orchestrator",
            "--dangerously-skip-permissions",
            "--prompt-interactive",
        ),
        _ => return Err(Error::new("unsupported provider orchestrator")),
    };
    let model = options.get("model").unwrap_or(default_model);
    let mut argv = commands::strings(&[program, "--model", model, permission_flag]);
    if let Some(effort) = options.get("reasoning-effort") {
        require(
            provider != "antigravity" || !model.starts_with("gemini-pro-orchestrator"),
            "Antigravity Orchestrator has fixed effort; omit --reasoning-effort or select an adjustable model",
        )?;
        // Match the existing peer transport. Requested tokens do not prove the
        // native CLI accepted them or that the service applied this effort.
        if provider == "codex" {
            argv.extend(commands::strings(&[
                "-c",
                &format!("model_reasoning_effort=\"{effort}\""),
            ]));
        } else {
            argv.extend(commands::strings(&["--effort", effort]));
        }
    }
    argv.extend(commands::strings(&[prompt_option, STARTUP_PROMPT]));
    Ok(argv)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pane {
    window: String,
    id: String,
    pid: u32,
    active: bool,
}

pub struct Panel<'a> {
    repo: &'a Path,
    services: &'a dyn Services,
    stop: &'a AtomicBool,
    session: String,
    caller: String,
    main: Pane,
    attempt: String,
    owner: String,
    room: &'a str,
    leads: BTreeMap<String, Pane>,
    live_providers: BTreeSet<&'static str>,
}

// Nonempty last field keeps tmux's trimmed output from dropping trailing columns.
const PANE_FORMAT: &str = "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{window_active}\t#{@oms_panel_native_pane}\t#{@oms_panel_main_attempt}\t#{@oms_panel_owner}\t#{@oms_panel_room_participant}\t#{@oms_panel_room}\t#{@oms_provider_session}\t#{@oms_provider_parent}\t.";

#[derive(Default)]
pub struct Observations {
    panes: BTreeMap<String, Observation>,
}

impl Observations {
    pub fn quota_pending(&self) -> bool {
        self.panes.values().any(|sample| {
            sample
                .pending
                .is_some_and(|started| started.elapsed() < Duration::from_secs(15))
        })
    }
}

#[derive(Default)]
struct Observation {
    pid: u32,
    model: Option<String>,
    weekly: Option<(f64, f64)>,
    quota_visible: bool,
    pending: Option<Instant>,
    agy_parent: Option<std::path::PathBuf>,
}

impl<'a> Panel<'a> {
    pub fn load(
        repo: &'a Path,
        room: &'a str,
        services: &'a dyn Services,
        stop: &'a AtomicBool,
    ) -> Result<Self> {
        let caller = std::env::var("TMUX_PANE").unwrap_or_default();
        require(
            commands::valid_target(&caller, '%'),
            "open an OMS tmux panel first",
        )?;
        let raw = terminals::tmux(
            services,
            repo,
            &commands::strings(&["list-panes", "-s", "-t", &caller, "-F", PANE_FORMAT]),
            stop,
        )?;
        let rows: Vec<Vec<&str>> = raw.lines().map(|line| line.split('\t').collect()).collect();
        let mut mains = rows.iter().filter(|f| {
            f.len() == 14
                && f[4] == "0"
                && f[6] == f[2]
                && f[7] == f[9]
                && f[10] == room
                && valid_attempt(f[7])
                && PROVIDERS.contains(&f[8])
        });
        let main = mains
            .next()
            .ok_or_else(|| Error::new("live room Main unavailable"))?;
        require(mains.next().is_none(), "multiple room Main panes found")?;
        let session = main[0].to_owned();
        terminals::session_proof(services, repo, &session, stop)?;
        let mut leads = BTreeMap::new();
        let mut live_providers = BTreeSet::new();
        for f in &rows {
            let Some(provider) = live_provider(f, &session, main[2]) else {
                continue;
            };
            live_providers.insert(provider);
            if f[10] == room && f[12] == main[7] {
                require(
                    leads.insert(provider.into(), pane(f)?).is_none(),
                    "multiple windows for the provider session",
                )?;
            }
        }
        Ok(Self {
            repo,
            services,
            stop,
            session,
            caller,
            main: pane(main)?,
            attempt: main[7].into(),
            owner: main[8].into(),
            room,
            leads,
            live_providers,
        })
    }

    pub fn connected(&self) -> Value {
        json!(self.leads.keys().collect::<Vec<_>>())
    }

    pub fn observe(&self, readings: &mut Observations, report: &mut Value) {
        let now = oms_panel_core::timestamp() as f64;
        report["provider_sessions"] = self.connected();
        report["provider_models"] = json!({});
        let mut children = Vec::new();
        readings.panes.retain(|provider, sample| {
            self.leads
                .get(provider)
                .is_some_and(|pane| pane.pid == sample.pid)
        });
        for (provider, pane) in &self.leads {
            let sample = readings
                .panes
                .entry(provider.clone())
                .or_insert_with(|| Observation {
                    pid: pane.pid,
                    ..Observation::default()
                });
            if let Ok(screen) = self.tmux(&["capture-pane", "-p", "-t", &pane.id]) {
                if let Some(model) = observed_model(provider, &screen) {
                    sample.model = Some(model.to_owned());
                }
                let current = weekly_from_usage(provider, &screen);
                if let Some(used) = current {
                    if !sample.quota_visible || sample.weekly.is_none_or(|(last, _)| last != used) {
                        sample.weekly = Some((used, now));
                    }
                    if sample
                        .pending
                        .take()
                        .is_some_and(|started| started.elapsed() < Duration::from_secs(15))
                        && !pane.active
                        && Self::load(self.repo, self.room, self.services, self.stop).is_ok_and(
                            |fresh| {
                                fresh.leads.get(provider) == Some(pane)
                                    && fresh.require_board(None).is_ok()
                            },
                        )
                    {
                        let _ = self.tmux(&["send-keys", "-t", &pane.id, "Escape"]);
                    }
                }
                sample.quota_visible = current.is_some();
                if ["claude", "antigravity"].contains(&provider.as_str()) {
                    let recent = if provider == "antigravity" {
                        self.tmux(&["capture-pane", "-p", "-J", "-S", "-100", "-t", &pane.id])
                            .unwrap_or(screen)
                    } else {
                        screen
                    };
                    children.extend(crate::native_children::observe(
                        provider,
                        pane.pid,
                        self.repo,
                        &recent,
                        &mut sample.agy_parent,
                    ));
                }
            }
            if let Some(model) = &sample.model {
                report["provider_models"][provider] = json!(model);
            }
            if let Some((used, observed_at)) = sample.weekly {
                let status = &mut report["provider_status"][provider];
                if oms_panel_core::metrics::stamp(&status["observed_at"])
                    .is_none_or(|old| old < observed_at)
                {
                    status["weekly_used"] = json!(used);
                    status["weekly_native_observed_at"] = json!(observed_at);
                }
            }
        }
        report["native_children"] = json!(children);
    }

    pub fn refresh_usage(&self, readings: &mut Observations) -> usize {
        if self.require_board(None).is_err() {
            return 0;
        }
        let mut started = 0;
        for provider in ["claude", "antigravity"] {
            let Some(pane) = self.leads.get(provider) else {
                continue;
            };
            if self.send(provider, "/usage").is_ok() {
                let sample = readings.panes.entry(provider.into()).or_default();
                if sample.pid != pane.pid {
                    *sample = Observation {
                        pid: pane.pid,
                        ..Observation::default()
                    };
                }
                sample.pending = Some(Instant::now());
                sample.quota_visible = false;
                started += 1;
            }
        }
        started
    }

    fn tmux(&self, args: &[&str]) -> Result<String> {
        terminals::tmux(
            self.services,
            self.repo,
            &commands::strings(args),
            self.stop,
        )
    }

    pub fn focus(&self, provider: &str) -> Result<()> {
        crate::parent_only()?;
        let lead = self
            .leads
            .get(provider)
            .ok_or_else(|| Error::new("provider session is not connected to this Main"))?;
        self.tmux(&[
            "select-window",
            "-t",
            &format!("{}:{}", self.session, lead.window),
        ])?;
        self.tmux(&["select-pane", "-t", &lead.id])?;
        Ok(())
    }

    pub(crate) fn docked(&self) -> Option<&'static str> {
        PROVIDERS.into_iter().find(|provider| {
            self.leads
                .get(*provider)
                .is_some_and(|pane| pane.window == self.main.window)
        })
    }

    fn require_main(&self, owner: Option<&str>) -> Result<()> {
        require(
            self.caller == self.main.id
                && std::env::var("OMS_PANEL_MAIN_ATTEMPT").as_deref() == Ok(&self.attempt)
                && std::env::var("OMS_ROOM_PARTICIPANT").as_deref() == Ok(&self.attempt)
                && owner.is_none_or(|owner| owner == self.owner),
            "provider sessions are managed by the room's Main",
        )?;
        self.require_membership()?;
        let attempts = commands::query(
            self.services,
            NativeCommand::Events,
            self.repo,
            &["list", "--json"],
            self.stop,
        )?;
        require(
            std::env::var_os("OMS_CAMPAIGN_MANIFEST_SHA256").is_none()
                && crate::rows(&attempts).iter().any(|row| {
                    row["attempt_id"] == self.attempt
                        && row["tool"] == "panel-main"
                        && row["terminal"] == false
                        && row["refs"]["panel_room_id"] == self.room
                        && row["refs"]["panel_native_scheduling"] == "unmanaged"
                }),
            "native provider scheduling requires a verified Main outside a managed campaign",
        )
    }

    fn require_membership(&self) -> Result<()> {
        let room = commands::query(
            self.services,
            NativeCommand::Room,
            self.repo,
            &["show", "--id", self.room, "--json"],
            self.stop,
        )?;
        require(
            room["closed"] == false
                && crate::rows(&room["participants"]).iter().any(|row| {
                    row["participant"] == self.attempt
                        && row["role"] == "main"
                        && row["provider"] == self.owner
                        && row["joined"] == true
                }),
            "Main membership in this room could not be verified",
        )
    }

    fn require_board(&self, owner: Option<&str>) -> Result<()> {
        crate::parent_only()?;
        let board = self.tmux(&[
            "display-message",
            "-p",
            "-t",
            &self.caller,
            "#{session_id}\t#{window_id}\t#{pane_pid}\t#{pane_dead}\t#{window_active}\t#{pane_in_mode}\t#{@oms_panel_main_watch}\t#{@oms_panel_main_attempt}\t#{@oms_panel_room}\t#{@oms_panel_repo}",
        ])?;
        require(
            self.caller != self.main.id
                && owner.is_none_or(|owner| owner == self.owner)
                && board.split('\t').eq([
                    self.session.as_str(),
                    self.main.window.as_str(),
                    &std::process::id().to_string(),
                    "0",
                    "1",
                    "0",
                    "1",
                    self.attempt.as_str(),
                    self.room,
                    self.repo.to_str().unwrap_or_default(),
                ]),
            "native chat requires the managed board in the active Main window",
        )?;
        self.require_membership()
    }

    /// Move the existing provider PTY between its own window and the bottom of the board.
    pub(crate) fn toggle_dock(
        &self,
        provider: Option<&str>,
        preferred_rows: usize,
        owner: Option<&str>,
    ) -> Result<Option<&'static str>> {
        crate::parent_only()?;
        let store = Store::open(self.repo)?;
        let tx = store.transaction("panel-provider-sessions", Duration::from_secs(3))?;
        let fresh = Self::load(self.repo, self.room, self.services, self.stop)?;
        require(
            self.main == fresh.main && self.leads == fresh.leads,
            "provider panes changed; select the provider again",
        )?;
        fresh.require_board(owner)?;
        let current = self.docked();
        let next = provider.filter(|provider| Some(*provider) != current);
        let lead = next
            .map(|provider| {
                self.leads
                    .get(provider)
                    .ok_or_else(|| Error::new("provider session is unavailable"))
            })
            .transpose()?;
        let board_panes = self.tmux(&[
            "display-message",
            "-p",
            "-t",
            &self.caller,
            "#{window_panes}",
        ])?;
        require(
            board_panes == if current.is_some() { "3" } else { "2" },
            "native chat needs the managed board and Main layout",
        )?;
        if let Some(lead) = lead {
            require(
                self.tmux(&["display-message", "-p", "-t", &lead.id, "#{window_panes}"])? == "1",
                "provider window has additional panes",
            )?;
        }
        let mut rows = preferred_rows;
        if let Some(current) = current {
            let pane = &self.leads[current];
            rows = self
                .tmux(&["display-message", "-p", "-t", &pane.id, "#{pane_height}"])?
                .parse()
                .map_err(Error::new)?;
            tx.check()?;
            self.tmux(&[
                "break-pane",
                "-d",
                "-s",
                &pane.id,
                "-n",
                &format!("{} Orchestrator", provider_name(current)),
            ])?;
            self.tag_provider(&pane.id, current, "-w")?;
        }
        if let (Some(provider), Some(lead)) = (next, lead) {
            let height: usize = self
                .tmux(&[
                    "display-message",
                    "-p",
                    "-t",
                    &self.caller,
                    "#{pane_height}",
                ])?
                .parse()
                .map_err(Error::new)?;
            require(height >= 17, "make the panel taller to open native chat")?;
            let size = rows.clamp(8, height - 9).to_string();
            tx.check()?;
            self.tag_provider(&lead.id, provider, "-p")?;
            self.tmux(&[
                "join-pane",
                "-d",
                "-v",
                "-l",
                &size,
                "-s",
                &lead.id,
                "-t",
                &self.caller,
            ])?;
            self.tmux(&["select-pane", "-t", &lead.id])?;
        }
        Ok(PROVIDERS
            .into_iter()
            .find(|provider| Some(*provider) == next))
    }

    fn tag_provider(&self, id: &str, provider: &str, scope: &str) -> Result<()> {
        for (key, value) in [
            ("@oms_panel_room", self.room),
            ("@oms_provider_parent", &self.attempt),
            ("@oms_provider_session", provider),
        ] {
            self.tmux(&["set-option", scope, "-t", id, key, value])?;
        }
        Ok(())
    }

    fn open(&self, provider: &str, options: &Options) -> Result<String> {
        require(
            !self.live_providers.contains(provider),
            "provider already has a live pane in this panel",
        )?;
        let command = orchestrator_command(provider, options)?;
        let global = self.tmux(&["show-environment", "-g"])?;
        let session = self.tmux(&["show-environment", "-t", &self.session])?;
        let strip: std::collections::BTreeSet<_> = global
            .lines()
            .chain(session.lines())
            .filter_map(|line| line.split_once('=').map(|(key, _)| key))
            .filter(|key| inherited_authority(key))
            .collect();
        let name = format!("{} Orchestrator", provider_name(provider));
        let mut argv = commands::strings(&[
            "new-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            &format!("{}:", self.session),
            "-n",
            &name,
            "-c",
        ]);
        argv.push(self.repo.into());
        argv.push("env".into());
        for key in strip {
            argv.extend(commands::strings(&["-u", key]));
        }
        argv.extend(commands::strings(&["-u", "TMUX", "-u", "TMUX_PANE"]));
        // Keep the OMS child markers even when the provider runs with full native permissions.
        argv.extend(commands::strings(&[
            "OMS_HARNESS_CHILD=1",
            "OMS_HARNESS_DELEGATE_DEPTH=1",
        ]));
        argv.extend(command);
        let id = terminals::tmux(self.services, self.repo, &argv, self.stop)?;
        require(
            commands::valid_target(&id, '%'),
            "provider pane could not be verified",
        )?;
        if let Err(error) = self.tag_provider(&id, provider, "-w") {
            let cleanup = AtomicBool::new(false);
            terminals::tmux(
                self.services,
                self.repo,
                &commands::strings(&["kill-pane", "-t", &id]),
                &cleanup,
            )
            .map_err(|cleanup| {
                Error::new(format!(
                    "{error}; new provider pane {id} cleanup failed: {cleanup}"
                ))
            })?;
            return Err(error);
        }
        let _ = self.tmux(&["rename-window", "-t", &self.main.window, MAIN_WINDOW]);
        let _ = self.tmux(&["set-option", "-t", &self.session, "mouse", "on"]);
        Ok(id)
    }

    fn send(&self, provider: &str, prompt: &str) -> Result<()> {
        let lead = self
            .leads
            .get(provider)
            .ok_or_else(|| Error::new("open the provider session before sending a task"))?;
        require(
            !lead.active,
            "provider window is focused; use it directly or return to Main",
        )?;
        let cursor = self.tmux(&[
            "display-message",
            "-p",
            "-t",
            &lead.id,
            "#{pane_in_mode}|#{cursor_x}|#{cursor_y}",
        ])?;
        let row = cursor
            .strip_prefix("0|2|")
            .and_then(|row| row.parse::<u32>().ok())
            .ok_or_else(|| Error::new("provider cursor is outside the start of its input"))?
            .to_string();
        let input = self.tmux(&["capture-pane", "-p", "-t", &lead.id, "-S", &row, "-E", &row])?;
        require(
            input.starts_with(['>', '❯', '›']),
            "provider cursor does not point to a native prompt",
        )?;
        let capture = self.tmux(&["capture-pane", "-p", "-e", "-S", &row, "-t", &lead.id])?;
        require(
            input_ready(provider, &capture),
            "provider input is busy or a draft is present; try again when ready",
        )?;
        let fresh = Self::load(self.repo, self.room, self.services, self.stop)?;
        require(
            self.main == fresh.main && fresh.leads.get(provider) == Some(lead),
            "provider or Main pane changed before typing",
        )?;
        self.tmux(&["send-keys", "-t", &lead.id, "-l", &literal_tmux_arg(prompt)])?;
        std::thread::sleep(Duration::from_millis(150));
        let fresh = Self::load(self.repo, self.room, self.services, self.stop)?;
        require(
            self.main == fresh.main && fresh.leads.get(provider) == Some(lead),
            "pane changed after typing; prompt submission is unconfirmed",
        )?;
        self.tmux(&["send-keys", "-t", &lead.id, "Enter"])
            .map_err(|_| Error::new("prompt submission is unconfirmed"))?;
        Ok(())
    }
}

fn literal_tmux_arg(text: &str) -> Cow<'_, str> {
    // tmux consumes trailing semicolons as command separators even with send-keys -l.
    match text.strip_suffix(';') {
        Some(prefix) => Cow::Owned(format!(r"{prefix}\;")),
        None => Cow::Borrowed(text),
    }
}

fn weekly_from_usage(provider: &str, screen: &str) -> Option<f64> {
    let (heading, marker) = match provider {
        "claude"
            if screen.lines().any(|line| {
                line.contains("Settings") && line.contains("Status") && line.contains("Usage")
            }) =>
        {
            ("Current week (all models)", "% used")
        }
        "antigravity" if screen.lines().any(|line| line.trim() == "└ Models & Quota") => {
            ("Weekly Limit Remaining", "%")
        }
        _ => return None,
    };
    let mut lines = screen.lines().map(str::trim);
    if provider == "antigravity" {
        lines.find(|line| *line == "GEMINI MODELS")?;
    }
    lines
        .by_ref()
        .take_while(|line| *line != "CLAUDE AND GPT MODELS")
        .find(|line| *line == heading)?;
    // Locate the amount on the next rendered row inside the selected weekly window.
    let progress = lines.next()?;
    let (pct, suffix) = progress.split_once(marker)?;
    if !suffix.trim().is_empty() {
        return None;
    }
    let pct = pct.split_whitespace().next_back()?.parse::<f64>().ok()?;
    if !(0.0..=100.0).contains(&pct) {
        return None;
    }
    Some(if provider == "antigravity" {
        100.0 - pct
    } else {
        pct
    })
}

fn valid_attempt(attempt: &str) -> bool {
    attempt.strip_prefix("att_").is_some_and(|s| {
        s.len() == 32
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn live_provider(fields: &[&str], session: &str, main_pane: &str) -> Option<&'static str> {
    if fields.len() != 14 || fields[0] != session || fields[4] != "0" || fields[2] == main_pane {
        return None;
    }
    PROVIDERS
        .into_iter()
        .find(|provider| *provider == fields[11])
}

fn pane(f: &[&str]) -> Result<Pane> {
    require(
        commands::valid_target(f[1], '@') && commands::valid_target(f[2], '%'),
        "invalid pane binding",
    )?;
    Ok(Pane {
        window: f[1].into(),
        id: f[2].into(),
        pid: f[3].parse().map_err(Error::new)?,
        active: f[5] == "1",
    })
}

fn inherited_authority(key: &str) -> bool {
    key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && ([
            "OMS_",
            "CODEX_THREAD_",
            "CODEX_SESSION_",
            "CLAUDE_CODE_",
            "AGY_SESSION_",
            "ANTIGRAVITY_SESSION_",
        ]
        .iter()
        .any(|prefix| key.starts_with(prefix))
            || [
                "CLAUDECODE",
                "CLAUDE_SESSION_ID",
                "CODEX_INTERNAL_ORIGINATOR_OVERRIDE",
            ]
            .contains(&key))
}

fn input_ready(provider: &str, capture: &str) -> bool {
    if provider == "codex"
        && let Some(input) = codex_prompt_area(capture)
    {
        return oms_panel_core::tick::screen_input_empty(input).0;
    }
    // AGY 1.5 represents this placeholder with a dedicated foreground instead of dim SGR.
    const AGY_HINT: &str = "Auto mode: edits & safe commands auto-approved (shift+tab to cycle)";
    if provider == "antigravity" {
        let hint = format!("\x1b[38;5;109m{AGY_HINT}\x1b[39m");
        let dim = format!("\x1b[2m{AGY_HINT}\x1b[0m");
        return oms_panel_core::tick::screen_input_empty(&capture.replace(&hint, &dim)).0;
    }
    oms_panel_core::tick::screen_input_empty(capture).0
}

fn codex_prompt_area(capture: &str) -> Option<&str> {
    // The status and key-help rows sit outside the prompt, with blank space
    // in place of the explicit bottom divider used by the other providers.
    let mut visible = capture
        .lines()
        .rev()
        .filter(|line| !crate::render::clean(line, 4096).trim().is_empty());
    let keys = crate::render::clean(visible.next()?, 4096);
    let status = visible.next()?;
    if keys.trim() != "← for agents · ? for shortcuts"
        || observed_model("codex", &crate::render::clean(status, 4096)).is_none()
    {
        return None;
    }
    capture.rfind(status).map(|start| &capture[..start])
}

fn observed_model<'a>(provider: &str, screen: &'a str) -> Option<&'a str> {
    screen
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(6)
        .find_map(|line| {
            let line = line.trim();
            let model = match provider {
                "codex" => {
                    let (prefix, status) = line.split_once(" · Context ")?;
                    if !status.contains("% left") || !prefix.starts_with("GPT-") {
                        return None;
                    }
                    match prefix.rsplit_once(' ') {
                        Some((model, "low" | "medium" | "high" | "xhigh" | "max" | "ultra")) => {
                            model
                        }
                        _ => prefix,
                    }
                }
                "claude" if line.contains(" | ctx ") => {
                    let (model, _) = line.split_once(" | ")?;
                    if !["Sonnet ", "Opus ", "Haiku "]
                        .iter()
                        .any(|prefix| model.starts_with(prefix))
                    {
                        return None;
                    }
                    model
                }
                "antigravity" if line.contains(" | ctx ") => {
                    line.split_once(" (Antigravity")?.0.trim_end()
                }
                _ => return None,
            };
            (!model.is_empty()
                && model.len() <= 80
                && model
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || " .-()_/".contains(c)))
            .then_some(model)
        })
}

pub static CHAT_SGR: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\x1b\[[0-9;]{0,64}m").expect("static SGR pattern")
});

pub fn provider_name(provider: &str) -> &str {
    match provider {
        "codex" => "Codex",
        "claude" => "Claude",
        _ => "AGY",
    }
}

pub fn run(
    repo: &Path,
    provider: &str,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    crate::parent_only()?;
    let room = commands::room(options).ok_or_else(|| Error::new("select the Main's room"))?;
    let store = Store::open(repo)?;
    let tx = store.transaction("panel-provider-sessions", Duration::from_secs(3))?;
    let panel = Panel::load(repo, &room, services, stop)?;
    tx.check()?;
    let status = if options.has("focus-orchestrator") {
        panel.focus(provider)?;
        "focused"
    } else {
        panel.require_main(options.get("owner"))?;
        tx.check()?;
        if let Some(task) = options.get("task") {
            require(
                !task.trim().is_empty()
                    && task.len() <= 4096
                    && !task.chars().any(char::is_control),
                "provider task must be a nonempty single line of up to 4096 bytes",
            )?;
            let turn = oms_panel_core::digest(
                format!(
                    "{}:{provider}:{}:{task}",
                    panel.attempt,
                    oms_panel_core::now()
                )
                .as_bytes(),
            );
            panel.send(provider, &format!(
                "Task from Overall Main. Work within this task and report the result here. Turn {}. You own this provider's work. Give native subagents bounded, disjoint work and completion checks. Wait for them, verify their output yourself, and report results, checks and unfinished work. Stay within the assigned authority. Task: {task}", &turn[..20]
            ))?;
            "submitted"
        } else if panel.leads.contains_key(provider) {
            require(
                !(options.has("model") || options.has("reasoning-effort")),
                "provider session is already connected; model and effort cannot be changed by reconnecting",
            )?;
            "connected"
        } else {
            panel.open(provider, options)?;
            "started"
        }
    };
    Ok((
        0,
        json(&json!({"status": status, "provider": provider}))?,
        vec![],
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        STARTUP_PROMPT, input_ready, literal_tmux_arg, live_provider, observed_model,
        orchestrator_command, weekly_from_usage,
    };
    use crate::{cli::Options, commands};

    #[test]
    fn provider_commands_apply_model_permissions_and_startup_role() {
        for (provider, program, model, permission_flag, prompt_option) in [
            (
                "codex",
                "codex",
                "gpt-6.2-sol",
                "--dangerously-bypass-approvals-and-sandbox",
                "--",
            ),
            (
                "claude",
                "claude",
                "opus",
                "--dangerously-skip-permissions",
                "--append-system-prompt",
            ),
            (
                "antigravity",
                "agy",
                "gemini-pro-orchestrator",
                "--dangerously-skip-permissions",
                "--prompt-interactive",
            ),
        ] {
            assert_eq!(
                orchestrator_command(provider, &Options::default()).unwrap(),
                [
                    program,
                    "--model",
                    model,
                    permission_flag,
                    prompt_option,
                    STARTUP_PROMPT,
                ]
            );
            for (selected_model, effort) in [
                (Some("fixture-model"), None),
                (Some("fixture-model"), Some("xhigh")),
                (None, Some("xhigh")),
            ] {
                let mut args = commands::strings(&["--orchestrator", provider]);
                if let Some(selected_model) = selected_model {
                    args.extend(commands::strings(&["--model", selected_model]));
                }
                if let Some(effort) = effort {
                    args.extend(commands::strings(&["--reasoning-effort", effort]));
                }
                let options = Options::parse(&args).unwrap();
                if provider == "antigravity" && selected_model.is_none() {
                    assert!(orchestrator_command(provider, &options).is_err());
                    continue;
                }
                let mut expected = commands::strings(&[
                    program,
                    "--model",
                    selected_model.unwrap_or(model),
                    permission_flag,
                ]);
                if effort.is_some() {
                    expected.extend(if provider == "codex" {
                        commands::strings(&["-c", "model_reasoning_effort=\"xhigh\""])
                    } else {
                        commands::strings(&["--effort", "xhigh"])
                    });
                }
                expected.extend(commands::strings(&[prompt_option, STARTUP_PROMPT]));
                assert_eq!(orchestrator_command(provider, &options).unwrap(), expected);
            }
        }
        for model in [
            None,
            Some("gemini-pro-orchestrator"),
            Some("gemini-pro-orchestrator-argon"),
        ] {
            let mut args = commands::strings(&[
                "--orchestrator",
                "antigravity",
                "--reasoning-effort",
                "high",
            ]);
            if let Some(model) = model {
                args.extend(commands::strings(&["--model", model]));
            }
            let options = Options::parse(&args).unwrap();
            assert!(orchestrator_command("antigravity", &options).is_err());
        }
    }

    #[test]
    fn previous_main_live_provider_prevents_duplicate_native_pane() {
        let mut pane = [
            "$1",
            "@3",
            "%4",
            "123",
            "0",
            "0",
            "",
            "",
            "",
            "",
            "previous-room",
            "codex",
            "previous-main",
            ".",
        ];
        assert_eq!(live_provider(&pane, "$1", "%1"), Some("codex"));
        assert_eq!(live_provider(&pane, "$2", "%1"), None);
        assert_eq!(live_provider(&pane, "$1", "%4"), None);
        pane[11] = "";
        assert_eq!(live_provider(&pane, "$1", "%1"), None);
        pane[11] = "codex";
        pane[4] = "1";
        assert_eq!(live_provider(&pane, "$1", "%1"), None);
    }

    #[test]
    fn codex_status_footer_is_outside_the_draft_check() {
        let status = "\n\x1b[49m  \x1b[38;5;223mGPT-6.2-Sol high\x1b[0m · Context 100% left · weekly 48% left\n← for agents · ? for shortcuts";
        let prompt =
            "\x1b[1m›\x1b[0m\x1b[48;5;235m \x1b[2mAsk Codex to do anything\x1b[0m\x1b[48;5;235m\n";
        assert!(input_ready("codex", &format!("{prompt}{status}")));
        assert!(!input_ready("codex", &format!("› draft\n{status}")));
        assert!(!input_ready(
            "codex",
            &format!("{prompt}draft continued\n{status}")
        ));
    }

    #[test]
    fn terminal_semicolon_is_preserved_as_literal_input() {
        assert_eq!(literal_tmux_arg("before; after;"), r"before; after\;");
        assert_eq!(literal_tmux_arg(r"before\;"), r"before\\;");
        assert_eq!(literal_tmux_arg("before; after"), "before; after");
    }

    #[test]
    fn weekly_quota_uses_the_correct_group_and_direction() {
        let claude = "  Settings   Status   Config   Usage   Stats\n\n  Current week (all models)\n  ███      80% used\n\n  Current week (Fable)\n  ███      82% used";
        let agy = "└ Models & Quota\n\nGEMINI MODELS\n  Weekly Limit Remaining\n   [███] 94.62%\n  Five Hour Limit Remaining\n   [███] 86.37%\nCLAUDE AND GPT MODELS\n  Weekly Limit Remaining\n   [███] 100.00%";
        assert_eq!(weekly_from_usage("claude", claude), Some(80.0));
        assert!((weekly_from_usage("antigravity", agy).unwrap() - 5.38).abs() < 0.001);
        assert_eq!(
            weekly_from_usage("antigravity", &agy.replace("GEMINI MODELS", "")),
            None
        );
        assert_eq!(weekly_from_usage("claude", "7d 79%"), None);
    }

    #[test]
    fn observes_models_from_the_native_status_rows() {
        for (provider, screen, model) in [
            (
                "codex",
                "  GPT-6.2-Sol high · Context 100% left · weekly 54% left",
                "GPT-6.2-Sol",
            ),
            (
                "claude",
                "  Sonnet 5.5 | branch | medium+think | ctx [----] 6%",
                "Sonnet 5.5",
            ),
            (
                "antigravity",
                "Gemini 4 Hydrogen (High) (Antigravity ) | ctx 2% | branch",
                "Gemini 4 Hydrogen (High)",
            ),
        ] {
            assert_eq!(observed_model(provider, screen), Some(model));
        }
        assert_eq!(observed_model("codex", "Probably GPT-6.2-Sol"), None);
    }

    #[test]
    fn native_placeholder_is_distinct_from_a_draft() {
        let agy = "\x1b[38;5;111m>\x1b[39m \x1b[38;5;109mAuto mode: edits & safe commands auto-approved (shift+tab to cycle)\x1b[39m\n────\n";
        assert!(input_ready("antigravity", agy));
        assert!(!input_ready(
            "antigravity",
            &agy.replace("\x1b[38;5;109m", "")
        ));
        assert!(input_ready(
            "claude",
            "❯\u{a0}\x1b[2mTry something\x1b[0m\n────\n"
        ));
        assert!(!input_ready("claude", "❯\u{a0}Work in progress\n────\n"));
    }
}
