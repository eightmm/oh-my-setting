//! Observe a single private receipt. Each caller has its own bounded wait and
//! never consumes a notification or takes the publisher's lock.
use crate::{
    Context, Error, Output, Result, V, check, cli::Options, delivery, digest, git::Git, hex, obj,
    reconcile, require,
};
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

struct Selected<'a> {
    root: oms_safe_fs::Root,
    lock: oms_lock::Lock,
    head: &'a str,
    stamp: &'a str,
    name: String,
    checkout: String,
}

impl Selected<'_> {
    fn activity(&self) -> (&'static str, Option<String>) {
        match self.lock.probe() {
            oms_lock::Status::Free => return ("not-held", None),
            oms_lock::Status::Unknown => return ("unverified", None),
            oms_lock::Status::Held => {}
        }
        let Some(holder) = reconcile::read_private(&self.root, "active-holder")
            .and_then(|bytes| V::parse(&bytes).ok())
        else {
            return ("unverified", None);
        };
        if holder.get("sha").text() != Some(self.head)
            || holder.get("receipt").text() != Some(&self.name)
        {
            return ("unmatched", None);
        }
        let Some(pid) = holder
            .get("pid")
            .py()
            .parse::<u32>()
            .ok()
            .filter(|p| *p > 0)
        else {
            return ("unverified", None);
        };
        let Some(birth) = holder.get("start").text().filter(|s| !s.is_empty()) else {
            return ("unverified", None);
        };
        if !oms_command_operations::lock::pid_alive(i64::from(pid)) {
            return ("process-ended", None);
        }
        match oms_command_operations::lock::process_start(pid) {
            Some(live) if live == birth => ("held", Some(format!("{pid}:{live}"))),
            Some(_) => ("process-ended", None),
            None => ("unverified", None),
        }
    }

    fn observe(&self) -> Result<(V, String)> {
        // Publication saves its final receipt before releasing the lock. Read
        // the receipt after the holder sample to capture that final write.
        let (activity, holder) = self.activity();
        let bytes = reconcile::read_private(&self.root, &self.name)
            .ok_or_else(|| Error::new(1, "exact receipt unavailable or untrusted"))?;
        let row = V::parse(&bytes).map_err(|_| Error::new(1, "invalid exact receipt"))?;
        require(
            reconcile::matches_checkout(&row, self.head, self.stamp, &self.checkout),
            "exact receipt does not match the selected checkout and request",
        )?;
        let state = row.get("state").text().unwrap_or("");
        let mode = row.get("mode").text().unwrap_or("");
        let gate = row.get("gate").get("rc");
        let push = row.get("push").get("rc");
        // An exact remote head can already exist. Delivery records that PR
        // even when this invocation did not perform or record a push.
        let pr_recorded = mode == "pr"
            && row.get("pr_number").integer(true)
            && row.get("pr_url").text().is_some_and(|url| !url.is_empty())
            && gate == &0usize.into();
        require(
            matches!(mode, "pr" | "direct" | "local-merge")
                && (state == "running"
                    || row.get("finished_at").text().is_some_and(|s| !s.is_empty()))
                && matches!(state, "running" | "awaiting-merge" | "passed" | "failed")
                && (state != "awaiting-merge" || pr_recorded),
            "unrecognized local receipt outcome",
        )?;
        let phase = if pr_recorded {
            "pr-recorded"
        } else if push == &0usize.into() {
            "push-recorded"
        } else if gate == &0usize.into() {
            "gate-passed"
        } else if gate.integer(false) {
            "gate-finished"
        } else {
            "gate-pending"
        };
        // A retry can replace its live process without changing the visible phase.
        // Only a digest of the revision contributes to the request-scoped cursor.
        let revision = format!("{}:{}", digest(&bytes), holder.unwrap_or_default());
        Ok((
            obj([
                ("activity", activity.into()),
                ("receipt_state", state.into()),
                ("phase", phase.into()),
            ]),
            revision,
        ))
    }
}

fn wait(
    flag: &AtomicBool,
    prefix: &str,
    after: &str,
    seconds: u64,
    observe: impl Fn() -> Result<(V, String)>,
) -> Result<V> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        check(flag)?;
        let (mut row, revision) = observe()?;
        check(flag)?;
        let activity = row.get("activity").text().unwrap_or("");
        let cursor = format!(
            "{prefix}{}",
            digest(format!("{prefix}{revision}:{activity}").as_bytes())
        );
        let held = row.get("activity").text() == Some("held");
        let terminal = row.get("receipt_state").text() != Some("running");
        let event = if terminal && !held {
            "terminal"
        } else if !held {
            "holder-unconfirmed"
        } else if after.is_empty() {
            "snapshot"
        } else if cursor != after {
            "changed"
        } else if Instant::now() >= deadline {
            "timeout"
        } else {
            // Independent waiters only sample the private pair. Cancellation
            // remains responsive between observations without polling Git.
            let next = deadline.min(Instant::now() + Duration::from_secs(1));
            while Instant::now() < next {
                check(flag)?;
                std::thread::sleep(
                    next.saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(50)),
                );
            }
            continue;
        };
        row.set("cursor", cursor);
        row.set("event", event);
        row.set("schema", 1usize);
        return Ok(row);
    }
}

pub(super) fn execute(c: &Context<'_>, o: &Options) -> Result<Output> {
    require(
        o.words.len() == 1
            && o.values.iter().all(|(k, v)| {
                [
                    "repo",
                    "reviewed-head",
                    "request",
                    "wait-seconds",
                    "after",
                    "json",
                ]
                .contains(&k.as_str())
                    && (k == "json" || v.len() == 1)
            }),
        "land wait accepts one reviewed head, request, repo, cursor, duration and JSON option only",
    )?;
    let head = o.get("reviewed-head");
    let stamp = o.get("request");
    require(
        (hex(head, 40) || hex(head, 64)) && reconcile::matches_request(head, stamp),
        "land wait requires a full reviewed head and its exact request stamp",
    )?;
    let seconds = o
        .value("wait-seconds", "0")
        .parse::<u64>()
        .map_err(|_| Error::new(2, "--wait-seconds must be 0..50"))?;
    require(seconds <= 50, "--wait-seconds must be 0..50")?;
    let repo = c.repo(o)?;
    let checkout = digest(repo.as_os_str().as_encoded_bytes());
    let prefix = format!("v1.{}.", digest(format!("{checkout}:{stamp}").as_bytes()));
    let after = o.get("after");
    require(
        !o.has("after") || after.strip_prefix(&prefix).is_some_and(|h| hex(h, 64)),
        "cursor does not match this checkout and request",
    )?;
    let directory = delivery::state_directory(&Git::new(&repo, c.interrupted))?;
    let root = oms_safe_fs::Root::open_private_workspace(&directory, |_| false)
        .map_err(|_| Error::new(1, "exact receipt store unavailable or untrusted"))?;
    let settings =
        oms_lock::Settings::from_env().map_err(|e| Error::new(e.exit_code(), e.to_string()))?;
    let selected = Selected {
        root,
        lock: oms_lock::Lock::for_file(&directory.join("active"), settings)
            .map_err(|e| Error::new(e.exit_code(), e.to_string()))?,
        head,
        stamp,
        name: format!("{stamp}.json"),
        checkout,
    };
    let result = wait(c.interrupted, &prefix, after, seconds, || {
        selected.observe()
    })?;
    if o.has("json") {
        Output::json(&result)
    } else {
        Ok(Output::text(format!(
            "land wait: {} ({}, {}, {})\ncursor: {}\n",
            result.get("event").py(),
            result.get("receipt_state").py(),
            result.get("phase").py(),
            result.get("activity").py(),
            result.get("cursor").py()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_stops_an_unchanged_wait() {
        let flag = AtomicBool::new(false);
        let snapshot = || {
            Ok((
                obj([
                    ("activity", "held".into()),
                    ("receipt_state", "running".into()),
                ]),
                String::from("first"),
            ))
        };
        let first = wait(&flag, "test.", "", 0, snapshot).unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(0);
        std::thread::scope(|scope| {
            let cancel = &flag;
            scope.spawn(move || {
                rx.recv().unwrap();
                cancel.store(true, std::sync::atomic::Ordering::Release);
            });
            let result = wait(
                &flag,
                "test.",
                first.get("cursor").text().unwrap(),
                50,
                || {
                    tx.send(()).unwrap();
                    snapshot()
                },
            );
            assert_eq!(result.unwrap_err().code, 130);
        });
    }
}
