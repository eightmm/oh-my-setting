use crate::Options;
use crate::metrics::{complete, cumulative, metrics, native};
use crate::time::{bound, day, is_day, timestamp};
use crate::value::{V, equal, obj, ratio, rounded};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

const CLASSES: [&str; 6] = [
    "explicit",
    "provider-default",
    "role-default",
    "fast",
    "balanced",
    "deep",
];
const EFFORTS: [&str; 6] = ["low", "medium", "high", "xhigh", "max", "ultra"];
const EMPTY_DIGEST: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const USAGE: [&str; 3] = ["input_tokens", "cache_read_tokens", "output_tokens"];
fn label(value: &V, fallback: &str) -> Vec<u8> {
    value
        .bytes()
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback.as_bytes())
        .to_vec()
}
fn stamp(row: &V) -> Option<i64> {
    row["ts"].text().and_then(timestamp)
}
fn effort(row: &V) -> &str {
    for key in ["selected_reasoning_effort", "reasoning_effort"] {
        if let Some(v) = row[key].text().filter(|s| EFFORTS.contains(s)) {
            return v;
        }
    }
    "unrecorded"
}
fn grouping(row: &V, by: &str, measured: &crate::metrics::Metrics) -> Result<Vec<u8>, String> {
    Ok(match by {
        "day" => stamp(row)
            .map(day)
            .transpose()?
            .unwrap_or("undated".into())
            .into_bytes(),
        "effort" => effort(row).as_bytes().to_vec(),
        "role" => row["role"]
            .bytes()
            .filter(|s| {
                let body = s.strip_suffix(b"\n").unwrap_or(s);
                !body.is_empty()
                    && body.len() <= 160
                    && body
                        .iter()
                        .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(b))
            })
            .unwrap_or(b"unknown")
            .to_vec(),
        "provider" => label(&row["provider"], "unrecorded"),
        "task" => label(&row["task_id"], "unrecorded"),
        _ if measured.ambiguous => b"ambiguous".to_vec(),
        _ => measured
            .model
            .as_ref()
            .map(|v| label(v, "unrecorded"))
            .unwrap_or_else(|| label(&row["selected_model"], "unrecorded")),
    })
}
fn zero_fields(keys: &[&str]) -> V {
    V::Object(
        keys.iter()
            .map(|k| (k.as_bytes().to_vec(), V::from(0)))
            .collect(),
    )
}
fn route(provider: Vec<u8>, class: Vec<u8>, model: Vec<u8>) -> V {
    let mut v = zero_fields(&[
        "operations",
        "recorded_exit_zero",
        "recorded_exit_nonzero",
        "recorded_exit_unavailable",
        "verification_recorded",
        "verification_nonzero",
        "fallback_used",
        "token_reports",
        "provider_reported_tokens",
        "cumulative_reports_excluded",
        "duration_reports",
        "verified_success",
        "verified_failure",
        "outcome_unknown",
    ]);
    v["provider"] = provider.into();
    v["model_class"] = class.into();
    v["selected_model"] = model.into();
    v["usage_detail"] = zero_fields(&[
        "reports",
        "input_tokens",
        "cache_read_tokens",
        "output_tokens",
    ]);
    v["wall_seconds"] = 0.0.into();
    v
}
fn group(name: Vec<u8>, role: bool) -> V {
    let mut v = zero_fields(&[
        "calls",
        "token_reports",
        "provider_reported_tokens",
        "usage_reports",
        "input_tokens",
        "cache_read_tokens",
        "output_tokens",
        "cumulative_reports_excluded",
        "cost_reports",
    ]);
    v["group"] = name.into();
    v["cost_usd"] = 0.0.into();
    if role {
        for key in [
            "cache_denominator",
            "admitted",
            "not_admitted_results",
            "not_admitted_tokens",
        ] {
            v[key] = 0.into();
        }
        v["efforts"] = obj!();
    }
    v
}
fn valid_digest(v: &V) -> bool {
    v.bytes().is_some_and(|s| {
        s.len() == 64
            && s.iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    })
}
fn admission_digest(v: &V) -> bool {
    v.bytes().is_some_and(|s| {
        let s = s.strip_suffix(b"\n").unwrap_or(s);
        s.len() == 64
            && s.iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    })
}
fn resolved(rows: &[V]) -> BTreeSet<Vec<u8>> {
    let mut entries = BTreeMap::<Vec<u8>, Vec<(usize, &V)>>::new();
    for (pos, row) in rows.iter().enumerate() {
        if let Some(id) = row["event_id"].bytes() {
            entries.entry(id.to_vec()).or_default().push((pos, row));
        }
    }
    let mut result = BTreeSet::new();
    for (pos, row) in rows.iter().enumerate() {
        if row["kind"].text() != Some("artifact-resolution") {
            continue;
        }
        let (Some(target), Some(id)) = (row["resolves_event_id"].bytes(), row["event_id"].bytes())
        else {
            continue;
        };
        let (Some(targets), Some(resolvers)) = (entries.get(target), entries.get(id)) else {
            continue;
        };
        if targets.len() != 1 || resolvers.len() != 1 {
            continue;
        }
        let (oldpos, old) = targets[0];
        if oldpos < pos
            && schema_one(&row["schema"])
            && schema_one(&old["schema"])
            && row["parent_event_id"] == row["resolves_event_id"]
            && row["resolution"].text() == Some("resolved")
            && row["exit"].int() == Some("0")
            && old["exit"].int().is_some_and(|v| v != "0")
            && old["kind"].text() != Some("artifact-resolution")
            && equal(&row["operation_id"], &old["operation_id"])
            && equal(&row["artifact_id"], &old["artifact_id"])
        {
            result.insert(target.to_vec());
        }
    }
    result
}
fn schema_one(v: &V) -> bool {
    v.number() == Some(1.0) || *v == V::Bool(true)
}
fn env_int(name: &str, default: usize) -> V {
    std::env::var(name)
        .ok()
        .and_then(|value| environment_integer(&value))
        .unwrap_or_else(|| default.into())
}
fn environment_integer(value: &str) -> Option<V> {
    let value =
        value.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c));
    let (negative, digits) = if let Some(s) = value.strip_prefix('-') {
        (true, s)
    } else {
        (false, value.strip_prefix('+').unwrap_or(value))
    };
    let mut result = String::new();
    let mut previous_digit = false;
    for c in digits.chars() {
        if c == '_' {
            if !previous_digit {
                return None;
            }
            previous_digit = false;
        } else {
            let digit = decimal_digit(c)?;
            result.push(char::from(b'0' + digit));
            previous_digit = true;
        }
    }
    if !previous_digit || result.len() > 4300 {
        return None;
    }
    let normalized = result.trim_start_matches('0');
    if negative && !normalized.is_empty() {
        return None;
    }
    Some(V::Int(if normalized.is_empty() {
        "0".into()
    } else {
        normalized.into()
    }))
}
fn decimal_digit(c: char) -> Option<u8> {
    use std::sync::LazyLock;
    static DECIMAL: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^\p{Nd}$").unwrap());
    if c.is_ascii_digit() {
        return Some(c as u8 - b'0');
    }
    let mut bytes = [0; 4];
    if !DECIMAL.is_match(c.encode_utf8(&mut bytes)) {
        return None;
    }
    let mut start = c as u32;
    while start > 0
        && char::from_u32(start - 1).is_some_and(|c| DECIMAL.is_match(c.encode_utf8(&mut bytes)))
    {
        start -= 1;
    }
    Some(((c as u32 - start) % 10) as u8)
}

pub(crate) fn report(
    root: &oms_safe_fs::Root,
    rows: &[V],
    options: &Options,
    interrupted: &AtomicBool,
) -> Result<V, String> {
    let since = bound(options.since.as_deref(), false)?;
    let until = bound(options.until.as_deref(), true)?;
    let bounded = since.is_some() || until.is_some();
    let until_day = options.until.as_deref().is_some_and(is_day);
    let matched: Vec<&V> = rows
        .iter()
        .filter(|row| {
            !bounded
                || stamp(row).is_some_and(|t| {
                    since.is_none_or(|s| t >= s)
                        && until.is_none_or(|u| if until_day { t < u } else { t <= u })
                })
        })
        .collect();
    let limit = options.limit;
    let selected = &matched[matched.len().saturating_sub(limit)..];
    let eligible: Vec<&&V> = selected
        .iter()
        .filter(|row| {
            row["kind"]
                .text()
                .is_some_and(|s| ["call", "ask", "review", "delegate"].contains(&s))
        })
        .collect();
    let resolved = resolved(rows);
    let admitted: BTreeSet<Vec<u8>> = rows
        .iter()
        .filter(|r| {
            [Some("patch-admit"), Some("patch-land")].contains(&r["kind"].text())
                && r["exit"].int() == Some("0")
                && admission_digest(&r["patch_sha256"])
                && r["patch_sha256"].text() != Some(EMPTY_DIGEST)
        })
        .filter_map(|r| r["patch_sha256"].bytes().map(<[u8]>::to_vec))
        .collect();
    let mut exits = zero_fields(&["zero", "nonzero", "unavailable"]);
    let mut verification = zero_fields(&["recorded", "zero", "nonzero", "unavailable"]);
    let mut fallback = zero_fields(&["used", "not_used", "unrecorded"]);
    let mut reasons = BTreeMap::<Vec<u8>, usize>::new();
    let mut resolution = zero_fields(&["events", "resolved_nonzero", "unresolved_nonzero"]);
    resolution["events"] = selected
        .iter()
        .filter(|r| r["kind"].text() == Some("artifact-resolution"))
        .count()
        .into();
    let mut coverage = zero_fields(&[
        "routing_metadata",
        "artifact_files",
        "duration_reports",
        "token_reports",
        "verification_reports",
    ]);
    let mut outcomes = zero_fields(&["verified_success", "verified_failure", "unknown"]);
    let mut token_total = obj!("value":0);
    let mut prompt_total = obj!("value":0);
    let mut prompt_reports = 0;
    let mut durations = Vec::new();
    let mut routes = BTreeMap::new();
    let mut groups = BTreeMap::new();
    for row in &eligible {
        if interrupted.load(Ordering::Relaxed) {
            return Err("interrupted".into());
        }
        let exit = row["exit"].int();
        let exit_key = match exit {
            None => "unavailable",
            Some("0") => "zero",
            _ => "nonzero",
        };
        exits.inc(exit_key);
        if exit_key == "nonzero" {
            let is_resolved = row["event_id"]
                .bytes()
                .is_some_and(|id| resolved.contains(id));
            resolution.inc(if is_resolved {
                "resolved_nonzero"
            } else {
                "unresolved_nonzero"
            });
        }
        let verify = if row["kind"].text() == Some("delegate")
            && [Some("none"), Some("dry-run")].contains(&row["context_verification"].text())
        {
            None
        } else {
            row["verify_exit"].int()
        };
        let outcome = match verify {
            None => "unknown",
            Some("0") if exit.is_none() => "unknown",
            Some("0") if exit == Some("0") => "verified_success",
            _ => "verified_failure",
        };
        outcomes.inc(outcome);
        if let Some(v) = verify {
            verification.inc("recorded");
            coverage.inc("verification_reports");
            verification.inc(if v == "0" { "zero" } else { "nonzero" });
        } else {
            verification.inc("unavailable");
        }
        match row["fallback_used"] {
            V::Bool(true) => {
                fallback.inc("used");
                if let Some(reason) = row["fallback_reason"].bytes().filter(|s| !s.is_empty()) {
                    *reasons.entry(reason.to_vec()).or_default() += 1;
                }
            }
            V::Bool(false) => fallback.inc("not_used"),
            _ => fallback.inc("unrecorded"),
        }
        let provider = label(&row["provider"], "unrecorded");
        let class = row["model_class"]
            .text()
            .filter(|s| CLASSES.contains(s))
            .unwrap_or("unrecorded")
            .as_bytes()
            .to_vec();
        let model = label(&row["selected_model"], "unrecorded");
        if [&provider, &class, &model]
            .iter()
            .all(|v| v.as_slice() != b"unrecorded")
        {
            coverage.inc("routing_metadata");
        }
        let key = (provider.clone(), class.clone(), model.clone());
        let route = routes
            .entry(key)
            .or_insert_with(|| route(provider, class, model));
        route.inc("operations");
        route.inc(&format!("recorded_exit_{exit_key}"));
        if let Some(v) = verify {
            route.inc("verification_recorded");
            if v != "0" {
                route.inc("verification_nonzero");
            }
        }
        route.inc(if outcome == "unknown" {
            "outcome_unknown"
        } else {
            outcome
        });
        if row["fallback_used"] == V::Bool(true) {
            route.inc("fallback_used");
        }
        let measured = metrics(root.physical_root(), row)?;
        if let Some(detail) = &measured.detail {
            if cumulative(detail) {
                route.inc("cumulative_reports_excluded");
            } else if complete(detail) {
                route["usage_detail"].inc("reports");
                for key in USAGE {
                    route["usage_detail"].add(key, &detail[key]);
                }
            }
        }
        if row["prompt_bytes"].int().is_some() {
            prompt_reports += 1;
            prompt_total.add("value", &row["prompt_bytes"]);
        }
        if measured.present {
            coverage.inc("artifact_files");
        }
        if let Some(duration) = measured.duration {
            coverage.inc("duration_reports");
            durations.push(duration);
            route.inc("duration_reports");
            route["wall_seconds"] =
                rounded(route["wall_seconds"].number().unwrap() + duration, 3).into();
        }
        if let Some(tokens) = &measured.tokens {
            coverage.inc("token_reports");
            token_total.add("value", tokens);
            route.inc("token_reports");
            route.add("provider_reported_tokens", tokens);
        }
        if let Some(by) = &options.by {
            let name = grouping(row, by, &measured)?;
            let g = groups
                .entry(name.clone())
                .or_insert_with(|| group(name, by == "role"));
            g.inc("calls");
            if let Some(tokens) = &measured.tokens {
                g.inc("token_reports");
                g.add("provider_reported_tokens", tokens);
            }
            if let Some(detail) = &measured.detail {
                if cumulative(detail) {
                    g.inc("cumulative_reports_excluded");
                } else if complete(detail) {
                    g.inc("usage_reports");
                    for key in USAGE {
                        g.add(key, &detail[key]);
                    }
                    if by == "role" {
                        g.add("cache_denominator", &detail["input_tokens"]);
                        if detail["cache_in_input"] != V::Bool(true) {
                            g.add("cache_denominator", &detail["cache_read_tokens"]);
                        }
                    }
                }
            }
            if let Some(cost) = measured.cost {
                g.inc("cost_reports");
                g["cost_usd"] = rounded(g["cost_usd"].number().unwrap() + cost, 6).into();
            }
            if by == "role" {
                g["efforts"].inc(effort(row));
                if row["kind"].text() == Some("delegate") {
                    if row["patch_sha256"]
                        .bytes()
                        .is_some_and(|d| admitted.contains(d))
                    {
                        g.inc("admitted");
                    } else {
                        g.inc("not_admitted_results");
                        if let Some(tokens) = &measured.tokens {
                            g.add("not_admitted_tokens", tokens);
                        }
                    }
                }
            }
        }
    }
    let duration_total = if durations.is_empty() {
        V::from(0)
    } else {
        V::from(rounded(
            durations.iter().fold(0.0, |total, value| total + value),
            3,
        ))
    };
    durations.sort_by(f64::total_cmp);
    let median = if durations.is_empty() {
        None
    } else {
        let n = durations.len();
        Some(rounded(
            if n % 2 == 1 {
                durations[n / 2]
            } else {
                (durations[n / 2 - 1] + durations[n / 2]) / 2.0
            },
            3,
        ))
    };
    let cumulative_reports = routes
        .values()
        .map(|v| v["cumulative_reports_excluded"].count())
        .sum::<usize>();
    fallback["reasons"] = reasons
        .into_iter()
        .map(|(r, c)| obj!("reason":r,"count":c))
        .collect::<Vec<_>>()
        .into();
    let mut report = obj!(
        "schema":1,"action":"telemetry","semantic_outcome":if verification["recorded"].truth() {"mechanical-only"} else {"unavailable"},
        "window":obj!("scope":"retained-index-window","available_rows":rows.len(),"selected_rows":selected.len(),"limit":limit,"invalid_lines":0,
            "first_ts":selected.first().map_or_else(|| V::from(""),|r| r.get("ts").cloned().unwrap_or_else(|| "".into())),
            "last_ts":selected.last().map_or_else(|| V::from(""),|r| r.get("ts").cloned().unwrap_or_else(|| "".into()))),
        "operations":obj!("eligible":eligible.len(),"excluded":selected.len()-eligible.len()),
        "recorded_exit":exits,"verification":verification,"outcomes":outcomes,"fallback":fallback,"resolution":resolution,"coverage":coverage,
        "usage":obj!("prompt_bytes":obj!("reports":prompt_reports,"total":prompt_total.remove("value"),"basis":"recorded primary prompt only; excludes provider-added context, retries and repair prompts; not tokens"),
            "cumulative_reports_excluded":cumulative_reports,"provider_reported_tokens":obj!("reports":coverage_count(&routes),"total":token_total.remove("value")),
            "wall_seconds":obj!("reports":durations.len(),"total":duration_total,"median":median)),
        "routes":routes.into_values().collect::<Vec<_>>(),"native_activity":native(root)?);
    if bounded {
        report["window"]["since"] = options.since.clone().into();
        report["window"]["until"] = options.until.clone().into();
        report["window"]["timezone"] = "UTC".into();
        report["window"]["matched_rows"] = matched.len().into();
        report["window"]["undated_rows"] =
            rows.iter().filter(|r| stamp(r).is_none()).count().into();
    }
    if let Some(by) = &options.by {
        let mut warnings = Vec::new();
        let min_pct = env_int("OMS_TELEMETRY_CACHE_MIN_PCT", 80);
        let min_requests = env_int("OMS_TELEMETRY_CACHE_MIN_REQUESTS", 10);
        for g in groups.values_mut() {
            if !g["cost_reports"].truth() {
                g["cost_usd"] = V::Null;
            }
            if by == "role" {
                let denominator = g.remove("cache_denominator");
                let share = if denominator.truth() {
                    Some(rounded(ratio(&g["cache_read_tokens"], &denominator)?, 4))
                } else {
                    None
                };
                g["cache_read_share"] = share.into();
                g["tokens_per_admitted"] = if g["admitted"].truth() && g["token_reports"].truth() {
                    // round(x) uses ties-to-even, and returns an integer.
                    V::Int(format!(
                        "{:.0}",
                        ratio(&g["provider_reported_tokens"], &g["admitted"])?
                    ))
                } else {
                    V::Null
                };
                if share.is_some_and(|s| s * 100.0 < min_pct.number().unwrap())
                    && g["usage_reports"].number().unwrap() >= min_requests.number().unwrap()
                {
                    warnings.push(V::from(format!(
                        "cache-read share {:.1}% is under {}% for role {} ({} usage reports)",
                        share.unwrap() * 100.0,
                        crate::value::display(&min_pct),
                        crate::value::display(&g["group"]),
                        crate::value::display(&g["usage_reports"])
                    )));
                }
            }
        }
        report["usage_by"] = obj!("by":by.clone(),"timezone":"UTC","basis":"provider-reported values only; nothing is priced or estimated from tokens","capped":matched.len()>selected.len(),"omitted_rows":matched.len()-selected.len(),"groups":groups.into_values().collect::<Vec<_>>());
        if by == "role" {
            report["usage_by"]["admission_basis"]="delegate rows whose patch digest a later successful patch-admit/patch-land row carries; reported values, never approval or verification evidence".into();
            report["usage_by"]["cache_warn"] = obj!("min_pct":min_pct,"min_requests":min_requests);
            report["usage_by"]["warnings"] = warnings.into();
        }
    }
    let delegates: Vec<_> = selected
        .iter()
        .filter(|r| r["kind"].text() == Some("delegate"))
        .collect();
    let traces: Vec<_> = delegates
        .iter()
        .filter_map(|r| valid_trace(&r["repair_replay"]).then_some(&r["repair_replay"]))
        .collect();
    if !traces.is_empty() {
        let mut counts = obj!();
        let mut selected = 0;
        let mut early = 0;
        for trace in &traces {
            let policy = trace["policy"].text().unwrap();
            counts.inc(policy);
            if policy == "stop-repeat" {
                selected += 1;
                let steps = trace["steps"].array();
                let len = steps.len();
                if len >= 2
                    && steps[len - 1] == steps[len - 2]
                    && !(steps[len - 1]["worker"].int() == Some("0")
                        && steps[len - 1]["verify"].int() == Some("0"))
                    && len < trace["budget"].count() + 1
                {
                    early += 1;
                }
            }
        }
        report["repair_replay"] = obj!("recorded":traces.len(),"delegations_seen":delegates.len(),"selected":selected,"policy_counts":counts,"early_stops":early,
            "basis":"historical-prefix replay; not proof of future success","cost_basis":"worker attempts only; not measured token or wall-clock savings");
    }
    Ok(report)
}
type RouteKey = (Vec<u8>, Vec<u8>, Vec<u8>);
fn coverage_count(routes: &BTreeMap<RouteKey, V>) -> usize {
    routes.values().map(|r| r["token_reports"].count()).sum()
}
fn exact_keys(v: &V, keys: &[&str]) -> bool {
    matches!(v,V::Object(m) if m.len()==keys.len() && keys.iter().all(|k| v.get(k).is_some()))
}
fn valid_trace(v: &V) -> bool {
    exact_keys(
        v,
        &["schema", "cohort", "budget", "policy", "samples", "steps"],
    ) && v["schema"].int() == Some("1")
        && [Some("budget"), Some("stop-repeat")].contains(&v["policy"].text())
        && valid_digest(&v["cohort"])
        && v["budget"].int().is_some()
        && (1..=3).contains(&v["budget"].count())
        && v["samples"]
            .int()
            .is_some_and(|s| s.len() <= 4 && s.parse::<usize>().is_ok_and(|n| n <= 1000))
        && matches!(v["steps"], V::Array(_))
        && !v["steps"].array().is_empty()
        && v["steps"].array().len() <= v["budget"].count() + 1
        && v["steps"].array().iter().all(|s| {
            exact_keys(s, &["candidate", "failure", "worker", "verify"])
                && valid_digest(&s["candidate"])
                && valid_digest(&s["failure"])
                && ["worker", "verify"].iter().all(|k| {
                    s[k].int()
                        .is_some_and(|n| n.len() <= 3 && n.parse::<usize>().is_ok_and(|n| n <= 255))
                })
        })
}
