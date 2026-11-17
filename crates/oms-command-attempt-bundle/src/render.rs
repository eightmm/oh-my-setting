use crate::value::V;
fn line(item: &V, keys: &[&str]) -> String {
    keys.iter()
        .filter(|k| item.get(k) != &V::Null)
        .map(|k| format!("{k}={}", item.get(k).py()))
        .collect::<Vec<_>>()
        .join(" ")
}
fn default(item: &V, key: &str, value: &str) -> String {
    item.default(key, value).py()
}
pub(crate) fn text(b: &V) -> Result<Vec<u8>, String> {
    let mut out = vec![
        format!("Attempt post-mortem bundle: {}", b.get("attempt_id").py()),
        b.get("notice").py(),
        format!(
            "provider/tool: {}/{}  run/task: {}/{}",
            b.get("provider").or("-").py(),
            b.get("tool").or("-").py(),
            b.get("run_id").or("-").py(),
            b.get("task_id").or("-").py()
        ),
        String::new(),
    ];
    let life = b.get("lifecycle");
    out.push(format!(
        "Lifecycle [{}]: state={} terminal={} reason={} events={} longest_gap_s={}",
        life.get("source").py(),
        life.get("state").py(),
        life.get("terminal").py(),
        life.get("reason_code").or("-").py(),
        life.get("events_total").py(),
        life.get("longest_gap_s").py()
    ));
    for e in life.get("events").array() {
        out.push(format!(
            "  #{} {} {} {}->{} {}",
            e.get("seq").py(),
            e.get("ts").py(),
            e.get("event_type").py(),
            default(e, "from_state", "-"),
            default(e, "to_state", "-"),
            default(e, "reason_code", "")
        ));
    }
    let art = b.get("artifact_rows");
    out.extend([
        String::new(),
        format!(
            "Artifact-index rows [{}]: {}",
            art.get("source").py(),
            default(art, "rows_total", "unreadable")
        ),
    ]);
    for row in art.get("rows").array() {
        out.push(format!(
            "  {}",
            line(
                row,
                &[
                    "event_id",
                    "ts",
                    "kind",
                    "provider",
                    "exit",
                    "verify_exit",
                    "linked_by",
                    "role",
                    "selected_model",
                    "model_class",
                    "fallback_used",
                    "answer_quality",
                    "prompt_sha256",
                    "status"
                ]
            )
        ));
        if row.get("result_lint").truth() {
            let values = match row.get("result_lint") {
                V::Arr(a) => a.iter().map(V::py).collect::<Vec<_>>(),
                V::Str(s) => std::str::from_utf8(s)
                    .map_err(|_| "cannot render string")?
                    .chars()
                    .map(|c| c.to_string())
                    .collect(),
                _ => return Err("result_lint is not iterable".into()),
            };
            out.push(format!("    result_lint: {}", values.join(",")));
        }
        if row.get("review").truth() {
            out.push(format!(
                "    review: {}",
                row.get("review").default_json(true)?
            ));
        }
    }
    let fail = b.get("fail_ledger");
    out.extend([
        String::new(),
        format!(
            "Fail-ledger rows [{}]: {} ({})",
            fail.get("source").py(),
            default(fail, "rows_total", "unreadable"),
            default(fail, "match", "")
        ),
    ]);
    for row in fail.get("rows").array() {
        out.push(format!(
            "  {}",
            line(row, &["line", "ts", "kind", "event", "exit", "fingerprint"])
        ));
    }
    let t = b.get("transcript_timeline");
    out.push(String::new());
    if !t.get("available").truth() {
        out.push(format!(
            "Transcript timeline: not available ({})",
            default(t, "reason", "not read")
        ));
    } else {
        let first = t.get("first_error");
        out.push(format!("Transcript timeline [{}]: steps={} errors={} pending={} first_error_step={} steps_after={}",t.get("source").py(),t.get("steps_total").py(),t.get("errors").py(),t.get("pending").py(),if first.truth(){first.get("step").py()}else{"-".into()},t.get("steps_after_first_error").py()));
        for step in t.get("steps").array() {
            let status = if step.get("error").truth() {
                "error"
            } else if step.get("ok").truth() {
                "ok"
            } else {
                "no-result"
            };
            let repeat = if step.get("repeat_of").truth() {
                format!(" repeat_of={}", step.get("repeat_of").py())
            } else {
                String::new()
            };
            let mark = if first.truth() && step.get("step") == first.get("step") {
                " FIRST ERROR"
            } else {
                ""
            };
            out.push(format!(
                "  {} {} {} duration_ms={} arg_bytes={} result_bytes={}{}{}",
                step.get("step").py(),
                step.get("tool").py(),
                status,
                step.get("duration_ms").py(),
                step.get("argument_bytes").py(),
                step.get("result_bytes").py(),
                repeat,
                mark
            ));
        }
        if t.get("steps_omitted").truth() {
            out.push(format!(
                "  ({} more steps; use context-inspect --session PATH --timeline)",
                t.get("steps_omitted").py()
            ));
        }
    }
    let mut bytes = out.join("\n").into_bytes();
    bytes.push(b'\n');
    Ok(bytes)
}
