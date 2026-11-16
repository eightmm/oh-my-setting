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
            Self::Auto | Self::Summary => Self::Compact,
            Self::Compact => Self::Detail,
            Self::Detail => Self::Graph,
            Self::Graph => Self::Tree,
            Self::Tree => Self::Summary,
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
