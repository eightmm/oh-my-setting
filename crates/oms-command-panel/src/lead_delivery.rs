//! Bounded input delivery evidence for ordinary managed AGY and Claude leads.
use super::{AccountHost, CHAT_SGR, PaneIdentity, Panel, input_ready};
use crate::{Error, Result, require};
use oms_panel_core::{Store, Transaction, terminals::SessionProof};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_RECEIPT: usize = 8 * 1024;
const MAX_FRAME: usize = 64 * 1024;
const MAX_ROWS: usize = 160;
pub(super) const FRAME: &str = concat!(
    "#{pane_id}\t#{pane_dead}\t#{pane_in_mode}\t#{cursor_x}\t#{cursor_y}\t",
    "#{pane_width}\t#{pane_height}\t.",
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Delivery {
    Unverified,
    DraftRetained,
    InputClearedUnverified,
    TurnObserved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Outcome {
    pub(super) delivery: Delivery,
    pub(super) reason: Option<&'static str>,
}

impl Outcome {
    pub(super) fn uncertain(reason: &'static str) -> Self {
        Self {
            delivery: Delivery::Unverified,
            reason: Some(reason),
        }
    }

    pub(super) fn reconcile_cue(self) -> &'static str {
        self.reason.unwrap_or(match self.delivery {
            Delivery::DraftRetained => "The draft is still visible. Inspect the provider pane before resending.",
            Delivery::InputClearedUnverified => "Input cleared without verified provider acceptance. Inspect the provider pane before resending.",
            Delivery::Unverified => "Provider acceptance could not be verified. Inspect the provider pane before resending.",
            Delivery::TurnObserved => "The turn was observed but its receipt could not be updated. Inspect the provider pane before resending.",
        })
    }
}

impl From<Delivery> for Outcome {
    fn from(delivery: Delivery) -> Self {
        Self {
            delivery,
            reason: None,
        }
    }
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Scope {
    session: SessionProof,
    room: String,
    attempt: String,
    member: String,
    provider: String,
    main: PaneIdentity,
    lead: PaneIdentity,
}

impl Scope {
    pub(super) fn sample(
        panel: &Panel<'_>,
        provider: &str,
        host: &dyn AccountHost,
    ) -> Result<Self> {
        let lead = panel
            .leads
            .get(provider)
            .ok_or_else(|| Error::new("provider pane unavailable"))?;
        Ok(Self {
            session: panel.session_proof.clone(),
            room: panel.room.into(),
            attempt: panel.attempt.clone(),
            member: panel.participant.clone(),
            provider: provider.into(),
            main: PaneIdentity::sample_with_host(&panel.main, host, true)?,
            lead: PaneIdentity::sample_with_host(lead, host, true)?,
        })
    }

    pub(super) fn proves_agy_turn(&self, repo: &Path, turn: &str, prompt: &str) -> bool {
        self.provider == "antigravity"
            && crate::native_children::agy_submitted_user_turn(
                self.lead.pid,
                &self.lead.birth,
                repo,
                turn,
                prompt,
            )
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    pub(super) scope: Scope,
    turn_id: String,
    pub(super) delivery: Delivery,
}

impl Receipt {
    pub(super) fn new(scope: Scope, turn: &str) -> Self {
        Self {
            scope,
            turn_id: turn.into(),
            delivery: Delivery::Unverified,
        }
    }

    pub(super) fn write(&self, tx: &Transaction<'_>, provider: &str) -> Result<()> {
        let path =
            receipt_path(provider).ok_or_else(|| Error::new("invalid lead receipt provider"))?;
        tx.write_json(
            &path,
            &serde_json::to_value(self).map_err(Error::new)?,
            false,
            MAX_RECEIPT,
        )?;
        Ok(())
    }

    pub(super) fn for_live_lead(
        store: &Store,
        panel: &Panel<'_>,
        provider: &str,
        host: &dyn AccountHost,
    ) -> Option<Delivery> {
        let raw = store
            .json(&receipt_path(provider)?, MAX_RECEIPT, true)
            .ok()??;
        let receipt: Self = serde_json::from_value(raw).ok()?;
        (receipt.scope == Scope::sample(panel, provider, host).ok()?
            && receipt.turn_id.len() == 20
            && receipt
                .turn_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .then_some(receipt.delivery)
    }
}

fn receipt_path(provider: &str) -> Option<PathBuf> {
    matches!(provider, "claude" | "antigravity")
        .then(|| format!(".oms/artifacts/native-lead-delivery/{provider}.json").into())
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Cursor {
    x: usize,
    row: usize,
    width: usize,
    height: usize,
}

#[derive(PartialEq, Eq)]
pub(super) struct Draft {
    cursor: Cursor,
    start: usize,
    rows: Vec<String>,
}

pub(super) struct Frame {
    rows: Vec<String>,
    cursor: Cursor,
}

impl Frame {
    fn read(
        tmux: &mut dyn FnMut(&[&str]) -> Result<String>,
        pane: &str,
        capture: &[&str],
    ) -> Result<(Cursor, String)> {
        let mut argv = vec![
            "display-message",
            "-p",
            "-t",
            pane,
            FRAME,
            ";",
            "capture-pane",
            "-p",
            "-e",
        ];
        argv.extend_from_slice(capture);
        // Keep trailing input spaces intact through the tmux adapter's stdout trim.
        argv.extend_from_slice(&["-t", pane, ";", "display-message", "-p", "-t", pane, "."]);
        let output = tmux(&argv)?;
        require(
            output.len() <= MAX_FRAME,
            "lead delivery viewport too large",
        )?;
        let (head, screen) = output
            .split_once('\n')
            .and_then(|(head, body)| Some((head, body.strip_suffix("\n.")?)))
            .ok_or_else(|| Error::new("lead delivery viewport unavailable"))?;
        let fields: Vec<_> = head.split('\t').collect();
        let [found, "0", "0", x, row, width, height, "."] = fields.as_slice() else {
            return Err(Error::new("lead delivery viewport changed"));
        };
        let cursor = Cursor {
            x: x.parse().map_err(Error::new)?,
            row: row.parse().map_err(Error::new)?,
            width: width.parse().map_err(Error::new)?,
            height: height.parse().map_err(Error::new)?,
        };
        require(
            *found == pane
                && cursor.height <= MAX_ROWS
                && cursor.row < cursor.height
                && cursor.x < cursor.width,
            "lead delivery viewport changed",
        )?;
        Ok((cursor, screen.to_owned()))
    }

    pub(super) fn capture(
        tmux: &mut dyn FnMut(&[&str]) -> Result<String>,
        pane: &str,
    ) -> Result<Self> {
        let (cursor, screen) = Self::read(tmux, pane, &["-S", "0"])?;
        let rows: Vec<_> = screen.lines().map(str::to_owned).collect();
        require(
            rows.len() <= cursor.height && cursor.row < rows.len(),
            "lead delivery viewport changed",
        )?;
        Ok(Self { rows, cursor })
    }

    pub(super) fn exact_draft(
        tmux: &mut dyn FnMut(&[&str]) -> Result<String>,
        pane: &str,
        provider: &str,
        expected: &str,
    ) -> Option<Draft> {
        let frame = Self::capture(tmux, pane).ok()?;
        let draft = frame.draft(provider)?;
        let cursor = draft.cursor;
        // One logical line, using tmux's wrap metadata and preserving even
        // single-row trailing spaces. App-painted rows can hide separators.
        let start = draft.start.to_string();
        let end = cursor.row.to_string();
        let (checked, joined) = Self::read(tmux, pane, &["-J", "-S", &start, "-E", &end]).ok()?;
        if checked != cursor {
            return None;
        }
        let line = CHAT_SGR.replace_all(&joined, "");
        let cells = crate::render::width(&line);
        (!line.contains(['\r', '\n'])
            && !expected.is_empty()
            && composer_body(provider, &line) == Some(expected)
            && cells / cursor.width == cursor.row - draft.start
            && cells % cursor.width == cursor.x)
            .then_some(draft)
    }

    fn draft(&self, provider: &str) -> Option<Draft> {
        let rows = &self.rows[..=self.cursor.row];
        let start = rows
            .iter()
            .rposition(|line| composer_body(provider, &CHAT_SGR.replace_all(line, "")).is_some())?;
        let top = CHAT_SGR.replace_all(self.rows.get(start.checked_sub(1)?)?, "");
        let bottom = CHAT_SGR.replace_all(self.rows.get(self.cursor.row + 1)?, "");
        // The two provider composer rules distinguish the last editable row
        // from an earlier cursor position with unverified input below it.
        let rule = top.trim_end_matches(' ');
        if rule != bottom.trim_end_matches(' ')
            || rule.chars().count() < 3
            || !rule.chars().all(|cell| cell == '─')
        {
            return None;
        }
        Some(Draft {
            cursor: self.cursor,
            start,
            rows: rows[start..].to_vec(),
        })
    }

    pub(super) fn shows_turn(&self, turn: &str) -> bool {
        self.rows
            .iter()
            .any(|line| CHAT_SGR.replace_all(line, "").contains(turn))
    }

    pub(super) fn delivery(&self, provider: &str, prior_draft: Option<&Draft>) -> Delivery {
        if self.empty(provider) {
            return Delivery::InputClearedUnverified;
        }
        if prior_draft.is_some() && self.draft(provider).as_ref() == prior_draft {
            return Delivery::DraftRetained;
        }
        Delivery::Unverified
    }

    fn empty(&self, provider: &str) -> bool {
        let line = CHAT_SGR.replace_all(&self.rows[self.cursor.row], "");
        self.cursor.x == 2
            && (composer_body(provider, &line).is_some()
                || matches!(
                    (provider, line.as_ref()),
                    ("antigravity", ">") | ("claude", "❯")
                ))
            && input_ready(provider, &self.rows[self.cursor.row..].join("\n"))
    }
}

fn composer_body<'a>(provider: &str, line: &'a str) -> Option<&'a str> {
    match provider {
        "antigravity" => line.strip_prefix("> "),
        "claude" => line
            .strip_prefix("❯ ")
            .or_else(|| line.strip_prefix("❯\u{a0}")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::{SystemAccountHost, codex_chat_fixture::BoardSpy};

    #[test]
    fn ordinary_lead_delivery_receipt_requires_live_generation() {
        let observed = BoardSpy::new("delivery");
        observed.provider.set("antigravity");
        let panel = observed.panel();
        let store = Store::open(panel.repo).unwrap();
        let tx = store
            .transaction("panel-provider-sessions", std::time::Duration::from_secs(1))
            .unwrap();
        let host = &SystemAccountHost;
        let mut receipt = Receipt::new(
            Scope::sample(&panel, "antigravity", host).unwrap(),
            "a0123b0123c0123d0123",
        );
        receipt.write(&tx, "antigravity").unwrap();
        assert_eq!(
            Receipt::for_live_lead(&store, &panel, "antigravity", host),
            Some(Delivery::Unverified),
        );
        receipt.delivery = Delivery::TurnObserved;
        receipt.write(&tx, "antigravity").unwrap();
        assert_eq!(
            Receipt::for_live_lead(&store, &panel, "antigravity", host),
            Some(Delivery::TurnObserved),
        );
        observed.lead.set("%4");
        assert!(Receipt::for_live_lead(&store, &observed.panel(), "antigravity", host).is_none());
        observed.lead.set("%3");
        receipt.scope.lead.birth = "stale-process".into();
        receipt.write(&tx, "antigravity").unwrap();
        assert!(Receipt::for_live_lead(&store, &panel, "antigravity", host).is_none());
        receipt.scope = Scope::sample(&panel, "antigravity", host).unwrap();
        receipt.scope.room = "room-foreign".into();
        receipt.write(&tx, "antigravity").unwrap();
        assert!(Receipt::for_live_lead(&store, &panel, "antigravity", host).is_none());
    }
}
