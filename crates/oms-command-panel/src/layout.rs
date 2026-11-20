use crate::{Error, Result, require};

pub const BOARD_SHARE: usize = 28;
pub const BOARD_HEIGHT_SHARE: usize = 45;
pub const NATIVE_MIN: usize = 40;
pub const MIN_BOARD: usize = 20;
const TOP_MIN_ROWS: usize = 17;
const MAIN_MIN_ROWS: usize = 12;
const MIN_DOCK_ROWS: usize = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Auto,
    Summary,
    Compact,
    Detail,
    Graph,
    Tree,
}
impl View {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "auto" => Ok(Self::Auto),
            "summary" => Ok(Self::Summary),
            "compact" => Ok(Self::Compact),
            "detail" => Ok(Self::Detail),
            "graph" => Ok(Self::Graph),
            "tree" => Ok(Self::Tree),
            _ => Err(Error::new("invalid panel view")),
        }
    }
    pub fn resolved(self, width: usize, height: usize) -> Self {
        if self != Self::Auto {
            return self;
        }
        if width < 40 || height < 12 {
            Self::Summary
        } else if width < 100 {
            Self::Compact
        } else {
            Self::Detail
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::Auto => Self::Compact,
            _ => Self::Auto,
        }
    }
}

pub fn split_cells(stored: Option<usize>, total: usize) -> usize {
    if total < MIN_BOARD + NATIVE_MIN {
        return total.saturating_sub(NATIVE_MIN).max(1);
    }
    stored
        .unwrap_or(total * BOARD_SHARE / 100)
        .clamp(MIN_BOARD, total - NATIVE_MIN)
}
pub fn parse_split(value: &str) -> Result<usize> {
    let value = value
        .parse::<usize>()
        .map_err(|_| Error::new("invalid panel split"))?;
    require((1..=10000).contains(&value), "panel split out of range")?;
    Ok(value)
}
pub fn dimensions() -> (usize, usize) {
    rustix::termios::tcgetwinsize(std::io::stdout())
        .ok()
        .filter(|v| v.ws_col > 0 && v.ws_row > 0)
        .map(|v| (usize::from(v.ws_col), usize::from(v.ws_row)))
        .unwrap_or((80, 24))
}

const PANE_LAYOUT: &str = concat!(
    "#{pane_id}\t#{pane_pid}\t#{pane_active}\t#{pane_dead}\t",
    "#{pane_left}\t#{pane_top}\t#{pane_width}\t#{pane_height}\t",
    "#{window_width}\t#{window_height}\t#{window_zoomed_flag}\t#{window_id}\t",
    "#{session_name}\t#{@oms_panel_main_watch}\t#{@oms_panel_main_attempt}\t",
    "#{@oms_panel_native_pane}\t#{@oms_panel_owner}\t#{@oms_panel_room}\t",
    "#{@oms_panel_repo}\t#{@oms_provider_session}\t#{@oms_provider_parent}\t.",
);

/// Reflow the managed board and conversations in the same Main window.
pub(crate) fn reflow(
    repo: &std::path::Path,
    room: &str,
    services: &dyn crate::Services,
    stop: &std::sync::atomic::AtomicBool,
) {
    let Some((pane, session)) = std::env::var("TMUX_PANE")
        .ok()
        .zip(std::env::var("OMS_PANEL_SESSION").ok())
    else {
        return;
    };
    if room.is_empty() || !crate::commands::valid_target(&pane, '%') {
        return;
    }
    if let Some(repo_text) = repo.to_str() {
        reflow_with(
            &pane,
            std::process::id(),
            &session,
            room,
            repo_text,
            |args| {
                oms_panel_core::terminals::tmux(
                    services,
                    repo,
                    &crate::commands::strings(args),
                    stop,
                )
            },
        );
    }
}

fn reflow_with(
    caller: &str,
    caller_pid: u32,
    session: &str,
    room: &str,
    repo: &str,
    mut tmux: impl FnMut(&[&str]) -> Result<String>,
) {
    let inventory = ["list-panes", "-t", caller, "-F", PANE_LAYOUT];
    let Ok(raw) = tmux(&inventory) else {
        return;
    };
    let Some(panes) = raw.lines().map(Pane::parse).collect::<Option<Vec<_>>>() else {
        return;
    };
    let Some(board) = panes
        .iter()
        .find(|pane| pane.id == caller && pane.pid == caller_pid && pane.board)
    else {
        return;
    };
    let Some(main) = panes.iter().find(|pane| pane.id == board.main) else {
        return;
    };
    let mut active = panes.iter().filter(|pane| pane.active);
    let Some(focused) = active.next() else {
        return;
    };
    if !(2..=3).contains(&panes.len())
        || active.next().is_some()
        || main.board
        || !board.provider.is_empty()
        || !main.provider.is_empty()
        || !board.provider_parent.is_empty()
        || !main.provider_parent.is_empty()
        || panes.iter().enumerate().any(|(index, pane)| {
            pane.session != session
                || pane.room != room
                || pane.repo != repo
                || pane.window != board.window
                || pane.canvas != board.canvas
                || pane.attempt != board.attempt
                || pane.main != main.id
                || pane.owner != board.owner
                || panes[..index]
                    .iter()
                    .any(|other| other.id == pane.id || other.pid == pane.pid)
        })
    {
        return;
    }
    let dock = panes
        .iter()
        .find(|pane| pane.id != board.id && pane.id != main.id);
    if dock.is_some_and(|pane| {
        pane.board
            || !crate::orchestrator::PROVIDERS.contains(&pane.provider)
            || pane.provider_parent != board.attempt
    }) {
        return;
    }
    let Some((layout, desired)) = plan_layout(board, main, dock) else {
        return;
    };

    let mut args = vec!["select-layout", "-t", board.window, &layout];
    let mut order: Vec<_> = panes.iter().map(|pane| pane.id).collect();
    let mut swapped = false;
    // tmux assigns custom layout leaves in pane-list order, even with leaf IDs.
    for (index, id) in desired.into_iter().enumerate() {
        if order[index] != id {
            let Some(other) = order.iter().position(|pane| *pane == id) else {
                return;
            };
            args.extend([";", "swap-pane", "-d", "-s", order[index], "-t", id]);
            order.swap(index, other);
            swapped = true;
        }
    }
    if swapped {
        args.extend([";", "select-pane", "-t", focused.id]);
    }
    // A pane can be closed, docked, or replaced while the layout is computed.
    if tmux(&inventory).is_ok_and(|current| current == raw) {
        let _ = tmux(&args);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bounds {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

impl Bounds {
    fn cell(self) -> String {
        format!("{}x{},{},{}", self.width, self.height, self.x, self.y)
    }

    fn columns(self, first: usize) -> Option<(Self, Self)> {
        let second = self.width.checked_sub(first + 1)?;
        (first > 0 && second > 0).then_some((
            Self {
                width: first,
                ..self
            },
            Self {
                x: self.x + first + 1,
                width: second,
                ..self
            },
        ))
    }

    fn rows(self, first: usize) -> Option<(Self, Self)> {
        let second = self.height.checked_sub(first + 1)?;
        (first > 0 && second > 0).then_some((
            Self {
                height: first,
                ..self
            },
            Self {
                y: self.y + first + 1,
                height: second,
                ..self
            },
        ))
    }
}

struct Pane<'a> {
    id: &'a str,
    pid: u32,
    active: bool,
    bounds: Bounds,
    canvas: Bounds,
    window: &'a str,
    session: &'a str,
    board: bool,
    attempt: &'a str,
    main: &'a str,
    owner: &'a str,
    room: &'a str,
    repo: &'a str,
    provider: &'a str,
    provider_parent: &'a str,
}

impl<'a> Pane<'a> {
    fn parse(row: &'a str) -> Option<Self> {
        let fields: Vec<_> = row.split('\t').collect();
        let [
            id,
            pid,
            active @ ("0" | "1"),
            "0",
            x,
            y,
            width,
            height,
            win_width,
            win_height,
            "0",
            window,
            session,
            board @ ("" | "0" | "1"),
            attempt,
            main,
            owner,
            room,
            repo,
            provider,
            provider_parent,
            ".",
        ] = fields.as_slice()
        else {
            return None;
        };
        let valid = crate::commands::valid_target;
        let cell = |value: &str| value.parse::<u16>().ok().map(usize::from);
        let pid = pid.parse().ok()?;
        let bounds = Bounds {
            x: cell(x)?,
            y: cell(y)?,
            width: cell(width)?,
            height: cell(height)?,
        };
        let canvas = Bounds {
            x: 0,
            y: 0,
            width: cell(win_width)?,
            height: cell(win_height)?,
        };
        (valid(id, '%')
            && valid(main, '%')
            && valid(window, '@')
            && pid > 0
            && !attempt.is_empty()
            && !owner.is_empty()
            && bounds.width > 0
            && bounds.height > 0
            && bounds.x + bounds.width <= canvas.width
            && bounds.y + bounds.height <= canvas.height)
            .then_some(Self {
                id,
                pid,
                active: *active == "1",
                bounds,
                canvas,
                window,
                session,
                board: *board == "1",
                attempt,
                main,
                owner,
                room,
                repo,
                provider,
                provider_parent,
            })
    }
}

pub(crate) fn wide_board_rows(width: usize, height: usize) -> Option<usize> {
    // The existing native chat dock needs at least 17 rows in the board.
    (width >= 100 && width >= height.saturating_mul(2) && height > TOP_MIN_ROWS + MAIN_MIN_ROWS)
        .then(|| ((height - 1) * BOARD_SHARE / 100).clamp(TOP_MIN_ROWS, height - MAIN_MIN_ROWS - 1))
}

fn plan_layout<'a>(
    board: &Pane<'a>,
    main: &Pane<'a>,
    dock: Option<&Pane<'a>>,
) -> Option<(String, Vec<&'a str>)> {
    let canvas = board.canvas;
    let (w, h) = (canvas.width, canvas.height);
    let (board_actual, main_actual) = (board.bounds, main.bounds);
    let board_left = if dock.is_some() {
        w > MIN_BOARD + NATIVE_MIN && h > MIN_DOCK_ROWS * 2
    } else {
        !board_below(w, h)
    };
    let mut slots = Vec::with_capacity(3);
    let mut leaf = |bounds: Bounds, pane: &Pane<'a>| {
        slots.push(pane.id);
        format!("{},{}", bounds.cell(), &pane.id[1..])
    };
    let body = if let Some(rows) = wide_board_rows(w, h) {
        if board_actual.y == 0
            && board_actual.width == w
            && main_actual.x == 0
            && board_actual.y < main_actual.y
            && dock
                .is_none_or(|pane| pane.bounds.y == main_actual.y && pane.bounds.x > main_actual.x)
        {
            return None;
        }
        let (top, work) = canvas.rows(rows)?;
        let top = leaf(top, board);
        let work = if let Some(dock) = dock {
            let (left, right) = work.columns((w - 1) / 2)?;
            format!(
                "{}{{{},{}}}",
                work.cell(),
                leaf(left, main),
                leaf(right, dock)
            )
        } else {
            leaf(work, main)
        };
        format!("{}[{top},{work}]", canvas.cell())
    } else if board_left {
        if board_actual.x == 0
            && board_actual.y == 0
            && main_actual.y == 0
            && main_actual.x > 0
            && dock.is_none_or(|pane| {
                pane.bounds.x == board_actual.x
                    && pane.bounds.width == board_actual.width
                    && pane.bounds.y > board_actual.y
            })
        {
            return None;
        }
        let (left, right) = canvas.columns(split_cells(None, w.checked_sub(1)?))?;
        let left = if let Some(dock) = dock {
            let (top, bottom) = left.rows((h - 1) / 2)?;
            format!(
                "{}[{},{}]",
                left.cell(),
                leaf(top, board),
                leaf(bottom, dock)
            )
        } else {
            leaf(left, board)
        };
        format!("{}{{{left},{}}}", canvas.cell(), leaf(right, main))
    } else {
        if board_actual.width == w
            && main_actual.y == 0
            && board_actual.y > 0
            && dock.is_none_or(|pane| pane.bounds.width == w && pane.bounds.y > board_actual.y)
        {
            return None;
        }
        if let Some(dock) = dock {
            if h < MIN_DOCK_ROWS * 3 + 2 {
                return None;
            }
            let (top, bottom) = canvas.rows((h - 2) / 3)?;
            let (middle, last) = bottom.rows((bottom.height - 1) / 2)?;
            format!(
                "{}[{},{},{}]",
                canvas.cell(),
                leaf(top, main),
                leaf(middle, board),
                leaf(last, dock)
            )
        } else {
            let (top, bottom) = canvas.rows(h - h * BOARD_HEIGHT_SHARE / 100 - 1)?;
            format!(
                "{}[{},{}]",
                canvas.cell(),
                leaf(top, main),
                leaf(bottom, board)
            )
        }
    };
    let checksum = body.bytes().fold(0u16, |sum, byte| {
        sum.rotate_right(1).wrapping_add(u16::from(byte))
    });
    Some((format!("{checksum:04x},{body}"), slots))
}

fn board_below(width: usize, height: usize) -> bool {
    height >= 20 && (width < 100 || width < height.saturating_mul(2))
}

#[cfg(test)]
mod tests {
    use super::{PANE_LAYOUT, Pane, reflow_with, wide_board_rows};
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn tab_switches_between_overview_and_all_tasks() {
        use super::View;
        assert_eq!(View::Auto.next(), View::Compact);
        assert_eq!(View::Compact.next(), View::Auto);
        for view in [View::Summary, View::Detail, View::Graph, View::Tree] {
            assert_eq!(view.next(), View::Auto);
        }
        assert_eq!(View::parse("graph").unwrap(), View::Graph);
        assert_eq!(View::parse("detail").unwrap(), View::Detail);
    }
    #[test]
    fn orientation_leaves_room_for_main_and_board() {
        assert_eq!(wide_board_rows(316, 90), Some(24));
        assert_eq!(wide_board_rows(100, 30), Some(17));
        assert_eq!(wide_board_rows(100, 29), None);
        assert_eq!(wide_board_rows(140, 130), None);
        assert!(!super::board_below(220, 70));
        assert!(super::board_below(140, 130));
        assert!(super::board_below(80, 30));
        assert!(!super::board_below(80, 18));
    }

    #[test]
    fn tmux_reflow_preserves_processes_focus_and_foreign_layouts() {
        if !Command::new("tmux")
            .arg("-V")
            .output()
            .is_ok_and(|out| out.status.success())
        {
            return;
        }

        struct Server(String);
        impl Server {
            fn run(&self, args: &[&str]) -> crate::Result<String> {
                let out = Command::new("tmux")
                    .args(["-L", &self.0, "-f", "/dev/null"])
                    .args(args)
                    .env_remove("TMUX")
                    .env_remove("TMUX_PANE")
                    .output()
                    .map_err(crate::Error::new)?;
                if !out.status.success() {
                    return Err(crate::Error::new(String::from_utf8_lossy(&out.stderr)));
                }
                Ok(String::from_utf8(out.stdout)
                    .map_err(crate::Error::new)?
                    .trim()
                    .into())
            }
        }
        impl Drop for Server {
            fn drop(&mut self) {
                let _ = self.run(&["kill-server"]);
            }
        }

        let server = Server(format!("oms-reflow-{}", std::process::id()));
        let tmux = |args: &[&str]| server.run(args).unwrap();
        let session = "fixture";
        let room = "room-fixture";
        let repo = Path::new("/fixture");
        let main = tmux(&[
            "new-session",
            "-d",
            "-s",
            session,
            "-x",
            "140",
            "-y",
            "130",
            "-P",
            "-F",
            "#{pane_id}",
            "sleep 60",
        ]);
        for (option, value) in [
            ("@oms_panel_main_attempt", "attempt-fixture"),
            ("@oms_panel_native_pane", &main),
            ("@oms_panel_owner", "codex"),
            ("@oms_panel_room", room),
            ("@oms_panel_repo", repo.to_str().unwrap()),
        ] {
            tmux(&["set-option", "-w", "-t", &main, option, value]);
        }
        let board = tmux(&[
            "split-window",
            "-d",
            "-h",
            "-b",
            "-l",
            "28%",
            "-t",
            &main,
            "-P",
            "-F",
            "#{pane_id}",
            "sleep 60",
        ]);
        tmux(&[
            "set-option",
            "-p",
            "-t",
            &board,
            "@oms_panel_main_watch",
            "1",
        ]);
        let inventory = || tmux(&["list-panes", "-t", &board, "-F", PANE_LAYOUT]);
        let geometry = |id: &str| {
            let result = inventory();
            let details = result
                .lines()
                .filter_map(Pane::parse)
                .find(|pane| pane.id == id)
                .unwrap();
            (details.bounds, details.pid, details.active)
        };
        let resize = |w, h| {
            tmux(&["resize-window", "-t", &board, "-x", w, "-y", h]);
        };
        let board_pid = geometry(&board).1;
        let reflow = || {
            let mut changes = 0;
            reflow_with(
                &board,
                board_pid,
                session,
                room,
                repo.to_str().unwrap(),
                |args| {
                    changes += usize::from(args[0] == "select-layout");
                    server.run(args)
                },
            );
            changes
        };

        let main_pid = geometry(&main).1;
        assert_eq!(reflow(), 1);
        assert_eq!(geometry(&main).0.y, 0);
        assert!(geometry(&board).0.y > 0);
        resize("316", "90");
        assert_eq!(reflow(), 1);
        assert_eq!(geometry(&board).0.y, 0);
        assert_eq!(geometry(&board).0.width, 316);
        assert_eq!(geometry(&main).0.y, 25);
        assert!(geometry(&main).2);
        assert_eq!(reflow(), 0);

        let provider = tmux(&[
            "split-window",
            "-d",
            "-v",
            "-t",
            &board,
            "-P",
            "-F",
            "#{pane_id}",
            "sleep 60",
        ]);
        tmux(&[
            "set-option",
            "-p",
            "-t",
            &provider,
            "@oms_provider_session",
            "antigravity",
        ]);
        tmux(&[
            "set-option",
            "-p",
            "-t",
            &provider,
            "@oms_provider_parent",
            "attempt-fixture",
        ]);
        tmux(&["select-pane", "-t", &provider]);
        let provider_pid = geometry(&provider).1;
        assert_eq!(reflow(), 1);
        assert_eq!(geometry(&board).0.width, 316);
        assert_eq!(geometry(&main).0.y, geometry(&provider).0.y);
        assert_eq!(geometry(&main).0.x, 0);
        assert!(geometry(&provider).0.x > 0);
        assert!(geometry(&provider).2);

        resize("140", "130");
        assert_eq!(reflow(), 1);
        assert_eq!(geometry(&board).0.x, 0);
        assert_eq!(geometry(&provider).0.x, 0);
        assert_eq!(geometry(&main).0.height, 130);
        resize("59", "32");
        assert_eq!(reflow(), 1);
        assert_eq!(geometry(&main).0.y, 0);
        assert!(geometry(&board).0.y > 0);
        assert!(geometry(&provider).0.y > geometry(&board).0.y);
        assert_eq!(geometry(&provider).0.width, 59);
        assert_eq!(geometry(&main).1, main_pid);
        assert_eq!(geometry(&board).1, board_pid);
        assert_eq!(geometry(&provider).1, provider_pid);
        assert!(geometry(&provider).2);

        resize("316", "90");
        tmux(&[
            "set-option",
            "-p",
            "-t",
            &provider,
            "@oms_provider_parent",
            "foreign",
        ]);
        let before = inventory();
        assert_eq!(reflow(), 0);
        assert_eq!(inventory(), before);
        tmux(&[
            "set-option",
            "-p",
            "-t",
            &provider,
            "@oms_provider_parent",
            "attempt-fixture",
        ]);
        let first = inventory();
        let mut reads = 0;
        reflow_with(
            &board,
            board_pid,
            session,
            room,
            repo.to_str().unwrap(),
            |args| {
                assert_eq!(args[0], "list-panes");
                reads += 1;
                Ok(if reads == 1 {
                    first.clone()
                } else {
                    before.clone()
                })
            },
        );
        assert_eq!(reads, 2);
        tmux(&["resize-pane", "-Z", "-t", &provider]);
        let zoomed = inventory();
        assert_eq!(reflow(), 0);
        assert_eq!(inventory(), zoomed);
    }
}
