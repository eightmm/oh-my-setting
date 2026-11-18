use crate::value::{V, display};
use std::fmt::Write;

pub(crate) fn human(r: &V) -> String {
    let mut out = String::new();
    if let Some(b) = r.get("repair_replay") {
        writeln!(out,"repair replay: {}/{} delegations recorded, {} selected, {} early stops (token savings unmeasured)",display(&b["recorded"]),display(&b["delegations_seen"]),display(&b["selected"]),display(&b["early_stops"])).unwrap();
    }
    let w = &r["window"];
    let op = &r["operations"];
    let e = &r["recorded_exit"];
    let v = &r["verification"];
    let f = &r["fallback"];
    let c = &r["coverage"];
    let u = &r["usage"];
    writeln!(
        out,
        "retained window: {}/{} row(s) (limit {}; invalid lines {})",
        display(&w["selected_rows"]),
        display(&w["available_rows"]),
        display(&w["limit"]),
        display(&w["invalid_lines"])
    )
    .unwrap();
    writeln!(
        out,
        "routed operations: {} (excluded {})",
        display(&op["eligible"]),
        display(&op["excluded"])
    )
    .unwrap();
    writeln!(out, "semantic outcome: {}", display(&r["semantic_outcome"])).unwrap();
    writeln!(
        out,
        "recorded exit: zero={} nonzero={} unavailable={}",
        display(&e["zero"]),
        display(&e["nonzero"]),
        display(&e["unavailable"])
    )
    .unwrap();
    writeln!(
        out,
        "verification: recorded={} nonzero={} unavailable={}",
        display(&v["recorded"]),
        display(&v["nonzero"]),
        display(&v["unavailable"])
    )
    .unwrap();
    writeln!(
        out,
        "fallback: used={} not-used={} unrecorded={}",
        display(&f["used"]),
        display(&f["not_used"]),
        display(&f["unrecorded"])
    )
    .unwrap();
    writeln!(
        out,
        "coverage: routing={} artifacts={} durations={} token-reports={}",
        display(&c["routing_metadata"]),
        display(&c["artifact_files"]),
        display(&c["duration_reports"]),
        display(&c["token_reports"])
    )
    .unwrap();
    writeln!(out,"provider-reported tokens: total={} reports={}; observed wall seconds: total={} median={} reports={}",display(&u["provider_reported_tokens"]["total"]),display(&u["provider_reported_tokens"]["reports"]),display(&u["wall_seconds"]["total"]),display(&u["wall_seconds"]["median"]),display(&u["wall_seconds"]["reports"])).unwrap();
    let p = &u["prompt_bytes"];
    writeln!(
        out,
        "prompt bytes: total={} reports={} ({})",
        display(&p["total"]),
        display(&p["reports"]),
        display(&p["basis"])
    )
    .unwrap();
    let n = &r["native_activity"];
    writeln!(
        out,
        "native activity: sessions={} turns={} tools={} subagents={} events={}",
        display(&n["sessions"]),
        display(&n["turns"]),
        display(&n["tool_events"]),
        display(&n["subagent_events"]),
        display(&n["events"])
    )
    .unwrap();
    for route in r["routes"].array() {
        let d = &route["usage_detail"];
        if d["reports"].truth() {
            writeln!(
                out,
                "tokens: provider={} model={} input={} cached={} output={} reports={}",
                display(&route["provider"]),
                display(&route["selected_model"]),
                display(&d["input_tokens"]),
                display(&d["cache_read_tokens"]),
                display(&d["output_tokens"]),
                display(&d["reports"])
            )
            .unwrap();
        }
        writeln!(out,"route: provider={} class={} model={} operations={} exit-zero={} exit-nonzero={} fallback={}",display(&route["provider"]),display(&route["model_class"]),display(&route["selected_model"]),display(&route["operations"]),display(&route["recorded_exit_zero"]),display(&route["recorded_exit_nonzero"]),display(&route["fallback_used"])).unwrap();
    }
    if u["cumulative_reports_excluded"].truth() {
        writeln!(out,"cumulative: {} resumed-session report(s) carry a running total and are left out of the tokens lines",display(&u["cumulative_reports_excluded"])).unwrap();
    }
    lifetime(r, &mut out);
    out
}
fn lifetime(r: &V, out: &mut String) {
    if let Some(u) = r.get("lifetime_provider_usage") {
        writeln!(out,"lifetime provider receipts (independent of artifact window): calls={} tokens={} unknown-token-calls={} reported-microUSD={} unknown-cost-calls={} unsettled={} conflicts={} provider-estimate-calls={}",display(&u["calls"]),display(&u["tokens"]),display(&u["unknown_token_calls"]),display(&u["cost_microusd"]),display(&u["unknown_cost_calls"]),display(&u["unsettled_calls"]),display(&u["conflicts"]),display(&u["provider_estimate_calls"])).unwrap();
        writeln!(
            out,
            "native cleanup evidence: confirmed={} incomplete={} unknown={}",
            display(&u["cleanup_confirmed_calls"]),
            display(&u["cleanup_incomplete_calls"]),
            display(&u["cleanup_unknown_calls"])
        )
        .unwrap();
    }
}
fn reported(total: &V, reports: &V, calls: &V) -> String {
    if !reports.truth() {
        return "-".into();
    }
    let text = if let V::Float(n) = total {
        format!("{n:.4}")
    } else {
        display(total)
    };
    if reports == calls {
        text
    } else {
        format!("{text}+?")
    }
}
pub(crate) fn grouped(r: &V) -> String {
    let w = &r["window"];
    let b = &r["usage_by"];
    let by = b["by"].text().unwrap();
    let mut out = String::new();
    let mut scope = format!("limit {}", display(&w["limit"]));
    if w.get("matched_rows").is_some() {
        scope = format!("{} in range; {scope}", display(&w["matched_rows"]));
    }
    writeln!(
        out,
        "usage by {}{}: {} routed operation(s) in {}/{} retained row(s) ({scope})",
        by,
        if by == "day" { " (UTC)" } else { "" },
        display(&r["operations"]["eligible"]),
        display(&w["selected_rows"]),
        display(&w["available_rows"])
    )
    .unwrap();
    if b["capped"].truth() {
        writeln!(out,"window capped: the {} oldest matching row(s) are not counted; raise N or narrow the range",display(&b["omitted_rows"])).unwrap();
    } else {
        writeln!(
            out,
            "window complete: every matching retained row is counted (pruned history is not)"
        )
        .unwrap();
    }
    if w["undated_rows"].truth() {
        writeln!(
            out,
            "undated: {} row(s) carry no time and are outside any range",
            display(&w["undated_rows"])
        )
        .unwrap();
    }
    let role = by == "role";
    let with_effort = role && b["groups"].array().iter().any(
        |g| matches!(&g["efforts"],V::Object(m) if m.keys().any(|k| k.as_slice()!=b"unrecorded")),
    );
    let mut head = vec![by.to_owned()];
    if with_effort {
        head.push("effort".into());
    }
    head.extend(["calls", "tokens", "input", "cached", "output", "cost-usd"].map(str::to_owned));
    if role {
        head.extend(["cache%", "admitted", "tok/admit", "unadmitted-tok"].map(str::to_owned));
    }
    let mut table = vec![head];
    for g in b["groups"].array() {
        let mut cells = vec![display(&g["group"])];
        if with_effort {
            let mut entries = Vec::new();
            if let V::Object(m) = &g["efforts"] {
                for (k, v) in m {
                    entries.push(format!("{}:{}", String::from_utf8_lossy(k), display(v)));
                }
            }
            cells.push(entries.join(" "));
        }
        cells.push(display(&g["calls"]));
        for (total, reports) in [
            ("provider_reported_tokens", "token_reports"),
            ("input_tokens", "usage_reports"),
            ("cache_read_tokens", "usage_reports"),
            ("output_tokens", "usage_reports"),
            ("cost_usd", "cost_reports"),
        ] {
            cells.push(reported(&g[total], &g[reports], &g["calls"]));
        }
        if role {
            cells.push(
                g["cache_read_share"]
                    .number()
                    .map_or("-".into(), |s| format!("{:.1}%", s * 100.0)),
            );
            cells.push(display(&g["admitted"]));
            cells.push(if g["tokens_per_admitted"] == V::Null {
                "-".into()
            } else {
                display(&g["tokens_per_admitted"])
            });
            cells.push(display(&g["not_admitted_tokens"]));
        }
        table.push(cells);
    }
    let widths: Vec<_> = (0..table[0].len())
        .map(|n| {
            table
                .iter()
                .map(|row| row[n].chars().count())
                .max()
                .unwrap()
        })
        .collect();
    for row in table {
        let cells: Vec<_> = row
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let padding = " ".repeat(widths[i] - s.chars().count());
                if i == 0 {
                    format!("{s}{padding}")
                } else {
                    format!("{padding}{s}")
                }
            })
            .collect();
        writeln!(out, "{}", cells.join("  ").trim_end()).unwrap();
    }
    if b["groups"].array().is_empty() {
        writeln!(out, "(no routed operation in this window)").unwrap();
    }
    let excluded: usize = b["groups"]
        .array()
        .iter()
        .map(|g| g["cumulative_reports_excluded"].count())
        .sum();
    if excluded > 0 {
        writeln!(out,"cumulative: {excluded} resumed-session report(s) carry a running total and are left out of input/cached/output").unwrap();
    }
    for warning in b["warnings"].array() {
        writeln!(out, "warning: {}", display(warning)).unwrap();
    }
    if role {
        writeln!(out,"admitted = delegate rows whose patch a later patch-admit/patch-land accepted; tok/admit = group tokens / admitted; unadmitted-tok = tokens on the other delegate rows; reported values, never approval evidence").unwrap();
    }
    writeln!(out,"reported values only: `-` none reported, `+?` some calls reported nothing; no cost is derived from tokens and groups are not summed").unwrap();
    out
}

pub(crate) fn validate(report: &V, grouped: bool) -> Result<(), String> {
    let values: Vec<&V> = if grouped {
        report["usage_by"]["groups"]
            .array()
            .iter()
            .map(|g| &g["group"])
            .collect()
    } else {
        report["routes"]
            .array()
            .iter()
            .flat_map(|r| [&r["provider"], &r["model_class"], &r["selected_model"]])
            .collect()
    };
    fn check_numbers(v: &V) -> bool {
        match v {
            V::Int(n) => n.trim_start_matches('-').len() <= 4300,
            V::Array(a) => a.iter().all(check_numbers),
            V::Object(m) => m.values().all(check_numbers),
            _ => true,
        }
    }
    let numeric = if grouped {
        vec![&report["usage_by"]["groups"]]
    } else {
        vec![
            &report["routes"],
            &report["usage"]["provider_reported_tokens"],
            &report["usage"]["prompt_bytes"],
        ]
    };
    if !numeric.into_iter().all(check_numbers) {
        return Err("artifact telemetry: recorded integer exceeds the digit limit".into());
    }
    if values
        .iter()
        .any(|v| v.bytes().is_some_and(|s| std::str::from_utf8(s).is_err()))
    {
        Err("artifact telemetry: cannot encode surrogate text".into())
    } else {
        Ok(())
    }
}
