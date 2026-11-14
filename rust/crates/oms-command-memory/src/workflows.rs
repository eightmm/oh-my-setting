use crate::{cli::Options, store::Store, *};
use std::{path::Path, sync::atomic::AtomicBool};

pub(crate) use crate::conflicts::contradictions;
pub(crate) use crate::redact::redact;
pub(crate) fn execute(
    s: &Store,
    o: &Options,
    author: &str,
    cwd: &Path,
    flag: &AtomicBool,
) -> Result<Output> {
    match o.mode.as_str() {
        "import" => crate::review::import(s, o, author, cwd, flag),
        "review" => crate::review::review(s, o, author, flag),
        "capture" => crate::review::capture(s, flag),
        "promote" | "graduate" => crate::carry::carry(s, o, author, flag),
        "recall" if o.has("--all-repos") => crate::carry::recall_all(o, cwd, flag),
        "hygiene" => crate::hygiene::run(s, o),
        _ => err(format!("unknown command: {}", o.mode)),
    }
}
pub(crate) fn output(s: impl AsRef<str>) -> Output {
    (0, format!("{}\n", s.as_ref()).into_bytes(), Vec::new())
}
pub(crate) fn preview(s: &str, width: usize) -> String {
    let line = flat(s.trim().lines().next().unwrap_or(""));
    if line.chars().count() > width {
        format!("{}…", clip(&line, width.saturating_sub(1)))
    } else {
        line
    }
}
pub(crate) fn normalized(s: &str) -> String {
    fold(&flat(s))
}
pub(crate) fn short_hash(s: &str) -> String {
    hash(normalized(s).as_bytes())[..12].into()
}
pub(crate) fn review_id(s: &str) -> bool {
    s.len() <= 128 && records::valid_id(s)
}
