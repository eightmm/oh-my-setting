use crate::common::*;
use crate::project;
use serde_json::{Value, json};
use std::ffi::OsString;

pub(crate) fn ml(raw: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let args = Args::parse(raw, &["--repo", "--ledger", "--max-bytes"], &["--force"])?;
    args.count(0)?;
    let root = ctx.repo(args.get("--repo"))?;
    let maximum = number(
        args.get("--max-bytes").unwrap_or("5000"),
        1,
        usize::MAX,
        "--max-bytes",
    )?;
    let style = project::style(&root)?;
    if style != "ml" && !args.has("--force") {
        return Ok(Output::text(""));
    }
    let mut text = format!(
        "# ML Agent Context Digest\n\n- generated: {}\n- project-style: {style}\n- repo-path: omitted\n- policy: raw data, checkpoints, private env, wandb/tensorboard logs, and machine paths are omitted\n\n## Project Spec\n\n",
        timestamp()
    );
    let spec = optional(&root.join("PROJECT.md"))?;
    if spec.is_empty() {
        text.push_str("No PROJECT.md found.\n");
    } else {
        let allowed = [
            "State",
            "Goal",
            "Type",
            "Scope",
            "Non-goals",
            "Success criteria",
            "Required checks",
            "Baseline/metric",
            "Task type",
            "Prediction unit",
            "Inference-time information boundary",
            "Entity IDs/standardization",
            "Source snapshot/provenance",
            "Label/target definition",
            "Label units/direction/censoring/replicates",
            "Negative provenance",
            "Split policy",
            "Split/group keys",
            "Leakage risks",
            "Train-only fitted transforms",
            "Data manifest",
            "Calibration/applicability-domain plan",
        ];
        let paths=regex::Regex::new(r"(?i)[A-Za-z][A-Za-z0-9+.-]*://|(?:^|\s)(?:(?:\.\.?|~)?[/\\]|[A-Za-z]:[/\\])\S*|(?:^|\s)(?:data|datasets|splits|private|outputs|checkpoints|runs|wandb|mnt|home|tmp)[/\\]\S*|\.(?:csv|tsv|json|jsonl|parquet|arrow|h5|hdf5|pkl|pt|pth|ckpt|np[yz])(?:\s|$)").expect("static path matcher");
        let mut count = 0;
        for line in String::from_utf8_lossy(&spec).lines() {
            if let Some((name, value)) = line.strip_prefix("- ").and_then(|s| s.split_once(':'))
                && allowed.contains(&name)
            {
                let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
                let value = if paths.is_match(&value) {
                    "[redacted path-or-URL]"
                } else {
                    &value
                };
                text.push_str(&format!(
                    "{}\n",
                    oms_contract_markdown::bounded_line(&format!("- {name}: {value}"), 240)
                ));
                count += 1;
                if count >= 30 {
                    break;
                }
            }
        }
    }
    text.push_str("\n## Data Manifests\n\n");
    let manifests = root.join(".oms/manifests");
    let mut found = false;
    if manifests.is_dir() {
        let mut files = std::fs::read_dir(manifests)?.collect::<std::io::Result<Vec<_>>>()?;
        files.sort_by_key(|e| e.file_name());
        for entry in files
            .iter()
            .filter(|p| p.path().extension().is_some_and(|s| s == "json"))
            .take(10)
        {
            if let Ok(bytes) = read(&entry.path(), LIMIT)
                && let Ok(row) = json(&bytes)
            {
                found = true;
                let splits = row["splits"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v["label"].as_str())
                    .collect::<Vec<_>>();
                let keys = match &row["leakage_keys"] {
                    Value::Array(a) => a
                        .iter()
                        .map(|v| v.as_str().unwrap_or(""))
                        .collect::<Vec<_>>(),
                    Value::Object(a) => a.keys().map(String::as_str).collect(),
                    _ => vec![],
                };
                text.push_str(&format!(
                    "- name={} id={} leakage_keys={} splits={}\n",
                    row["name"].as_str().unwrap_or("unnamed"),
                    row.get("id_column")
                        .or_else(|| row.get("id_index"))
                        .map(Value::to_string)
                        .unwrap_or_else(|| "unspecified".into()),
                    keys.join(","),
                    splits.join(",")
                ));
            }
        }
    }
    if !found {
        text.push_str("No data manifests found.\n");
    }
    let files = walk(
        &root,
        3,
        &[
            ".git",
            ".venv",
            "node_modules",
            "__pycache__",
            "data",
            "datasets",
            "outputs",
            "checkpoints",
            "wandb",
            "runs",
        ],
    )?;
    for title in ["Likely ML Entry Points", "Likely Config Files"] {
        text.push_str(&format!("\n## {title}\n\n"));
        let mut count = 0;
        for file in &files {
            let name = file.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let relative = file
                .strip_prefix(&root)
                .map_err(|_| error("file outside repository"))?;
            let selected = if title == "Likely ML Entry Points" {
                [
                    "train.py",
                    "eval.py",
                    "evaluate.py",
                    "infer.py",
                    "inference.py",
                    "model.py",
                    "dataset.py",
                    "dataloader.py",
                ]
                .contains(&name)
            } else {
                ["config.yaml", "config.yml", "pyproject.toml"].contains(&name)
                    || relative.starts_with("configs")
                        && matches!(
                            file.extension().and_then(|s| s.to_str()),
                            Some("yaml" | "yml" | "toml")
                        )
            };
            if selected {
                text.push_str(&format!("- {}\n", relative.display()));
                count += 1;
                if count >= 30 {
                    break;
                }
            }
        }
        if count == 0 {
            text.push_str("None found.\n");
        }
    }
    text.push_str("\n## Verification Contract\n\n");
    if root.join("scripts/check.sh").is_file() {
        text.push_str(
            "- scripts/check.sh exists\n- fallback fast check: bash scripts/check.sh fast\n",
        );
    } else {
        text.push_str("No executable scripts/check.sh found.\n");
    }
    text.push_str("\n## Recent Experiments\n\n");
    let ledger = args
        .get("--ledger")
        .map(|s| ctx.path(s))
        .unwrap_or_else(|| root.join("docs/EXPERIMENTS.jsonl"));
    let ledger = optional(&ledger)?;
    if ledger.is_empty() {
        text.push_str("No docs/EXPERIMENTS.jsonl ledger found.\n");
    } else if !clean(&ledger) {
        text.push_str("Experiment ledger omitted because it contains sensitive-looking content.\n");
    } else {
        let rows = ledger
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .rev()
            .take(8)
            .filter_map(|l| json(l).ok())
            .collect::<Vec<_>>();
        let completed = rows
            .iter()
            .filter(|r| !r["exit"].is_null())
            .filter_map(|r| r["id"].as_str())
            .collect::<std::collections::BTreeSet<_>>();
        for row in rows.iter().rev() {
            if row["exit"].is_null() && row["id"].as_str().is_some_and(|s| completed.contains(s)) {
                continue;
            }
            let command = row["cmd"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ");
            let result = if row["exit"].is_null() {
                "started".into()
            } else {
                format!("exit={} {}s", row["exit"], row["duration_s"])
            };
            text.push_str(&format!(
                "- {} {result} sha={}{} cmd={} note={}\n",
                row["ts"].as_str().unwrap_or("unknown"),
                row["git_sha"].as_str().unwrap_or("?"),
                if row["dirty"] == true { "+dirty" } else { "" },
                oms_contract_markdown::bounded_line(&command, 120),
                oms_contract_markdown::bounded_line(row["note"].as_str().unwrap_or(""), 100)
            ));
        }
    }
    text.push_str("\n## Omitted By Design\n\n- data/, datasets/, outputs/, checkpoints/, wandb/, runs/\n- .env*, credentials, private keys, absolute machine paths, cluster/node details\n");
    if !clean(text.as_bytes()) {
        return Ok(Output {
            code: 0,
            out: vec![],
            err: b"warning: ML context omitted because digest contains sensitive-looking content\n"
                .to_vec(),
        });
    }
    let mut bytes = text.into_bytes();
    if bytes.len() > maximum {
        bytes.truncate(maximum);
        bytes.extend_from_slice(
            format!("\n\n... ML context truncated at {maximum} bytes.\n").as_bytes(),
        );
    }
    Ok(Output {
        code: 0,
        out: bytes,
        err: vec![],
    })
}
pub(crate) fn import(raw: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let args = Args::parse(
        raw,
        &[
            "--kind",
            "--provider",
            "--file",
            "--prompt-file",
            "--repo",
            "--artifact-dir",
            "--exit",
        ],
        &[],
    )?;
    args.count(0)?;
    let kind = args.required("--kind")?;
    if !["ask", "review", "delegate", "call"].contains(&kind) {
        return Err(error("--kind must be ask, review, delegate, or call"));
    }
    let provider = crate::models::normalize(args.required("--provider")?, ctx.cwd)?;
    let bytes = read(&ctx.path(args.required("--file")?), LIMIT)?;
    if bytes.is_empty() {
        return Err(error("result file is empty"));
    }
    let prompt = args
        .get("--prompt-file")
        .map(|s| read(&ctx.path(s), LIMIT))
        .transpose()?;
    let exit = number(
        args.get("--exit").unwrap_or("0"),
        0,
        i32::MAX as usize,
        "--exit",
    )?;
    let root = ctx.repo(args.get("--repo"))?;
    let directory = args
        .get("--artifact-dir")
        .map(|s| ctx.path(s))
        .unwrap_or_else(|| root.join(".oms/artifacts").join(kind));
    let suffix = crate::https::random_hex(12)?;
    let path = directory.join(format!(
        "{provider}-imported-result-{}-{suffix}.import.md",
        timestamp().replace(['-', ':'], "")
    ));
    let mut output = format!(
        "# {provider} {kind} import\n\n- imported: {}\n- repo: {}\n\n## Prompt\n\n",
        timestamp(),
        root.file_name()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default()
    )
    .into_bytes();
    output.extend_from_slice(
        prompt
            .as_deref()
            .unwrap_or(b"Prompt not supplied at import time.\n"),
    );
    output.extend_from_slice(b"\n\n## Output\n\n");
    output.extend_from_slice(&bytes);
    output.extend_from_slice(format!("\n\n## Exit\n\n{exit}\n").as_bytes());
    write(&root.join(".oms/.gitignore"), b"*\n", true)?;
    let index = env_path(
        "OMS_ARTIFACT_INDEX",
        root.join(".oms/artifacts/index.jsonl"),
    );
    let _guard = lock(&index)?;
    write(&path, &output, true)?;
    let mut row = json!({"schema":1,"event_id":format!("evt_{suffix}"),"operation_id":format!("import-{suffix}"),"ts":timestamp(),"kind":format!("{kind}-import"),"provider":provider,"exit":exit});
    match path.strip_prefix(&root) {
        Ok(relative) => {
            row["artifact"] = json!(relative);
            row["artifact_sha256"] = json!(digest(&output));
        }
        Err(_) => {
            row["artifact_external"] = json!({"name":path.file_name().map(|s|s.to_string_lossy()),"owned":false,"sha256":digest(&output)});
        }
    }
    if let Some(prompt) = prompt {
        row["prompt_sha256"] = json!(digest(&prompt));
    }
    let prior = optional(&index)?;
    let mut updated = prior.clone();
    updated.extend_from_slice(row.to_string().as_bytes());
    updated.push(b'\n');
    write_locked(&index, &updated, false, Some(&prior))?;
    let mut out = Output::text(format!("imported: {provider} -> {}\n", path.display()));
    if !clean(&bytes) {
        out.err=b"warning: imported result contains sensitive-looking content; review before sharing or promoting to memory\n".to_vec();
    }
    Ok(out)
}
pub(crate) fn slurm(raw: &[OsString], ctx: &Context<'_>) -> Result<Output> {
    let args = Args::parse(raw, &[], &["--check", "--dry-run"])?;
    args.count(0)?;
    if args.has("--check") && args.has("--dry-run") {
        return Err(error("choose --check or --dry-run"));
    }
    let path = env_path("OH_MY_SETTING_SLURM_REF", ctx.source.join("local/slurm.md"));
    if args.has("--check") {
        let raw = read(&path, LIMIT)?;
        let text = String::from_utf8_lossy(&raw);
        let valid = text.lines().any(|l| l == "Schema: 1")
            && text
                .lines()
                .any(|l| l.starts_with("Generated: ") && l.len() >= 24)
            && text.contains("## Effective Submission Defaults");
        let mut out = Output::text(format!(
            "{}Slurm snapshot: {} ({})\n",
            if valid { "" } else { "error: " },
            if valid { "ok" } else { "invalid" },
            path.display()
        ));
        out.code = i32::from(!valid);
        return Ok(out);
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
    let host = ctx
        .command("hostname", &[], 10)
        .map(|o| String::from_utf8_lossy(&o.out).trim().to_owned())
        .unwrap_or_default();
    let probe = |bin: &str, args: &[&str]| -> Result<String> {
        let output = ctx.command(bin, args, 30)?;
        if output.code == 0 {
            Ok(String::from_utf8_lossy(&output.out).into_owned())
        } else {
            Ok(String::new())
        }
    };
    let partitions = probe("scontrol", &["-o", "show", "partition"])?;
    let nodes = probe("scontrol", &["-o", "show", "node"])?;
    let associations = probe(
        "sacctmgr",
        &[
            "-nP",
            "show",
            "assoc",
            &format!("user={user}"),
            "format=Cluster,Account,User,Partition,DefaultQOS,QOS,GrpTRES,MaxTRES,MaxTRESPJ,MaxJobs,MaxSubmit,MaxWall",
        ],
    )?;
    let qos = probe(
        "sacctmgr",
        &[
            "-nP",
            "show",
            "qos",
            "format=Name,Priority,Preempt,PreemptMode,Flags,UsageFactor,GrpTRES,MaxTRESPU,MaxTRESPJ,MaxWall,MaxJobsPU,MaxSubmitPU",
        ],
    )?;
    let info = probe("sinfo", &["-h", "-o", "%P|%a|%l|%D|%G|%m|%c|%N"])?;
    let queue = probe(
        "squeue",
        &["-u", &user, "-o", "%.18i %.9P %.32j %.2t %.10M %.6D %R"],
    )?;
    let mut text = format!(
        "# Local Slurm Cluster\n\nSchema: 1\nGenerated: {}\nHost: {host}\nUser: {user}\n\nUse this as local reference only. Do not commit generated cluster details.\n\n## Summary\n\n",
        timestamp()
    );
    if info.is_empty() {
        text.push_str("- Slurm not detected.\n");
    }
    text.push_str("\n## Partitions\n\nName|State|Time|Nodes|CPUs/node|Mem|GPU/GRES|NodeList\n---|---|---|---:|---:|---:|---|---\n");
    let mut default = String::new();
    for line in info.lines() {
        let fields = line.split('|').collect::<Vec<_>>();
        if fields.len() < 8 {
            continue;
        }
        if fields[0].contains('*') {
            default = fields[0].replace('*', "");
        }
        text.push_str(&format!(
            "{}|{}|{}|{}|{}|{}|{}|{}\n",
            fields[0].replace('*', ""),
            fields[1],
            fields[2],
            fields[3],
            fields[6],
            fields[5],
            if fields[4] == "(null)" {
                "-"
            } else {
                fields[4]
            },
            fields[7]
        ));
    }
    for (title, body) in [
        ("Partition Configuration", &partitions),
        ("Node Configuration", &nodes),
        ("Associations (current user)", &associations),
        ("QOS", &qos),
        ("Current Queue", &queue),
    ] {
        text.push_str(&format!("\n## {title}\n\n"));
        if body.is_empty() {
            text.push_str(
                "Unavailable (command missing, accounting disabled, or access denied).\n",
            );
        } else {
            text.push_str(&format!("```text\n{body}```\n"));
        }
    }
    let value = |record: &str, name: &str| {
        record
            .split_whitespace()
            .find_map(|w| {
                w.split_once('=')
                    .filter(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_owned())
            })
            .unwrap_or_default()
    };
    if default.is_empty() {
        for record in partitions.lines() {
            if value(record, "Default") == "YES" {
                default = value(record, "PartitionName");
                break;
            }
        }
    }
    let record = partitions
        .lines()
        .find(|r| value(r, "PartitionName") == default)
        .unwrap_or("");
    let defaults = value(record, "JobDefaults");
    let assoc = associations
        .lines()
        .map(|l| l.split('|').collect::<Vec<_>>())
        .filter(|r| r.len() > 5)
        .collect::<Vec<_>>();
    let matched = assoc.iter().filter(|r| r[3] == default).collect::<Vec<_>>();
    let matched = if matched.is_empty() {
        assoc.iter().filter(|r| r[3].is_empty()).collect::<Vec<_>>()
    } else {
        matched
    };
    let accounts = matched
        .iter()
        .map(|r| r[1])
        .filter(|s| !s.is_empty())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    let qos_default = matched
        .iter()
        .map(|r| r[4])
        .filter(|s| !s.is_empty())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    let cpu = defaults
        .split(',')
        .filter(|s| s.starts_with("DefCpu"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut memory = vec![];
    for name in ["DefMemPerCPU", "DefMemPerNode", "DefMemPerGPU"] {
        let v = value(record, name);
        if !v.is_empty() {
            memory.push(format!("{name}={v}"));
        }
    }
    memory.extend(
        defaults
            .split(',')
            .filter(|s| s.starts_with("DefMem"))
            .map(str::to_owned),
    );
    let empty = |s: String| {
        if s.is_empty() {
            "(not configured)".into()
        } else {
            s
        }
    };
    text.push_str("\n## Effective Submission Defaults\n\nValues below are copied from Slurm configuration/associations; missing values are not inferred.\n\n");
    for (name, content) in [
        ("Preferred partition", default),
        ("Account", accounts),
        (
            "QOS default",
            if qos_default.is_empty() {
                value(record, "QoS")
            } else {
                qos_default
            },
        ),
        ("CPU default", cpu),
        ("GPU default", String::new()),
        ("Memory default", memory.join(", ")),
        ("Time default", value(record, "DefaultTime")),
    ] {
        text.push_str(&format!("- {name}: {}\n", empty(content)));
    }
    text.push_str("- Log path:\n- Checkpoint path:\n");
    if args.has("--dry-run") {
        return Ok(Output::text(text));
    }
    let _guard = lock(&path)?;
    write_locked(&path, text.as_bytes(), false, None)?;
    if std::env::var("OH_MY_SETTING_SLURM_WRITE_RAW").as_deref() == Ok("1") {
        let directory = env_path(
            "OH_MY_SETTING_SLURM_RAW_DIR",
            ctx.source.join("local/slurm"),
        );
        for (name, body) in [
            ("sinfo.txt", info),
            ("partitions.txt", partitions),
            ("nodes.txt", nodes),
            ("associations-current-user.psv", associations),
            ("qos.psv", qos),
        ] {
            write(&directory.join(name), body.as_bytes(), false)?;
        }
    }
    Ok(Output::text(format!("wrote {}\n", path.display())))
}
