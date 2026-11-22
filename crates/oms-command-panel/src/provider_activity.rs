//! Provider activity read from a captured pane: `Some("working")` only when a live status line
//! sits just above the current input prompt, or, for Antigravity, when a native child's
//! `  ● Agent(...)` row follows the prompt or a native timer row; `None` means unknown, never idle.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeActivity {
    Working,
    Command,
    TerminalWait,
    AgentWait,
}

impl NativeActivity {
    pub(crate) fn detail(self) -> Option<&'static str> {
        match self {
            Self::Working => None,
            Self::Command => Some("command"),
            Self::TerminalWait => Some("terminal_wait"),
            Self::AgentWait => Some("agent_wait"),
        }
    }
}

pub(crate) fn observe(provider: &str, screen: &str) -> Option<&'static str> {
    native_activity(provider, screen).map(|_| "working")
}

pub(crate) fn native_activity(provider: &str, screen: &str) -> Option<NativeActivity> {
    type Check = fn(&str) -> bool;
    let (is_prompt, is_status): (Check, Check) = match provider {
        "codex" => (|l| l.starts_with('›'), codex_status),
        "claude" => (|l| l.starts_with('❯'), claude_status),
        "antigravity" => (|l| l == ">" || l.starts_with("> "), agy_status),
        _ => return None,
    };
    let lines: Vec<String> = screen
        .lines()
        .map(|l| strip_sgr(l).trim_end().to_string())
        .collect();
    let prompt = (0..lines.len())
        .rev()
        .filter(|&i| !lines[i].is_empty())
        .take(12)
        .find(|&i| is_prompt(&lines[i]))?;
    let rule = |l: &String| l.chars().all(|c| c == '─');
    let above = 'above: {
        let mut skipped = 0;
        for line in lines[..prompt].iter().rev().filter(|l| !l.is_empty()) {
            if rule(line) || line.trim_start().starts_with("└ Tip:") {
                skipped += 1;
                if skipped > 4 {
                    break 'above None;
                }
                continue;
            }
            break 'above Some(line.as_str()).filter(|line| is_status(line));
        }
        None
    };
    // The child row lives in the bottom chrome and vanishes with the child, unlike the
    // model-authored `Waiting for ...` prose above the prompt.
    let child = provider == "antigravity" && {
        let chrome = &lines[prompt + 1..];
        chrome
            .iter()
            .position(|l| !l.is_empty() && !rule(l))
            .is_some_and(|i| {
                agy_child(&chrome[i])
                    || (agy_timer(&chrome[i]) && chrome.get(i + 1).is_some_and(|l| agy_child(l)))
            })
    };
    if provider == "claude" && claude_advisory_wait(screen, &lines, prompt) {
        return Some(NativeActivity::AgentWait);
    }
    if let Some(status) = above {
        return Some(match provider {
            "codex" if codex_waits_for_terminal(status) => NativeActivity::TerminalWait,
            "antigravity"
                if status
                    .split_once(' ')
                    .is_some_and(|(_, message)| message.trim_start() == "Running command...") =>
            {
                NativeActivity::Command
            }
            _ => NativeActivity::Working,
        });
    }
    child.then_some(NativeActivity::Working)
}

const CLAUDE_WAIT: &str = "✻ Waiting for 1 background agent to finish";
const CLAUDE_ADVISORY_SUFFIX: &str = "% until auto-compact";
const CLAUDE_MODE: &str = "  -- INSERT -- ⏵⏵ bypass permissions on (shift+tab to cycle)";

fn claude_advisory_wait(screen: &str, lines: &[String], prompt: usize) -> bool {
    let Some([wait, _, ruler]) = lines[..prompt].last_chunk::<3>() else {
        return false;
    };
    if wait != CLAUDE_WAIT
        || lines[prompt] != "❯"
        || ruler.is_empty()
        || !ruler.chars().all(|c| c == '─')
    {
        return false;
    }
    // Preserve the advisory's leading spaces and refuse tabs hidden by trim_end.
    let note = strip_sgr(screen.lines().nth(prompt - 2).unwrap_or_default());
    let note = note.trim_end_matches(' ');
    let aligned = note
        .strip_suffix(CLAUDE_ADVISORY_SUFFIX)
        .and_then(|prefix| prefix.as_bytes().split_last())
        .is_some_and(|(digit, padding)| {
            digit.is_ascii_digit()
                && padding.len() >= 2
                && padding.iter().all(|&byte| byte == b' ')
                && ruler
                    .chars()
                    .count()
                    .checked_sub(note.len())
                    .is_some_and(|margin| margin <= 1)
        });
    if !aligned {
        return false;
    }

    // A prompt in scrollback can have the same prelude. Only current footer
    // rows may follow the prompt in this shape.
    let tail_lines = &lines[prompt + 1..];
    let mut tail = tail_lines.iter().filter(|line| !line.is_empty());
    let Some(lower_ruler) = tail.next() else {
        return true;
    };
    if lower_ruler != ruler {
        return false;
    }
    let Some(footer) = tail.next() else {
        return true;
    };
    ((footer == CLAUDE_MODE
        || (footer.starts_with("  ") && tail.next().is_some_and(|line| line == CLAUDE_MODE)))
        && tail.next().is_none())
        || claude_agent_roster(tail_lines, ruler)
}

fn claude_agent_roster(lines: &[String], ruler: &str) -> bool {
    let Some([lower, hud, mode, gap, main, worker]) = lines.first_chunk::<6>() else {
        return false;
    };
    lower == ruler
        && hud.starts_with("  Opus ")
        && hud.contains(" | ctx [")
        && mode.strip_prefix(CLAUDE_MODE) == Some(" · ← 1 agent")
        && gap.is_empty()
        && main == "  ● main"
        && claude_roster_worker(worker)
        && lines[6..].iter().all(String::is_empty)
}

fn claude_roster_worker(line: &str) -> bool {
    if line.len() > 256 {
        return false;
    }
    let fields: Vec<_> = line.splitn(5, " | ").collect();
    let [task, "opus-5-5", context, duration, "running"] = fields.as_slice() else {
        return false;
    };
    let Some(file) = task.strip_prefix("  ◯ Reading ") else {
        return false;
    };
    let file = file.strip_suffix('…').unwrap_or(file);
    if !file.bytes().any(|b| b.is_ascii_alphanumeric())
        || file.len() > 128
        || !file
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-/".contains(&b))
    {
        return false;
    }
    let Some((tokens, percent)) = context
        .strip_prefix("ctx ")
        .and_then(|rest| rest.split_once(' '))
    else {
        return false;
    };
    let Some((used, limit)) = tokens.split_once('/') else {
        return false;
    };
    let Some(elapsed) = duration.strip_suffix('s') else {
        return false;
    };
    let (minutes, seconds) = elapsed
        .split_once('m')
        .map_or((None, elapsed), |(m, s)| (Some(m), s));
    claude_context_count(used)
        .zip(claude_context_count(limit))
        .is_some_and(|(used, limit)| limit > 0 && used <= limit)
        && percent
            .strip_suffix('%')
            .and_then(|n| ascii_counter(n, 3))
            .is_some_and(|n| n <= 100)
        && minutes.is_none_or(|m| ascii_counter(m, 3).is_some_and(|n| n > 0))
        && ascii_counter(seconds, 2).is_some_and(|n| n < 60)
}

fn claude_context_count(value: &str) -> Option<u32> {
    let (digits, scale) = value
        .strip_suffix('m')
        .map(|n| (n, 1_000_000))
        .or_else(|| value.strip_suffix('k').map(|n| (n, 1_000)))
        .unwrap_or((value, 1));
    Some(u32::from(ascii_counter(digits, 3)?) * scale)
}

fn ascii_counter(digits: &str, limit: usize) -> Option<u16> {
    if digits.len() > limit || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

const CODEX_BACKGROUND_SUFFIX: &str =
    " · 1 background terminal running · /ps to view · /stop to close";

fn codex_status(line: &str) -> bool {
    // The live bullet blinks between • and ◦.
    codex_waits_for_terminal(line)
        || line.strip_prefix(['•', '◦']).is_some_and(|status| {
            let status = status
                .strip_suffix(CODEX_BACKGROUND_SUFFIX)
                .unwrap_or(status);
            status.starts_with(" Working (") && status.ends_with("esc to interrupt)")
        })
}

fn codex_waits_for_terminal(line: &str) -> bool {
    line.strip_prefix(['•', '◦'])
        .and_then(|status| status.strip_suffix(CODEX_BACKGROUND_SUFFIX))
        .and_then(|status| status.strip_prefix(" Waiting for background terminal ("))
        .and_then(|elapsed| elapsed.strip_suffix(" • esc to interrupt)"))
        .is_some_and(codex_wait_elapsed)
}

fn codex_wait_elapsed(elapsed: &str) -> bool {
    let Some(elapsed) = elapsed.strip_suffix('s') else {
        return false;
    };
    let (minutes, seconds) = elapsed
        .split_once("m ")
        .map_or((None, elapsed), |(m, s)| (Some(m), s));
    minutes.is_none_or(|m| ascii_counter(m, 3).is_some_and(|n| n > 0))
        && ascii_counter(seconds, 2).is_some_and(|n| n < 60)
}

fn claude_status(l: &str) -> bool {
    let mut c = l.chars();
    if !c.next().is_some_and(|g| "·✢✳✶✻✽*".contains(g))
        || c.next() != Some(' ')
        || !c.next().is_some_and(|u| u.is_ascii_uppercase())
    {
        return false;
    }
    let message = l.strip_prefix(|g: char| "·✢✳✶✻✽*".contains(g)).unwrap();
    if message == " Waiting for 1 background agent to finish" {
        return true;
    }
    if let Some(seconds) = message
        .strip_prefix(" Compacting conversation… (")
        .and_then(|r| r.strip_suffix("s)"))
    {
        return !seconds.is_empty() && seconds.bytes().all(|b| b.is_ascii_digit());
    }
    let rest = c
        .as_str()
        .trim_start_matches(|ch: char| ch.is_ascii_alphabetic());
    rest.strip_prefix("… (")
        .is_some_and(|r| r.starts_with(|d: char| d.is_ascii_digit()))
}

fn agy_status(l: &str) -> bool {
    let mut c = l.chars();
    c.next()
        .is_some_and(|g| ('\u{2801}'..='\u{28FF}').contains(&g))
        && c.as_str().starts_with(' ')
        && !c.as_str().trim_start().is_empty()
}

fn agy_child(l: &str) -> bool {
    agy_child_fields(l).is_some()
}

fn agy_child_fields(l: &str) -> Option<(&str, &str, &str)> {
    let (head, rest) = l.strip_prefix("  ● Agent(")?.split_once(") · ")?;
    let (model, activity) = rest.split_once("  ")?;
    let (kind, label) = head.split_once(": ")?;
    let model_visible = model
        .strip_suffix(')')
        .and_then(|m| m.rsplit_once(" ("))
        .is_some_and(|(name, role)| {
            !name.is_empty() && !role.is_empty() && role.bytes().all(|b| b.is_ascii_alphanumeric())
        })
        || agy_truncated_model(model);
    // AGY can include digits in generated role slugs.
    (kind.len() <= 64
        && kind.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && kind
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric()))
        && !label.trim().is_empty()
        && model_visible
        && !activity.trim().is_empty())
    .then_some((kind, label.trim(), activity.trim()))
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct AgyRosterRow {
    role: &'static str,
    title: String,
    state: &'static str,
    activity: Option<&'static str>,
    elapsed: Option<String>,
}

pub(crate) const AGY_ROSTER_LIMIT: usize = 3;

fn agy_activity_label(activity: &str) -> Option<&'static str> {
    let operation = activity
        .split_once(" · ")
        .map_or(activity, |(head, _)| head);
    [
        ("Reading file", "Reading file"),
        ("Searching", "Searching"),
        ("Editing", "Editing"),
        ("Running command", "Running command"),
        ("Generating", "Generating"),
    ]
    .into_iter()
    .find_map(|(prefix, label)| {
        operation.strip_prefix(prefix).and_then(|suffix| {
            (suffix.is_empty() || suffix.starts_with(' ') || suffix == "...").then_some(label)
        })
    })
}

const AGY_PLACEHOLDER: &str =
    "> Auto mode: edits & safe commands auto-approved (shift+tab to cycle)";

fn agy_divider(line: &str) -> bool {
    let line = line.trim_end();
    line.len() >= "───".len() && line.chars().all(|c| c == '─')
}

fn agy_ctx_field(field: &str) -> bool {
    field
        .strip_prefix("ctx ")
        .and_then(|rest| rest.strip_suffix('%'))
        .is_some_and(|digits| {
            !digits.is_empty()
                && digits.len() <= 3
                && digits.bytes().all(|b| b.is_ascii_digit())
                && (digits.len() == 1 || !digits.starts_with('0'))
                && digits.parse::<u8>().is_ok_and(|pct| pct <= 100)
        })
}

fn agy_hud_fields(hud: &str) -> Option<Vec<&str>> {
    if hud.len() > 512
        || hud.chars().any(char::is_control)
        || hud.ends_with(" |")
        || hud.starts_with("| ")
    {
        return None;
    }
    let fields: Vec<_> = hud.split(" | ").collect();
    if fields.len() < 2 || fields.iter().any(|field| field.trim().is_empty()) {
        return None;
    }
    let head_ok = fields[0]
        .split_once(" (Antigravity")
        .is_some_and(|(model, tier)| !model.is_empty() && tier.ends_with(')'));
    (head_ok && agy_ctx_field(fields[1])).then_some(fields)
}

fn agy_prompt_chrome(lines: &[String]) -> Option<(&[String], &str)> {
    let newest = lines.iter().rposition(|line| line.starts_with('>'))?;
    if !matches!(lines[newest].trim_end(), ">" | AGY_PLACEHOLDER) {
        return None;
    }
    let (middle, hud) = match &lines[newest + 1..] {
        [bottom, hud] if agy_divider(bottom) => (&[][..], hud.trim_end()),
        [top, middle @ .., bottom, hud]
            if !middle.is_empty()
                && middle.len() <= 8
                && agy_divider(top)
                && agy_divider(bottom) =>
        {
            (middle, hud.trim_end())
        }
        _ => return None,
    };
    Some((middle, hud))
}

pub(crate) fn valid_agy_footer_remaining(value: &str) -> bool {
    matches!(value, "0%" | "<1%" | ">99%" | "100%")
        || value.strip_suffix('%').is_some_and(|digits| {
            (1..=2).contains(&digits.len())
                && digits.bytes().all(|b| b.is_ascii_digit())
                && !digits.starts_with('0')
                && digits
                    .parse::<u8>()
                    .is_ok_and(|pct| (1..=99).contains(&pct))
        })
}

/// Read the bounded `Gemini weekly <remaining> left` field from AGY's final
/// intact OMS statusline footer. Display-only: age and reset are unknown.
pub(crate) fn agy_footer_weekly(screen: &str) -> Option<String> {
    if screen.len() > 128 * 1024 {
        return None;
    }
    let lines: Vec<_> = screen
        .lines()
        .map(strip_sgr)
        .filter(|line| !line.trim().is_empty())
        .collect();
    let (middle, hud) = agy_prompt_chrome(&lines)?;
    if !middle.iter().all(|line| {
        let line = line.trim_end();
        agy_timer(line) || agy_clipped_running_job(line) || agy_child_fields(line).is_some()
    }) {
        return None;
    }
    let fields = agy_hud_fields(hud)?;
    let weekly = fields.get(2)?;
    let remaining = weekly
        .strip_prefix("Gemini weekly ")?
        .strip_suffix(" left")?;
    if !valid_agy_footer_remaining(remaining)
        || fields
            .iter()
            .enumerate()
            .any(|(i, field)| i != 2 && field.contains("Gemini weekly"))
    {
        return None;
    }
    Some(remaining.to_owned())
}

/// The current pane's child chrome supplies display text only. A row has no
/// conversation identity and never supplies transcript or action authority.
pub(crate) fn agy_roster(screen: &str) -> Vec<AgyRosterRow> {
    if screen.len() > 128 * 1024 {
        return vec![];
    }
    let lines: Vec<_> = screen
        .lines()
        .map(strip_sgr)
        .filter(|line| !line.trim().is_empty())
        .collect();
    let Some((middle, hud)) = agy_prompt_chrome(&lines) else {
        return vec![];
    };
    let hud_live = hud
        .split_once(" (Antigravity")
        .is_some_and(|(head, suffix)| {
            !head.is_empty() && suffix.contains(") | ctx ") && !hud.chars().any(char::is_control)
        });
    if middle.is_empty() || !hud_live {
        return vec![];
    }
    let rows = middle
        .iter()
        .map(|line| line.trim_end())
        .filter(|line| !agy_timer(line) && !agy_clipped_running_job(line))
        .map(|line| {
            let (kind, title, activity) = agy_child_fields(line)?;
            if title.len() > 512 || title.chars().any(char::is_control) {
                return None;
            }
            let role = match kind {
                "slice-builder" | "builder" => "Builder",
                "research" | "researcher" => "Researcher",
                "review" | "reviewer" => "Reviewer",
                "verify" | "verifier" => "Verifier",
                _ => "Worker",
            };
            let title = title.strip_suffix(&format!(" {role}")).unwrap_or(title);
            let title = oms_panel_core::redact(title.as_bytes()).ok()?;
            let label = agy_activity_label(activity);
            let state = match label {
                Some("Reading file" | "Searching") => "reading",
                Some("Editing") => "editing",
                Some("Generating") => "generating",
                Some("Running command") => "running",
                _ => "working",
            };
            static DURATION: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
                regex::Regex::new(r"^(?:[0-9]{1,3}h)?(?:[0-9]{1,3}m)?(?:[0-9]{1,3}s)?$")
                    .expect("static AGY duration")
            });
            let elapsed = activity
                .rsplit_once(" · ")
                .map(|(_, time)| time)
                .filter(|time| !time.is_empty() && DURATION.is_match(time))
                .map(str::to_owned);
            Some(AgyRosterRow {
                role,
                title,
                state,
                activity: label,
                elapsed,
            })
        })
        .collect::<Option<Vec<_>>>();
    rows.unwrap_or_default()
        .into_iter()
        .take(AGY_ROSTER_LIMIT)
        .collect()
}

#[cfg(test)]
#[test]
fn detailed_activity_comes_from_current_input_chrome() {
    let codex = format!(
        "◦ Waiting for background terminal (7s • esc to interrupt){CODEX_BACKGROUND_SUFFIX}\n› Ask Codex"
    );
    assert_eq!(
        native_activity("codex", &codex),
        Some(NativeActivity::TerminalWait)
    );
    let rule = "────────────────────────";
    let agy = format!("⣟  Running command...\n{rule}\n>\n{rule}");
    assert_eq!(
        native_activity("antigravity", &agy),
        Some(NativeActivity::Command)
    );
    let prose = format!("Previous output: Running command...\n{rule}\n>\n{rule}");
    assert!(native_activity("antigravity", &prose).is_none());
    let stale = format!("{agy}\nSome newer provider output\n>");
    assert!(native_activity("antigravity", &stale).is_none());
    assert_eq!(
        native_activity("codex", "◦ Working (8s • esc to interrupt)\n› Ask Codex"),
        Some(NativeActivity::Working)
    );
}

#[cfg(test)]
#[test]
fn agy_roster_reads_only_the_current_prompt_chrome() {
    let rule = "────────────────────────";
    let roster = format!(
        "prior output\n>\n{rule}\n  ● [12:03:44] 4m timer running\n  ● Agent(slice-builder: Team Check Builder) · Gemini 4 Hydrogen (Orchestrator)  Reading file · 53m17s\n  ● Agent(research: Inspect Ownership) · Gemini 4 Hydrog...  Generating... · 53m1\n{rule}\nOrchestrator (Carbon) (Antigravity ) | ctx 42% | main | oms F18"
    );
    let rows = agy_roster(&roster);
    assert_eq!(rows.len(), 2);
    assert_eq!(agy_roster(&roster.replace("ctx 42%", "ctx …")), rows);
    let mixed = roster.replace(
        "4m timer running\n",
        "4m timer running\n  ● [14:48:20] ( foreground command... running\n",
    );
    assert_eq!(agy_roster(&mixed), rows);
    assert!(agy_roster(&mixed.replace("14:48:20", "26:48:20")).is_empty());
    let clipped = mixed
        .replace("Team Check Builder", "Team (Scoped) Builder")
        .replace(
            "Gemini 4 Hydrogen (Orchestrator)",
            "Gemini 4 Hydrogen (Orch...",
        );
    let clipped = agy_roster(&clipped);
    assert_eq!(clipped.len(), 2);
    assert_eq!(clipped[0].title, "Team (Scoped)");
    assert_eq!(rows[0].role, "Builder");
    assert_eq!(rows[0].title, "Team Check");
    assert_eq!(rows[0].state, "reading");
    assert_eq!(rows[0].elapsed.as_deref(), Some("53m17s"));
    assert_eq!(rows[1].role, "Researcher");
    assert_eq!(rows[1].elapsed, None);

    for (operation, expected, state) in [
        (
            "Reading file secret/path · 12s",
            Some("Reading file"),
            "reading",
        ),
        (
            "Searching secret query · 2m3s",
            Some("Searching"),
            "reading",
        ),
        (
            "Editing files secret/path · 1h2m3s",
            Some("Editing"),
            "editing",
        ),
        (
            "Running command secret args · 4s",
            Some("Running command"),
            "running",
        ),
        ("Generating... · 53m1", Some("Generating"), "generating"),
        ("Reading filefish · 4s", None, "working"),
        ("Running commands · 4s", None, "working"),
        ("Unknown secret prompt · 4s", None, "working"),
    ] {
        let screen = roster.replace("Reading file · 53m17s", operation);
        let row = serde_json::to_value(&agy_roster(&screen)[0]).unwrap();
        assert_eq!(row["activity"].as_str(), expected, "{operation}");
        assert_eq!(row["state"], state, "{operation}");
    }
    let pasted = roster.replace(
        "prior output",
        "  ● Agent(research: Pasted) · Gemini 4 Hydrogen (Orchestrator)  Searching secret",
    );
    assert_eq!(agy_roster(&pasted), rows);

    let generated = roster.replace(
        "slice-builder: Team Check Builder",
        "check270-rosters-builder: Check Live Roster",
    );
    let generated_rows = agy_roster(&generated);
    assert_eq!(generated_rows.len(), 2);
    assert_eq!(generated_rows[0].role, "Worker");
    assert_eq!(generated_rows[0].title, "Check Live Roster");
    assert_eq!(generated_rows[0].state, "reading");
    assert_eq!(observe("antigravity", &generated), Some("working"));

    for unsafe_kind in [
        "2research",
        "research--builder",
        "research-",
        "research_builder",
        "research!",
        "researché",
        "research\t",
        &"r".repeat(65),
    ] {
        let malformed = roster.replace("● Agent(research:", &format!("● Agent({unsafe_kind}:"));
        assert!(agy_roster(&malformed).is_empty(), "{unsafe_kind:?}");
    }
    for stale in [
        format!("{roster}\nNew output after footer"),
        roster.replace(" (Antigravity ) | ctx ", " | ctx "),
        roster.replace("prior output\n>\n", "prior output\n> user draft\n"),
    ] {
        assert!(agy_roster(&stale).is_empty(), "{stale}");
    }
}

#[cfg(test)]
#[test]
fn agy_footer_weekly_reads_only_final_intact_oms_footer() {
    let rule = "────────────────────────";
    let base = |weekly: &str| {
        format!(
            "prior output\n{rule}\n>\n{rule}\nOrchestrator (Carbon) (Antigravity ) | ctx 31% | {weekly} | main | oms F1\n"
        )
    };
    for expected in ["0%", "<1%", "1%", "99%", ">99%", "100%"] {
        let screen = base(&format!("Gemini weekly {expected} left"));
        assert_eq!(
            agy_footer_weekly(&screen).as_deref(),
            Some(expected),
            "{screen}"
        );
    }
    let placeholder = base("Gemini weekly 99% left").replace(
        "\n>\n",
        "\n> Auto mode: edits & safe commands auto-approved (shift+tab to cycle)\n",
    );
    assert_eq!(agy_footer_weekly(&placeholder).as_deref(), Some("99%"));
    let sgr = format!(
        "{rule}\n>\n{rule}\n\x1b[2mOrchestrator (Carbon) (Antigravity ) | ctx 31% | Gemini weekly <1% left | main\x1b[0m\n"
    );
    assert_eq!(agy_footer_weekly(&sgr).as_deref(), Some("<1%"));
    let with_tray = format!(
        "prior\n>\n{rule}\n  ● [12:03:44] 4m timer running\n  ● Agent(research: Check) · Gemini 4 Hydrogen (Orchestrator)  Reading · 12s\n{rule}\nOrchestrator (Carbon) (Antigravity ) | ctx 42% | Gemini weekly >99% left | main\n"
    );
    assert_eq!(agy_footer_weekly(&with_tray).as_deref(), Some(">99%"));

    let ok = base("Gemini weekly 99% left");
    for bad in [
        format!("{ok}New output after footer\n"),
        format!("{ok}>\n{rule}\nOrchestrator (Carbon) (Antigravity ) | ctx 31% | main\n"),
        format!(
            "Gemini weekly 99% left\n{rule}\n>\n{rule}\nOrchestrator (Carbon) (Antigravity ) | ctx 31% | main\n"
        ),
        ok.replace("\n>\n", "\n> Gemini weekly 99% left\n"),
        ok.replace(
            "| main | oms F1",
            "| Gemini weekly 50% left | main | oms F1",
        ),
        ok.replace("oms F1", "oms T1 Gemini weekly 50% left"),
        base("Gemini weekly 00% left"),
        base("Gemini weekly 09% left"),
        base("Gemini weekly 101% left"),
        base("Gemini weekly -1% left"),
        base("Gemini weekly +1% left"),
        base("Gemini weekly 99.5% left"),
        base("Gemini weekly <2% left"),
        base("Gemini weekly >98% left"),
        base("Gemini weekly 99% used"),
        base("Gemini weekly 99% lef"),
        ok.replace(" (Antigravity ) | ctx 31%", " | ctx 31%"),
        ok.replace(" | ctx 31%", ""),
        ok.replace("ctx 31%", "ctx 105%"),
        format!("{ok}\x07"),
        ok.replace("oms F1\n", "oms F1 | \n"),
        format!(
            "prior\n>\n{rule}\nfake prose between dividers\n{rule}\nOrchestrator (Carbon) (Antigravity ) | ctx 42% | Gemini weekly 99% left\n"
        ),
    ] {
        assert_eq!(agy_footer_weekly(&bad), None, "{bad:?}");
    }
}

fn agy_truncated_model(model: &str) -> bool {
    let Some(rest) = model
        .strip_prefix("Gemini ")
        .and_then(|m| m.strip_suffix("..."))
    else {
        return false;
    };
    let Some((version, name)) = rest.split_once(' ') else {
        return false;
    };
    let (base, role_tail) = name.split_once(" (").map_or(
        (name.strip_suffix(' ').unwrap_or(name), None),
        |(base, role)| (base, Some(role)),
    );
    !version.is_empty()
        && version.bytes().all(|b| b.is_ascii_digit())
        && base.len() >= 3
        && base.bytes().all(|b| b.is_ascii_alphabetic())
        && role_tail.is_none_or(|role| role.bytes().all(|b| b.is_ascii_alphanumeric()))
}

fn agy_job_clock(line: &str) -> Option<&str> {
    let (clock, label) = line.strip_prefix("  ● [")?.split_once("] ")?;
    let parts: Vec<_> = clock.split(':').collect();
    (parts.len() == 3
        && parts.iter().enumerate().all(|(i, part)| {
            part.len() == 2
                && part.bytes().all(|b| b.is_ascii_digit())
                && part
                    .parse::<u8>()
                    .is_ok_and(|v| v < if i == 0 { 24 } else { 60 })
        }))
    .then_some(label)
}

fn agy_timer(line: &str) -> bool {
    agy_job_clock(line)
        .and_then(|label| label.strip_suffix("m timer running"))
        .is_some_and(|minutes| !minutes.is_empty() && minutes.bytes().all(|b| b.is_ascii_digit()))
}

fn agy_clipped_running_job(line: &str) -> bool {
    // A clipped foreground command can share the current child's live tray.
    // Recognize its framing without projecting the command text.
    agy_job_clock(line)
        .and_then(|label| label.strip_prefix("( "))
        .and_then(|label| label.strip_suffix("... running"))
        .is_some_and(|label| !label.trim().is_empty() && !label.chars().any(char::is_control))
}

fn strip_sgr(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(i) = rest.find("\x1b[") {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 2..];
        match tail.find(|c: char| !(c.is_ascii_digit() || c == ';')) {
            Some(j) if tail[j..].starts_with('m') => rest = &tail[j + 1..],
            _ => {
                out.push_str("\x1b[");
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::{CLAUDE_MODE, CLAUDE_WAIT, observe};

    const CODEX: &str = r"• Ran git status --short
  └  M crates/x.rs
• Working (13m 53s • esc to interrupt)
  └ Tip: Use /import to bring over your setup.

› Ask Codex to do anything

  GPT-6.2-Sol high · Context 82% left · weekly 41% left · main
  ← for agents · ? for shortcuts
";
    const CODEX_BACKGROUND_WAIT: &str = "• Waiting for background terminal (8m 29s • esc to interrupt) · 1 background terminal running · /ps to view · /stop to close";
    const AGY: &str = r"● Explored 1 file (output.txt)
⣷  Delegating...
└ Tip: Use /cs to search in your codebase.
────────────────────────
>
────────────────────────
Orchestrator (Carbon) (Antigravity ) | ctx 31% | main
";
    const AGY_WAIT: &str = r"● Explored 2 files (a.rs, b.rs)
● Ran 1 subagent

  Waiting for builder subagent (~/x/y) to finish the patch.

────────────────────────
>
────────────────────────
  ● Agent(research: Independent Reviewer) · Gemini 4 Hydrogen (Orchestrator)  Generating...
────────────────────────
Orchestrator (Carbon) (Antigravity ) | ctx 28% | main
";
    const AGY_ROW: &str = "  ● Agent(research: Independent Reviewer) · Gemini 4 Hydrogen (Orchestrator)  Generating...";
    const CLAUDE: &str = "● Checking panes
✢ Deciphering… (23s · ↓ 1.6k tokens · thought for 3s)
────────────────────────
❯ \n────────────────────────
  Opus 5.5 | proj | git main | ctx [##--] 21% | 7d 82%
  -- INSERT -- ⏵⏵ bypass permissions on (shift+tab to cycle)
";

    #[test]
    fn live_status_above_prompt_is_working() {
        assert_eq!(observe("codex", CODEX), Some("working"));
        assert_eq!(observe("antigravity", AGY), Some("working"));
        assert_eq!(observe("claude", CLAUDE), Some("working"));
        let dot = CLAUDE.replace("✢ Deciphering… (23s", "· Deciphering… (17s");
        assert_eq!(observe("claude", &dot), Some("working"));
        let blink = CODEX.replace("• Working", "◦ Working");
        assert_eq!(observe("codex", &blink), Some("working"));
        let sgr = "\x1b[1m• Working (2s • esc to interrupt)\x1b[0m\n\x1b[2m› \x1b[0m";
        assert_eq!(observe("codex", sgr), Some("working"));
    }

    #[test]
    fn native_background_and_compaction_statuses_are_working() {
        for screen in [
            "✽ Compacting conversation… (0s)\n────────────────────────\n❯ \n",
            "✻ Waiting for 1 background agent to finish\n────────────────────────\n❯ \n",
        ] {
            assert_eq!(observe("claude", screen), Some("working"), "{screen}");
        }
        let screen = "• Working (12s • esc to interrupt) · 1 background terminal running · /ps to view · /stop to close\n› \n";
        assert_eq!(observe("codex", screen), Some("working"));
        let waiting = CODEX.replace(
            "• Working (13m 53s • esc to interrupt)",
            CODEX_BACKGROUND_WAIT,
        );
        assert_eq!(observe("codex", &waiting), Some("working"));
        let blink = CODEX_BACKGROUND_WAIT
            .replacen('•', "◦", 1)
            .replace("8m 29s", "7s");
        assert_eq!(
            observe("codex", &format!("\x1b[2m{blink}\x1b[0m\n› \n")),
            Some("working")
        );
        for screen in [
            AGY_WAIT.to_string(),
            AGY_WAIT
                .replace(
                    "research: Independent Reviewer",
                    "panel-core-builder: Lifecycle Builder",
                )
                .replace("Generating...", "Editing files"),
            AGY_WAIT.replace("Generating...", "Reading file x.rs"),
            AGY_WAIT.replace(AGY_ROW, &format!("\x1b[2m{AGY_ROW}\x1b[0m")),
        ] {
            assert_eq!(observe("antigravity", &screen), Some("working"), "{screen}");
        }
    }

    #[test]
    fn claude_wait_with_a_native_auto_compact_advisory_is_working() {
        let wait = "✻ Waiting for 1 background agent to finish";
        for (columns, margin) in [(48, 0), (64, 1), (100, 0)] {
            let ruler = "─".repeat(columns);
            let width = columns - margin;
            let note = format!("{:>width$}", "1% until auto-compact");
            let screen = format!("{wait}\n{note}\n{ruler}\n❯ \n");
            assert_eq!(observe("claude", &screen), Some("working"));
            let mode_only = format!("{screen}{ruler}\n{CLAUDE_MODE}\n");
            assert_eq!(
                super::native_activity("claude", &mode_only),
                Some(super::NativeActivity::AgentWait)
            );
            let footer = format!(
                "{wait}\n\x1b[2m{note} \x1b[0m\n{ruler}\n\x1b[1m❯ \x1b[0m\n{ruler}\n  Opus 5.5 | proj | git main | ctx [##--] 21%\n{CLAUDE_MODE}\n"
            );
            assert_eq!(observe("claude", &footer), Some("working"));
        }
    }

    #[test]
    fn claude_wait_with_one_native_agent_roster_is_working() {
        let ruler = "─".repeat(100);
        let advisory = format!("{:>99}", "1% until auto-compact");
        let screen = format!(
            "✻ Waiting for 1 background agent to finish\n{advisory}\n{ruler}\n❯ \n{ruler}\n  Opus 5.5 | proj | git main | ctx [##--] 21%\n  -- INSERT -- ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 1 agent\n\n  ● main\n  ◯ Reading install-lifecyc… | opus-5-5 | ctx 90k/1m 9% | 1m45s | running\n"
        );
        for live in [
            screen.clone(),
            screen.replace("1% until auto-compact", "6% until auto-compact"),
            screen
                .replace("1% until auto-compact", "7% until auto-compact")
                .replace("Reading install-lifecyc…", "Reading crates/worker.rs")
                .replace("90k/1m 9% | 1m45s", "1k/200k 1% | 35s"),
        ] {
            assert_eq!(observe("claude", &live), Some("working"), "{live}");
        }
        for (from, to) in [
            ("1% until auto-compact", "６% until auto-compact"),
            ("← 1 agent", "← 2 agents"),
            (" · ← 1 agent", ""),
            ("-- INSERT --", "-- NORMAL --"),
            ("⏵⏵ bypass permissions on", "⏵⏵ approve edits first"),
            (
                "  Opus 5.5 | proj | git main | ctx [##--] 21%",
                "  Arbitrary indented output",
            ),
            (" | ctx [", " | unrelated ["),
            ("\n\n  ● main", "\n  ● main"),
            ("  ● main", "  ● quoted-main"),
            ("  ● main\n  ◯", "  ● main\n\n  ◯"),
            ("  ◯ Reading", "  ○ Reading"),
            ("  ◯ Reading", "  ◯ Quoting"),
            ("Reading install-lifecyc…", "Reading …"),
            ("opus-5-5", "quoted-model"),
            ("ctx 90k/1m 9%", "ctx 2m/1m 9%"),
            ("ctx 90k/1m 9%", "ctx 0k/0m 0%"),
            ("ctx 90k/1m 9%", "ctx 90k/1m 109%"),
            ("1m45s", "1m60s"),
            ("1m45s", "9999m1s"),
            ("| running", "| finished"),
            ("| running", "| running | trailing output"),
            ("\n❯ \n", "\n❯ arbitrary draft\n"),
        ] {
            let bad = screen.replace(from, to);
            assert_eq!(observe("claude", &bad), None, "{bad}");
        }
        for stale in [
            format!("{screen}● Subsequent output\n"),
            format!("{screen}  Additional indented output\n"),
            format!("{screen}● Subsequent output\n❯ \n"),
        ] {
            assert_eq!(observe("claude", &stale), None, "{stale}");
        }
        let codex = screen
            .replace(
                "✻ Waiting for 1 background agent to finish",
                "• Working (1s • esc to interrupt)",
            )
            .replace('❯', "›");
        let agy = screen
            .replace(
                "✻ Waiting for 1 background agent to finish",
                "⣷  Delegating...",
            )
            .replace('❯', ">");
        assert_eq!(observe("codex", &codex), None);
        assert_eq!(observe("antigravity", &agy), None);
    }

    #[test]
    fn claude_auto_compact_advisory_does_not_bridge_lookalikes() {
        const ADVISORY: &str = "1% until auto-compact";
        let ruler = "─".repeat(64);
        let note = format!("{ADVISORY:>63}");
        let screen = format!("{CLAUDE_WAIT}\n{note}\n{ruler}\n❯ \n");
        for bad in [
            format!("{note}\n{ruler}\n❯ \n"),
            screen.replace(CLAUDE_WAIT, "✻ Deciphering… (2s)"),
            screen.replace(CLAUDE_WAIT, "❯ ✻ Waiting for 1 background agent to finish"),
            screen.replace(&note, &format!("{note}\n{note}")),
            screen.replace(&note, &format!("\t{note}")),
            screen.replace(&note, &format!("{note}\t")),
            screen.replace(&note, &note[1..]),
            screen.replace(&note, ADVISORY),
            screen.replace(ADVISORY, "11% until auto-compact"),
            screen.replace(ADVISORY, "1% until auto-compact (quoted)"),
            screen.replace(&ruler, ""),
            screen.replace("❯ ", "❯ arbitrary draft"),
            format!("{screen}● New output\n"),
            format!("{screen}● New output\n❯ \n"),
            format!("{screen}{ruler}\n  Arbitrary indented output\n"),
            format!("{screen}{ruler}\n{CLAUDE_MODE}\n● New output\n"),
        ] {
            assert_eq!(observe("claude", &bad), None, "{bad}");
        }
        let codex = screen
            .replace(CLAUDE_WAIT, "• Working (1s • esc to interrupt)")
            .replace('❯', "›");
        let agy = screen
            .replace(CLAUDE_WAIT, "⣷  Delegating...")
            .replace('❯', ">");
        assert_eq!(observe("codex", &codex), None);
        assert_eq!(observe("antigravity", &agy), None);
    }

    #[test]
    fn agy_timer_then_truncated_child_is_working() {
        let timer = "  ● [23:16:11] 4m timer running";
        let child = "  ● Agent(orchestrator-account-builder: Orchestrator Account Binding Builder) · Gemini 4 Hydrog...  Generating · 23m35s";
        let screen = format!(
            "previous message\n>\n────────────────────────\n{timer}\n{child}\n────────────────────────\nOrchestrator (Carbon)\n"
        );
        assert_eq!(observe("antigravity", &screen), Some("working"));
        for bad in [
            screen.replace(child, ""),
            screen.replace(timer, "  ● [23:16] 4m timer running"),
            screen.replace(timer, "  ● [99:16:11] 4m timer running"),
            screen.replace(timer, "  ● [23:16:11] timer running"),
            screen.replace(timer, "arbitrary row"),
            screen.replace(timer, &format!("{timer}\n────────────────────────")),
            screen.replace("Gemini 4 Hydrog...", "Gemini 4 Hydrogen"),
            screen.replace("Gemini 4 Hydrog...", "Other 4 Hydrog..."),
            screen.replace("Gemini 4 Hydrog...", "Gemini 4 ..."),
            screen.replace("Gemini 4 Hydrog...", "Gemini 4 Hydrogen (Fake Role)"),
            screen.replace(": Orchestrator", ":Orchestrator"),
            screen.replace(timer, "Orchestrator (Carbon)"),
        ] {
            assert_eq!(observe("antigravity", &bad), None, "{bad}");
        }
        assert_eq!(observe("codex", &screen.replace(">\n", "› \n")), None);
        assert_eq!(observe("claude", &screen.replace(">\n", "❯ \n")), None);
        assert_eq!(
            observe("antigravity", &format!("{child}\n>\n{timer}\n")),
            None
        );
    }

    #[test]
    fn background_and_compaction_lookalikes_are_unknown() {
        for screen in [
            "✽ Compacting conversation… (0s)\n● Done\n❯ \n",
            "✽ Compacting conversation… (0s) extra\n❯ \n",
            "✻ Waiting for 1 background agent to finish later\n❯ \n",
            "✻ X Waiting for 1 background agent to finish\n❯ \n",
            "✽ Xompacting conversation… (0s)\n❯ \n",
            "Waiting for 1 background agent to finish\n❯ \n",
        ] {
            assert_eq!(observe("claude", screen), None, "{screen}");
        }
        for screen in [
            "• Working (12s • esc to interrupt) · 1 background terminal running · /ps to view · /stop to close\n• Done\n› \n",
            "• Working (12s • esc to interrupt) · 1 background terminal running · /ps to view · /stop to close later\n› \n",
        ] {
            assert_eq!(observe("codex", screen), None, "{screen}");
        }
        for (from, to) in [
            ("8m 29s", "8m 60s"),
            ("8m 29s", "60s"),
            ("8m 29s", "8m"),
            ("8m 29s", "+8m 29s"),
            ("8m 29s", "８m 29s"),
            ("8m 29s", "1000m 29s"),
            ("8m 29s", "8m  29s"),
            (" • esc to interrupt", " · esc to interrupt"),
            (
                "1 background terminal running",
                "2 background terminals running",
            ),
            (
                " · 1 background terminal running · /ps to view · /stop to close",
                "",
            ),
            ("/stop to close", "/stop to close later"),
        ] {
            let line = CODEX_BACKGROUND_WAIT.replace(from, to);
            let screen = format!("{line}\n› \n");
            assert_eq!(observe("codex", &screen), None, "{screen}");
        }
    }

    #[test]
    fn stale_scrollback_is_unknown() {
        let codex = CODEX.replace("  └ Tip", "• Ran ls\n  └ Tip");
        assert_eq!(observe("codex", &codex), None);
        for screen in [
            format!("{CODEX_BACKGROUND_WAIT}\n• Done\n› \n"),
            format!("{CODEX_BACKGROUND_WAIT}\n› Older prompt\n• Done\n› Current prompt\n"),
            format!(
                "{CODEX_BACKGROUND_WAIT}\n› Older prompt\n{}",
                "out\n".repeat(12)
            ),
        ] {
            assert_eq!(observe("codex", &screen), None, "{screen}");
        }
        let agy = AGY.replace("└ Tip", "● Done\n└ Tip");
        assert_eq!(observe("antigravity", &agy), None);
        let claude = CLAUDE.replacen("──", "● Done\n──", 1);
        assert_eq!(observe("claude", &claude), None);
        let gone = AGY_WAIT.replace(&format!("{AGY_ROW}\n────────────────────────\n"), "");
        assert_eq!(observe("antigravity", &gone), None);
        let above = format!("{AGY_ROW}\n────────────────────────\n>\n");
        assert_eq!(observe("antigravity", &above), None);
    }

    #[test]
    fn lookalikes_are_unknown() {
        for screen in [
            "● x\n› • Working (1s • esc to interrupt)\n",
            "• Ran x\n› \n• Working (1s • esc to interrupt)\n",
            "• Ran x\n  • Working (1s • esc to interrupt)\n› hi\n",
            "› • Working (1s • esc to interrupt)\n• Ran x\n› Ask Codex\n",
            "• Working (5s)\n› x\n",
        ] {
            assert_eq!(observe("codex", screen), None, "{screen}");
        }
        for draft in [
            format!("› {CODEX_BACKGROUND_WAIT}\n"),
            format!("› draft\n  {CODEX_BACKGROUND_WAIT}\n› Current draft\n"),
        ] {
            assert_eq!(observe("codex", &draft), None, "{draft}");
        }
        assert_eq!(observe("claude", "> ✢ Deciphering… (2s)\n❯ \n"), None);
        assert_eq!(observe("claude", "> ✢ Deciphering… (2s)\n● Ok\n❯\n"), None);
        assert_eq!(observe("claude", "✻ Worked for 3m 2s\n❯ \n"), None);
        assert_eq!(observe("antigravity", "⣷\n>\n"), None);
        let after_footer = format!("Orchestrator (Carbon)\n{AGY_ROW}");
        let past_bound = format!("  x\n  y\n{AGY_ROW}");
        for (from, to) in [
            ("  Generating...", "  "),
            ("  Generating...", " Generating..."),
            (" (Orchestrator)", ""),
            (" (Orchestrator)", " ()"),
            ("research: Independent Reviewer", "research"),
            ("research: ", ": "),
            ("research: ", "-research: "),
            ("research: ", "research-: "),
            ("research: ", "panel--builder: "),
            ("research: ", "panel_builder: "),
            ("research: ", "panel.builder: "),
            ("research: ", "1panel: "),
            (AGY_ROW, "  ● Explored 2 files"),
            (AGY_ROW, &after_footer),
            (AGY_ROW, &past_bound),
        ] {
            let screen = AGY_WAIT.replace(from, to);
            assert_eq!(observe("antigravity", &screen), None, "{screen}");
        }
    }

    #[test]
    fn missing_prompt_or_provider_is_unknown() {
        let pushed = format!(
            "• Working (1s • esc to interrupt)\n› x\n{}",
            "out\n".repeat(12)
        );
        assert_eq!(observe("codex", &pushed), None);
        let walls = format!(
            "• Working (1s • esc to interrupt)\n{}› x\n",
            "──\n".repeat(5)
        );
        assert_eq!(observe("codex", &walls), None);
        assert_eq!(observe("claude", CODEX), None);
        assert_eq!(observe("codex", AGY), None);
        assert_eq!(observe("codex", &format!("› \n{AGY_ROW}\n")), None);
        assert_eq!(observe("claude", &format!("❯ \n{AGY_ROW}\n")), None);
        assert_eq!(observe("gemini", CODEX), None);
        assert_eq!(observe("codex", ""), None);
    }
}
