//! Read-only metric observations; staleness is never process liveness evidence.
use crate::json::{number, object, string};
use crate::{Context, Result, Value, error, helpers, json, require, storage};
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
#[derive(Default)]
struct Series {
    count: usize,
    recent: VecDeque<(i64, f64)>,
    nonfinite: usize,
    first: Option<i64>,
    minimum: Option<(i64, f64)>,
    maximum: Option<(i64, f64)>,
    last_ts: Option<f64>,
}
impl Series {
    fn add(&mut self, step: i64, value: f64, ts: Option<f64>) {
        self.count += 1;
        if let Some(ts) = ts {
            self.last_ts = Some(self.last_ts.map_or(ts, |t| t.max(ts)));
        }
        if value.is_finite() {
            if self.minimum.is_none_or(|(_, v)| value < v) {
                self.minimum = Some((step, value));
            }
            if self.maximum.is_none_or(|(_, v)| value > v) {
                self.maximum = Some((step, value));
            }
        } else {
            self.nonfinite += 1;
            self.first = self.first.or(Some(step));
        }
        self.recent.push_back((step, value));
        if self.recent.len() > 10 {
            self.recent.pop_front();
        }
    }
    fn slope(&self) -> Option<f64> {
        let points = self
            .recent
            .iter()
            .filter(|(_, v)| v.is_finite())
            .collect::<Vec<_>>();
        if points.len() < 3 {
            return None;
        }
        let xs = points.iter().map(|(s, _)| *s as f64).collect::<Vec<_>>();
        let ys = points.iter().map(|(_, v)| *v).collect::<Vec<_>>();
        let mx = oms_command_runs::statistics::mean(&xs)?;
        let my = oms_command_runs::statistics::mean(&ys)?;
        let den = xs.iter().map(|x| (x - mx).powi(2)).sum::<f64>();
        if den == 0.0 {
            None
        } else {
            Some(
                xs.iter()
                    .zip(&ys)
                    .map(|(x, y)| (x - mx) * (y - my))
                    .sum::<f64>()
                    / den,
            )
        }
    }
}
fn numeric(value: &Value) -> Option<f64> {
    match value {
        Value::Number(s) => s.parse().ok(),
        Value::String(_) => value.str().ok().and_then(|s| s.parse().ok()),
        _ => None,
    }
}
fn stamp(value: &Value) -> Option<f64> {
    let n = numeric(value).or_else(|| {
        value
            .str()
            .ok()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.timestamp_millis() as f64 / 1000.0)
    })?;
    n.is_finite()
        .then_some(if n > 1e11 { n / 1000.0 } else { n })
}
/// Summarize JSONL or DVCLive TSV, keeping only ten recent points per metric.
pub fn gather(target: &Path) -> Result<Value> {
    let target = helpers::absolute(&std::env::current_dir().map_err(|e| error(2, e))?, target);
    require(target.exists(), "metrics source does not exist")?;
    let stop = std::sync::atomic::AtomicBool::new(false);
    let root = target.parent().unwrap_or(Path::new("/")).to_owned();
    let ctx = Context {
        cwd: root.clone(),
        repo: root.clone(),
        state_repo: root,
        runtime: None,
        interrupted: &stop,
    };
    let paths = if target.is_dir() {
        helpers::files(&ctx, &target, ".tsv", 50000)?
    } else {
        vec![target]
    };
    let mut series: BTreeMap<String, Series> = BTreeMap::new();
    let mut skipped = 0;
    for path in &paths {
        let bytes = storage::input(path, storage::MAX_STREAM)?;
        let text =
            std::str::from_utf8(&bytes).map_err(|_| error(2, "metrics input is not UTF-8"))?;
        if path.extension().is_some_and(|s| s == "tsv") {
            let mut lines = text.lines();
            let header = lines.next().unwrap_or("").split('\t').collect::<Vec<_>>();
            let Some(si) = header.iter().position(|s| *s == "step") else {
                skipped += 1;
                continue;
            };
            let ti = header.iter().position(|s| *s == "timestamp");
            let columns = header
                .iter()
                .enumerate()
                .filter(|(_, n)| !["step", "timestamp"].contains(n))
                .collect::<Vec<_>>();
            for line in lines {
                let fields = line.split('\t').collect::<Vec<_>>();
                if fields.len() != header.len() {
                    skipped += 1;
                    continue;
                }
                let Some(step) = fields[si].parse::<f64>().ok().filter(|v| v.is_finite()) else {
                    skipped += 1;
                    continue;
                };
                let ts = ti.and_then(|i| stamp(&string(fields[i])));
                for (i, name) in &columns {
                    let name = if columns.len() == 1 && ["", "value"].contains(name) {
                        path.file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    } else {
                        (**name).to_owned()
                    };
                    if let Ok(value) = fields[*i].parse::<f64>() {
                        series.entry(name).or_default().add(step as i64, value, ts);
                    } else {
                        skipped += 1;
                    }
                }
            }
        } else {
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                let Ok(row) = json::parse_legacy(line.as_bytes(), storage::MAX_FILE) else {
                    skipped += 1;
                    continue;
                };
                let (Some(value), Some(step)) = (
                    numeric(row.get("value")),
                    numeric(row.get("step")).filter(|v| v.is_finite()),
                ) else {
                    skipped += 1;
                    continue;
                };
                if row.text("name").is_empty() {
                    skipped += 1;
                    continue;
                }
                require(
                    series.len() < 10000 || series.contains_key(row.text("name")),
                    "metric series limit exceeded",
                )?;
                series.entry(row.text("name").into()).or_default().add(
                    step as i64,
                    value,
                    stamp(row.get("ts")),
                );
            }
        }
    }
    let mut rows = Vec::new();
    for (name, item) in series {
        let lower = ["loss", "err", "perplex", "ppl", "mae", "mse", "rmse", "nll"]
            .iter()
            .any(|s| name.to_lowercase().contains(s));
        let best = if lower { item.minimum } else { item.maximum };
        let (last_step, latest) = item.recent.back().copied().unwrap_or((0, f64::NAN));
        let finite = item
            .recent
            .iter()
            .filter_map(|(_, v)| v.is_finite().then_some(*v))
            .collect::<Vec<_>>();
        rows.push(object([
            ("name", string(name)),
            ("records", number(item.count)),
            ("last_step", number(last_step)),
            (
                "latest",
                if latest.is_finite() {
                    number(latest)
                } else {
                    string(latest.to_string())
                },
            ),
            ("best", best.map(|(_, v)| number(v)).unwrap_or(Value::Null)),
            (
                "best_step",
                best.map(|(s, _)| number(s)).unwrap_or(Value::Null),
            ),
            ("direction", string(if lower { "min" } else { "max" })),
            ("slope", item.slope().map(number).unwrap_or(Value::Null)),
            (
                "stdev",
                oms_command_runs::statistics::stdev(&finite)
                    .map(number)
                    .unwrap_or(Value::Null),
            ),
            ("nonfinite", number(item.nonfinite)),
            (
                "first_nonfinite",
                item.first.map(number).unwrap_or(Value::Null),
            ),
            ("last_ts", item.last_ts.map(number).unwrap_or(Value::Null)),
        ]));
    }
    Ok(object([
        ("files", number(paths.len())),
        ("skipped", number(skipped)),
        ("series", Value::Array(rows)),
    ]))
}
pub fn render(target: &Path, max_series: usize, now: f64) -> Result<String> {
    let report = gather(target)?;
    let series = report.get("series").array()?;
    let mut out = format!(
        "## Metrics\n\n- source: {} ({} file(s), {} record(s) skipped as malformed)\n",
        target.display(),
        helpers::display(report.get("files")),
        helpers::display(report.get("skipped"))
    );
    if series.is_empty() {
        out.push_str("\nNo metric records found: not evidence that the run made no progress.\n");
        return Ok(out);
    }
    let latest = series
        .iter()
        .filter_map(|r| numeric(r.get("last_ts")))
        .reduce(f64::max);
    if let Some(ts) = latest {
        out.push_str(&format!("- last record: {:.0}s ago (file observation only; not proof the run is alive or dead)\n",now-ts));
    } else {
        out.push_str("- last record: no timestamp in the file; staleness cannot be read\n");
    }
    out.push_str("\n| metric | records | last step | latest | best (rule) | slope/step (last 10) | non-finite |\n|---|---|---|---|---|---|---|\n");
    for row in series.iter().take(max_series.max(1)) {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} @{} ({}) | {} | {} |\n",
            row.text("name"),
            helpers::display(row.get("records")),
            helpers::display(row.get("last_step")),
            helpers::display(row.get("latest")),
            helpers::display(row.get("best")),
            helpers::display(row.get("best_step")),
            row.text("direction"),
            helpers::display(row.get("slope")),
            helpers::display(row.get("nonfinite"))
        ));
    }
    if series.len() > max_series.max(1) {
        out.push_str(&format!(
            "\n{} more metric(s) not shown.\n",
            series.len() - max_series.max(1)
        ));
    }
    out.push_str("\nBest uses min for names containing loss/err/ppl/mae/mse/nll, otherwise max; check the direction for your metric.\n");
    Ok(out)
}
