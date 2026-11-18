use crate::{Error, Result, require};

pub const BOARD_SHARE: usize = 28;
pub const BOARD_HEIGHT_SHARE: usize = 45;
pub const NATIVE_MIN: usize = 40;
pub const MIN_BOARD: usize = 20;

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
            Self::Auto => Self::Summary,
            Self::Summary => Self::Compact,
            Self::Compact => Self::Detail,
            Self::Detail => Self::Graph,
            Self::Graph => Self::Tree,
            Self::Tree => Self::Auto,
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

/// Reflow just the managed watcher and Main, leaving other layouts in place.
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
    let tmux = |args: &[&str]| {
        oms_panel_core::terminals::tmux(services, repo, &crate::commands::strings(args), stop)
    };
    let Ok(raw) = tmux(&[
        "list-panes",
        "-t",
        &pane,
        "-F",
        "#{pane_id}\t#{pane_active}\t#{pane_dead}\t#{pane_left}\t#{pane_top}\t#{window_width}\t#{window_height}\t#{window_zoomed_flag}\t#{session_name}\t#{@oms_panel_main_watch}\t#{@oms_panel_main_attempt}\t#{@oms_panel_native_pane}\t#{@oms_panel_room}\t#{@oms_panel_repo}",
    ]) else {
        return;
    };
    let rows: Vec<Vec<&str>> = raw.lines().map(|row| row.split('\t').collect()).collect();
    if rows.len() != 2
        || rows.iter().any(|r| {
            r.len() != 14
                || !crate::commands::valid_target(r[0], '%')
                || r[2] != "0"
                || r[7] != "0"
                || r[8] != session
                || r[10].is_empty()
                || r[12] != room
                || Some(r[13]) != repo.to_str()
        })
    {
        return;
    }
    let Some(board_index) = rows.iter().position(|r| r[0] == pane && r[9] == "1") else {
        return;
    };
    let board = &rows[board_index];
    let main = &rows[1 - board_index];
    if main[9] == "1" || board[10] != main[10] || board[11] != main[0] {
        return;
    }
    let Some(active) = rows.iter().find(|r| r[1] == "1").map(|r| r[0]) else {
        return;
    };
    let (Ok(width), Ok(height)) = (board[5].parse::<usize>(), board[6].parse::<usize>()) else {
        return;
    };
    let (Ok(board_x), Ok(board_y), Ok(main_x), Ok(main_y)) = (
        board[3].parse::<usize>(),
        board[4].parse::<usize>(),
        main[3].parse::<usize>(),
        main[4].parse::<usize>(),
    ) else {
        return;
    };
    let below = board_below(width, height);
    if (below && board_x == main_x && board_y > main_y)
        || (!below && board_y == main_y && board_x < main_x)
    {
        return;
    }
    let (preset, axis, cells) = if below {
        ("even-vertical", "-y", height * BOARD_HEIGHT_SHARE / 100)
    } else {
        ("even-horizontal", "-x", split_cells(None, width))
    };
    let cells = cells.to_string();
    let mut args = vec!["select-layout", "-t", &pane, preset];
    // Tmux assigns layout slots in pane-list order. Keep the active process
    // selected when a slot swap changes which pane tmux considers active.
    if board_index != usize::from(below) {
        args.extend([";", "swap-pane", "-d", "-s", &pane, "-t", main[0]]);
    }
    args.extend([
        ";",
        "resize-pane",
        "-t",
        &pane,
        axis,
        &cells,
        ";",
        "select-pane",
        "-t",
        active,
    ]);
    let _ = tmux(&args);
}

fn board_below(width: usize, height: usize) -> bool {
    height >= 20 && (width < 100 || width < height.saturating_mul(2))
}

#[cfg(test)]
mod tests {
    #[test]
    fn tab_returns_to_auto_after_the_explicit_views() {
        use super::View;
        let expected = [
            View::Summary,
            View::Compact,
            View::Detail,
            View::Graph,
            View::Tree,
            View::Auto,
        ];
        let mut view = View::Auto;
        for next in expected {
            view = view.next();
            assert_eq!(view, next);
        }
    }
    #[test]
    fn orientation_leaves_room_for_main_and_board() {
        assert!(!super::board_below(220, 70));
        assert!(super::board_below(140, 130));
        assert!(super::board_below(80, 30));
        assert!(!super::board_below(80, 18));
    }
}
