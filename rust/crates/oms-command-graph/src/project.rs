//! Regenerable repository graph built from safe source reads and native parsers.
#[path = "project_affected.rs"]
pub mod affected;
#[path = "project_analytics.rs"]
pub mod analytics;
#[path = "project_benchmark.rs"]
pub mod benchmark;
#[path = "project_build.rs"]
mod build;
#[path = "project_cases.rs"]
mod cases;
#[path = "project_context.rs"]
pub mod context;
#[path = "project_history.rs"]
pub mod history;
#[path = "project_index.rs"]
pub mod index;
#[path = "project_sqlite.rs"]
mod sqlite;
#[path = "project_unicode.rs"]
mod unicode;
#[path = "project_visual.rs"]
pub(crate) mod visual;

use crate::{CommandResult, Error, Result, cli::Args, util::*};
use index::Graph;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub use build::{BuildOptions, build, check, ensure, load};

fn summary(action: &str, value: &Value) -> String {
    let revision = s(value, "revision");
    let short: String = revision.chars().take(12).collect();
    if action == "fresh" {
        return format!("graph: fresh revision={short}");
    }
    let stats = &value["stats"];
    let skipped = value["skipped"]
        .as_array()
        .map_or_else(|| value["skipped"].as_u64().unwrap_or(0) as usize, Vec::len);
    format!(
        "graph: {action} revision={short} files={} nodes={} edges={} cached={} parsed={} skipped={skipped}",
        n(stats, "files"),
        n(stats, "nodes"),
        n(stats, "edges"),
        n(stats, "cached"),
        n(stats, "parsed")
    )
}
pub fn state_dir(repo: &Path) -> Result<PathBuf> {
    match std::env::var_os("OMS_PROJECT_GRAPH_STATE").filter(|s| !s.is_empty()) {
        Some(raw) => {
            let path = PathBuf::from(raw);
            if !path.is_absolute() {
                return Err(Error::new(
                    "OMS_PROJECT_GRAPH_STATE must name an absolute directory",
                ));
            }
            Ok(path)
        }
        None => Ok(repo.join(".oms/project-graph")),
    }
}
fn bounds(args: &Args) -> Result<()> {
    let action = args.action.as_str();
    let range = |field: &str, min: i64, max: i64| -> Result<()> {
        if !(min..=max).contains(&args.int(field)) {
            return Err(Error::new(format!(
                "project {action} --{} must be between {min} and {max}",
                field.replace('_', "-")
            )));
        }
        Ok(())
    };
    match action {
        "map" => {
            range("limit", 1, 200)?;
            range("depth", 0, 4)?;
            if args.flag("json") && args.flag("ui_model") {
                return Err(Error::new(
                    "project map cannot combine --json with a visual output",
                ));
            }
            if args.flag("brief")
                && (args.flag("json")
                    || args.flag("ui_model")
                    || !args.text("html_fragment").is_empty())
            {
                return Err(Error::new(
                    "project map --brief cannot combine with --json, --ui-model or --html-fragment",
                ));
            }
            if args.flag("signatures") {
                if args.text("task").trim().is_empty() && args.list("entry_path").is_empty() {
                    return Err(Error::new(
                        "project map --signatures requires --task TEXT or --entry-path PATH",
                    ));
                }
                if args.flag("brief")
                    || args.flag("ui_model")
                    || !args.text("html_fragment").is_empty()
                {
                    return Err(Error::new(
                        "project map --signatures cannot combine with --brief, --ui-model or --html-fragment",
                    ));
                }
                range("max_bytes", 256, 262144)?;
                range("max_files", 1, 200)?;
            } else if !args.text("task").trim().is_empty() || !args.list("entry_path").is_empty() {
                return Err(Error::new(
                    "project map --task and --entry-path require --signatures",
                ));
            }
        }
        "api" => range("limit", 1, 2000)?,
        "search" | "symbol" => range("limit", 1, 500)?,
        "impact" => {
            range("limit", 1, 500)?;
            if args.int("commits") < 1 {
                return Err(Error::new("project impact --commits must be at least 1"));
            }
            if args.int("depth") < 0 {
                return Err(Error::new("project impact --depth must be zero or greater"));
            }
        }
        "coupling"
            if args.int("commits") < 1
                || args.int("max_changeset") < 2
                || args.int("min_shared") < 1
                || args.float("min_degree") < 0.0
                || args.int("limit") < 0 =>
        {
            return Err(Error::new(
                "coupling: --commits >= 1, --max-changeset >= 2, --min-shared >= 1, --min-degree >= 0, --limit >= 0",
            ));
        }
        "unreferenced" if args.int("limit") < 0 => {
            return Err(Error::new("unreferenced: --limit >= 0"));
        }
        "blast" if args.int("limit") < 0 => {
            return Err(Error::new("project blast --limit must be zero or greater"));
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn dispatch(
    args: &Args,
    repo: &Path,
    _cwd: &Path,
    cancel: &AtomicBool,
    helper: Option<&Path>,
    runtime_services: Option<&dyn oms_runtime::Services>,
    host: &dyn crate::HostServices,
) -> Result<CommandResult> {
    bounds(args)?;
    if args.action == "benchmark" {
        return benchmark::dispatch(args, repo, _cwd, cancel, helper);
    }
    let state = state_dir(repo)?;
    let action = args.action.as_str();
    let mut err = String::new();
    let mut code = 0;
    let mut text = String::new();
    let value;
    if matches!(action, "build" | "ensure" | "check") {
        value = match action {
            "build" => build(
                repo,
                &state,
                &BuildOptions {
                    include: args.list("include"),
                    exclude: args.list("exclude"),
                    max_bytes: args.int("max_bytes"),
                    force: args.flag("force"),
                },
                cancel,
                helper,
            )?,
            "ensure" => ensure(repo, &state, args.int("max_files"), cancel, helper)?,
            _ => check(repo, &state, cancel)?,
        };
        match action {
            "build" => text = format!("{}\n", summary("built", &value)),
            "ensure" => text = format!("{}\n", summary(s(&value, "action"), &value)),
            _ => {
                if value["fresh"] == true {
                    text.push_str("fresh\n");
                } else {
                    code = 3;
                    if value["present"] != true {
                        text.push_str("absent: run: oms graph project build\n");
                    }
                    for label in ["stale", "missing", "new"] {
                        for path in array(&value, label) {
                            text.push_str(&format!("{label}: {}\n", path.as_str().unwrap_or("")));
                        }
                    }
                }
            }
        }
    } else {
        if !args.flag("no_refresh")
            && std::env::var("OMS_GRAPH_AUTOBUILD")
                .unwrap_or_default()
                .trim()
                != "0"
        {
            let refreshed = ensure(repo, &state, 20000, cancel, helper)?;
            if s(&refreshed, "action") != "fresh" {
                err.push_str(&format!(
                    "{}\n",
                    summary(s(&refreshed, "action"), &refreshed)
                ));
            }
        }
        let graph = Graph::new(load(&state)?)?;
        let coverage = read_json(&state.join("manifest.json"), build::GRAPH_LIMIT)?
            .unwrap_or(json!({}))["coverage"]
            .clone();
        value = match action {
            "find" => {
                let mut rows = graph.find(
                    args.text("query"),
                    args.text("kind"),
                    args.int("limit"),
                    args.flag("include_tests"),
                );
                if rows.is_empty() && args.int("limit") > 0 {
                    err.push_str("graph find: no lexical match; search source identifiers or paths directly. The query is not translated, and an empty result does not prove missing code.\n");
                }
                let assurance = analytics::assurance(
                    &graph,
                    args.flag("include_tests") || args.text("kind") == "test",
                );
                for row in &mut rows {
                    text.push_str(&format!(
                        "{}  {}  {}  {}\n",
                        s(row, "id"),
                        s(row, "kind"),
                        s(row, "path"),
                        n(row, "score")
                    ));
                    if let Some(details) = assurance["nodes"]
                        .get(s(row, "id"))
                        .and_then(Value::as_object)
                    {
                        row.as_object_mut().unwrap().extend(details.clone());
                        row["assurance_basis"] = json!("structural-evidence");
                    }
                }
                json!(rows)
            }
            "api" => {
                let result = graph.file_api(args.text("path"), args.int("limit"))?;
                text.push_str(&format!(
                    "path: {}\nlanguage: {}\n",
                    s(&result, "path"),
                    s(&result, "language")
                ));
                if let Some(summary) = result["summary"].as_str() {
                    text.push_str(&format!("summary: {summary}\n"));
                }
                for row in array(&result, "symbols") {
                    text.push_str(&format!(
                        "{}  {}  {}{}\n",
                        n(row, "line"),
                        s(row, "kind"),
                        s(row, "signature"),
                        row["summary"]
                            .as_str()
                            .map(|s| format!(" — {s}"))
                            .unwrap_or_default()
                    ));
                }
                result
            }
            "neighbors" => {
                let rows = graph.neighbors(
                    args.text("node"),
                    args.text("relation"),
                    args.text("direction"),
                )?;
                for row in &rows {
                    text.push_str(&format!(
                        "{}  {}  {}  {}\n",
                        s(row, "direction"),
                        s(row, "relation"),
                        s(row, "confidence"),
                        s(row, "id")
                    ));
                }
                json!(rows)
            }
            "trace" => {
                let result = graph.trace(
                    args.text("node"),
                    args.text("direction"),
                    args.int("depth"),
                    &[],
                    128,
                    160,
                )?;
                for row in array(&result, "nodes") {
                    text.push_str(&format!(
                        "{}{}{}\n",
                        "  ".repeat(n(row, "distance") as usize),
                        s(row, "id"),
                        if s(row, "via").is_empty() {
                            String::new()
                        } else {
                            format!("  via {}", s(row, "via"))
                        }
                    ));
                }
                result
            }
            "search" => {
                let result = graph.search(
                    repo,
                    args.text("query"),
                    args.int("limit"),
                    args.flag("ranked"),
                    cancel,
                )?;
                if args.flag("ranked") {
                    text.push_str(&format!(
                        "ranked search: {} of {} block(s) match {}{}\n",
                        n(&result, "returned"),
                        n(&result, "total_matches"),
                        array(&result, "terms")
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" "),
                        if result["truncated"] == true {
                            " (truncated)"
                        } else {
                            ""
                        }
                    ));
                    for row in array(&result, "results") {
                        text.push_str(&format!(
                            "{:7.3}  {}  [{}]\n         {}:{}: {}\n",
                            row["score"].as_f64().unwrap_or(0.0),
                            s(row, "id"),
                            array(row, "matched_terms")
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(" "),
                            s(row, "path"),
                            n(&row["preview"], "line"),
                            s(&row["preview"], "text")
                        ));
                    }
                } else {
                    text.push_str(&format!(
                        "search: {} hits in {} groups{}\n",
                        n(&result, "total_hits"),
                        array(&result, "groups").len(),
                        if result["truncated"] == true {
                            " (truncated)"
                        } else {
                            ""
                        }
                    ));
                    for row in array(&result, "groups") {
                        text.push_str(&format!(
                            "{}  {}  {} incoming\n",
                            s(row, "id"),
                            s(row, "path"),
                            n(row, "incoming")
                        ));
                        for hit in array(row, "hits") {
                            text.push_str(&format!("  L{}: {}\n", n(hit, "line"), s(hit, "text")));
                        }
                    }
                }
                result
            }
            "symbol" => {
                let result = graph.symbol(repo, args.text("name"), args.int("limit"), cancel)?;
                text = index::symbol_text(&result);
                result
            }
            "unreferenced" => {
                let result = graph.unreferenced(args.int("limit"), args.flag("include_tests"));
                text.push_str(&format!("{}\n", s(&result, "note")));
                for row in array(&result, "candidates") {
                    text.push_str(&format!(
                        "{}:{}  {}\n",
                        s(row, "path"),
                        n(row, "line"),
                        s(row, "id")
                    ));
                }
                result
            }
            "map" => {
                if args.flag("signatures") {
                    let result = context::signature_map(
                        repo,
                        &graph,
                        args.text("task"),
                        args.int("max_files"),
                        args.int("max_bytes"),
                        &args.list("entry_path"),
                        &coverage,
                        cancel,
                    )?;
                    text = s(&result, "text").into();
                    result
                } else if args.flag("brief") {
                    let result =
                        read_json(&state.join("orientation.json"), 16384)?.unwrap_or(json!({}));
                    text = format!("{}\n", s(&result, "text"));
                    result
                } else if args.flag("ui_model") || !args.text("html_fragment").is_empty() {
                    let result = graph.visual(
                        args.int("limit"),
                        args.int("depth"),
                        args.flag("include_tests"),
                        &coverage,
                        &[],
                        &[],
                        &[],
                        &[],
                    );
                    text = index::html(&result);
                    result
                } else {
                    let mut result = graph.map_summary(args.flag("include_tests"));
                    result["coverage"] = coverage.clone();
                    text = index::map_text(&result);
                    result
                }
            }
            "context" => {
                let mut pack = context::select(
                    repo,
                    &graph,
                    args.text("task"),
                    args.int("max_files"),
                    &args.list("entry_path"),
                    args.text("base"),
                    &coverage,
                    cancel,
                )?;
                context::persist(&state, repo, &mut pack)?;
                if s(&pack["retrieval"], "status") != "matched" {
                    err.push_str(&format!(
                        "graph context: {}\n",
                        s(&pack["retrieval"], "hint")
                    ));
                }
                if args.flag("bundle") {
                    pack["bundle"] = if let Some(services) = runtime_services {
                        let mut request =
                            oms_command_plan::Request::new(oms_command_plan::Action::Status);
                        request.repo = Some(repo.into());
                        let roots = oms_command_plan::Context::from_environment(&request, repo)?;
                        context::compile_bundle_with_host(
                            repo,
                            &roots.state_repo,
                            &pack,
                            64 * 1024,
                            host.install_root(),
                            services,
                            cancel,
                        )?
                    } else {
                        context::compile_bundle_with_root(
                            repo,
                            &pack,
                            64 * 1024,
                            host.install_root(),
                            cancel,
                        )?
                    };
                }
                if args.flag("ui_model") || !args.text("html_fragment").is_empty() {
                    let ids = array(&pack, "entries")
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    let paths = array(&pack, "files")
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    let result = graph.visual(
                        args.int("max_files").saturating_mul(6).clamp(48, 120),
                        2,
                        false,
                        &coverage,
                        &ids,
                        &paths,
                        &array(&pack["change"], "changed")
                            .iter()
                            .chain(array(&pack["change"], "untracked"))
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect::<Vec<_>>(),
                        &array(&pack["change"], "impacted")
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect::<Vec<_>>(),
                    );
                    text = index::html(&result);
                    result
                } else {
                    text = context::text(&pack);
                    pack
                }
            }
            "blast" => {
                let paths = if args.list("path").is_empty() {
                    affected::changed_paths(repo, args.text("base"), cancel)?
                } else {
                    args.list("path")
                };
                let mut result = graph.blast(&paths, Some(args.int("depth")), &[], &[])?;
                result["paths"] = json!(paths);
                result["changed_paths"] =
                    affected::workspace_changes(repo, args.text("base"), cancel)?;
                text = affected::blast_text(&result);
                if args.flag("json") && args.int("limit") > 0 {
                    affected::limit_blast(&mut result, args.int("limit") as usize);
                }
                result
            }
            "coupling" => {
                let result = history::report(
                    repo,
                    &graph,
                    args.int("commits"),
                    args.int("max_changeset"),
                    args.int("min_shared"),
                    args.float("min_degree"),
                    &args.list("path"),
                    args.int("limit"),
                    cancel,
                )?;
                text = history::text(&result);
                result
            }
            "affected" => {
                let result = affected::report_with_helper(
                    repo,
                    &graph,
                    args.text("base"),
                    args.text("head"),
                    args.int("depth"),
                    &args.list("guard"),
                    &args.list("runnable"),
                    cancel,
                    helper,
                )?;
                text = affected::text(&result);
                result
            }
            "impact" => {
                let result = affected::impact_with_helper(
                    repo,
                    &graph,
                    args.text("base"),
                    args.text("head"),
                    args.int("depth"),
                    args.int("commits"),
                    args.int("limit"),
                    cancel,
                    helper,
                )?;
                text = affected::text(&result);
                result
            }
            _ => return Err(Error::new(format!("unknown project action: {action}"))),
        };
    }
    let fragment = args.text("html_fragment");
    let mut value = value;
    if !fragment.is_empty() {
        let path = Path::new(fragment);
        if !path.is_absolute() {
            return Err(Error::new(
                "--html-fragment requires an absolute output path",
            ));
        }
        if text.len() > 1024 * 1024 {
            return Err(Error::new(
                "HTML fragment exceeds the 1 MiB agent-view limit",
            ));
        }
        write(path, text.as_bytes())?;
        text = format!("html_fragment: {}\n", path.display());
        value["html_fragment"] = json!(fragment);
    }
    let out = if args.flag("json") || args.flag("ui_model") {
        bytes(&value, args.flag("pretty"))?
    } else {
        text.into_bytes()
    };
    Ok((code, out, err.into_bytes()))
}
