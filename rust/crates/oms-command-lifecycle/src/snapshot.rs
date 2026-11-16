use crate::{
    cli::Options,
    receipt::Paths,
    runtime::{self as r, Result},
    transaction::Transaction,
};
use std::{env, path::PathBuf, sync::atomic::AtomicBool, time::Duration};
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}
fn probe(paths: &Paths, name: &str, args: &[&str], flag: &AtomicBool) -> Option<String> {
    let program = r::find(name, &paths.cwd)?;
    let output = r::command(
        program,
        &args
            .iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>(),
        &paths.cwd,
        None,
        flag,
        Duration::from_secs(5),
    )
    .ok()?;
    if output.0 != 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&output.1).trim().to_owned())
}
pub(crate) fn output_path(paths: &Paths) -> Result<PathBuf> {
    r::leaf(
        &env::var_os("OH_MY_SETTING_MACHINE_SNAPSHOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| paths.source.join("local/machine.md")),
    )
}
pub(crate) fn valid(raw: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(raw) else {
        return false;
    };
    s.lines().any(|s| s == "- Schema: 1")
        && s.lines().any(|s| {
            s.strip_prefix("- Updated: ")
                .is_some_and(|s| chrono::DateTime::parse_from_rfc3339(s).is_ok())
        })
        && s.contains("## Local Agent CLI Paths")
}
pub(crate) fn render(paths: &Paths, flag: &AtomicBool) -> Result<String> {
    r::check(flag)?;
    let host = rustix::system::uname();
    let os_file = env::var_os("OMS_OS_RELEASE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/os-release"));
    let cpu_file = env::var_os("OMS_CPUINFO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/proc/cpuinfo"));
    let os = std::fs::read_to_string(os_file)
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("PRETTY_NAME=")
                    .map(|s| s.trim_matches('"').to_owned())
            })
        })
        .or_else(|| {
            if cfg!(target_os = "macos") {
                probe(paths, "sw_vers", &["-productVersion"], flag).map(|s| format!("macOS {s}"))
            } else {
                None
            }
        })
        .unwrap_or_else(|| host.sysname().to_string_lossy().into_owned());
    let cpu = std::fs::read_to_string(cpu_file)
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("model name")
                    .and_then(|s| s.split_once(':'))
                    .map(|(_, v)| v.trim().to_owned())
            })
        })
        .or_else(|| probe(paths, "sysctl", &["-n", "machdep.cpu.brand_string"], flag))
        .unwrap_or_else(|| host.machine().to_string_lossy().into_owned());
    let memory = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("MemTotal:")
                    .and_then(|s| s.split_whitespace().next())
                    .and_then(|s| s.parse::<u64>().ok())
            })
        })
        .map(|n| n * 1024)
        .or_else(|| {
            probe(paths, "sysctl", &["-n", "hw.memsize"], flag).and_then(|s| s.parse().ok())
        })
        .map(|n| format!("{:.1} GiB", n as f64 / 1073741824.0))
        .unwrap_or_else(|| "unknown".into());
    let gpu = probe(
        paths,
        "nvidia-smi",
        &["--query-gpu=name,memory.total", "--format=csv,noheader"],
        flag,
    )
    .or_else(|| {
        probe(
            paths,
            "rocm-smi",
            &["--showproductname", "--showmeminfo", "vram"],
            flag,
        )
    })
    .or_else(|| {
        if cfg!(target_os = "macos") {
            probe(paths, "system_profiler", &["SPDisplaysDataType"], flag).map(|s| {
                s.lines()
                    .filter_map(|l| l.trim().strip_prefix("Chipset Model:"))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        } else {
            None
        }
    })
    .unwrap_or_else(|| "none detected".into());
    let driver = probe(
        paths,
        "nvidia-smi",
        &["--query-gpu=driver_version", "--format=csv,noheader"],
        flag,
    )
    .and_then(|s| s.lines().next().map(str::to_owned))
    .unwrap_or_else(|| "n/a".into());
    let cuda = probe(paths, "nvcc", &["--version"], flag)
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.split_once("release ")
                    .and_then(|(_, s)| s.split(',').next())
                    .map(str::to_owned)
            })
        })
        .unwrap_or_else(|| "n/a".into());
    let storage = rustix::fs::statvfs(&paths.home)
        .ok()
        .map(|v| {
            let unit = v.f_frsize as u128;
            let total = v.f_blocks as u128 * unit;
            let available = v.f_bavail as u128 * unit;
            format!(
                "{:.1} GiB available / {:.1} GiB total",
                available as f64 / 1073741824.0,
                total as f64 / 1073741824.0
            )
        })
        .unwrap_or_else(|| "unknown".into());
    let mut body = format!(
        "# Machine Snapshot\n\nCompact local compute snapshot. Do not commit if it contains private details.\n\n- Schema: 1\n- Updated: {}\n- Host label: {}\n- OS: {}\n- Kernel: {}\n- CPU: {}\n- CPU cores: {}\n- RAM: {}\n- Home storage: {}\n- GPU:\n",
        r::now(),
        clean(&env::var("OH_MY_SETTING_HOST_LABEL").unwrap_or_else(|_| "local".into())),
        clean(&os),
        clean(&host.release().to_string_lossy()),
        clean(&cpu),
        std::thread::available_parallelism()
            .map(|n| n.get().to_string())
            .unwrap_or_else(|_| "unknown".into()),
        memory,
        storage
    );
    for line in gpu.lines().take(16) {
        body.push_str(&format!("  - {}\n", clean(line)));
    }
    body.push_str(&format!(
        "- NVIDIA driver: {}\n- CUDA: {}\n",
        clean(&driver),
        clean(&cuda)
    ));
    {
        let (name, args) = ("uv", &["--version"][..]);
        let value = probe(paths, name, args, flag).unwrap_or_else(|| "not detected".into());
        body.push_str(&format!("- {name}: {}\n", clean(&value)));
    }
    body.push_str(&format!(
        "- Slurm: {}\n- tsp queue: {}\n\n## Local Agent CLI Paths\n\n",
        if r::find("sinfo", &paths.cwd).is_some() {
            "available"
        } else {
            "not detected"
        },
        if r::find("tsp", &paths.cwd).is_some() {
            "available (not probed)"
        } else {
            "not detected"
        }
    ));
    for (label, name) in [
        ("Codex CLI", "codex"),
        ("Claude Code CLI", "claude"),
        ("Antigravity CLI", "agy"),
        ("gh", "gh"),
    ] {
        body.push_str(&format!(
            "- {label}: {}\n",
            r::find(name, &paths.cwd)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "not detected".into())
        ));
    }
    body.push_str("\n## Notes\n\n- Project environments stay local at `.venv` and run through `uv`.\n- Keep private account and mount details out of shared copies.\n");
    r::check(flag)?;
    Ok(body)
}
pub(crate) fn run(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<crate::Output> {
    let out = output_path(paths)?;
    if options.has("--check") {
        if !valid(&r::read(&out, r::LIMIT)?) {
            return Err(r::Error(1, "invalid machine snapshot".into()));
        }
        return Ok(r::text(format!("machine snapshot: ok ({})", out.display())));
    }
    let body = render(paths, flag)?;
    if options.preview() {
        return Ok((0, body.into_bytes(), Vec::new()));
    }
    r::child_guard()?;
    let _guard = r::lock(&paths.receipt, &paths.cwd)?;
    let mut tx = Transaction::begin(paths)?;
    tx.write(&out, body.as_bytes(), 0o600)?;
    tx.commit()?;
    Ok(r::text(format!("wrote {}", out.display())))
}

pub(crate) fn slurm(paths: &Paths, tx: &mut Transaction, flag: &AtomicBool) -> Result<()> {
    let output = env::var_os("OH_MY_SETTING_SLURM_REF")
        .map(PathBuf::from)
        .unwrap_or(paths.source.join("local/slurm.md"));
    let mut body = format!(
        "# Local Slurm Cluster\n\nSchema: 1\nGenerated: {}\n\nUse this as local reference only. Do not commit generated cluster details.\n\n",
        r::now()
    );
    let user = env::var("USER").unwrap_or_default();
    let assoc = format!("user={user}");
    let sections = [
        (
            "Partitions",
            "sinfo",
            vec!["-h", "-o", "%P|%a|%l|%D|%G|%m|%c|%N"],
        ),
        (
            "Partition Configuration",
            "scontrol",
            vec!["-o", "show", "partition"],
        ),
        ("Node Configuration", "scontrol", vec!["-o", "show", "node"]),
        (
            "Associations (current user)",
            "sacctmgr",
            vec![
                "-nP",
                "show",
                "assoc",
                assoc.as_str(),
                "format=Cluster,Account,User,Partition,DefaultQOS,QOS,GrpTRES,MaxTRES,MaxTRESPJ,MaxJobs,MaxSubmit,MaxWall",
            ],
        ),
        (
            "QOS",
            "sacctmgr",
            vec![
                "-nP",
                "show",
                "qos",
                "format=Name,Priority,Preempt,PreemptMode,Flags,UsageFactor,GrpTRES,MaxTRESPU,MaxTRESPJ,MaxWall,MaxJobsPU,MaxSubmitPU",
            ],
        ),
        (
            "Current Queue",
            "squeue",
            vec![
                "-u",
                user.as_str(),
                "-o",
                "%.18i %.9P %.32j %.2t %.10M %.6D %R",
            ],
        ),
    ];
    let mut collected = std::collections::BTreeMap::<String, String>::new();
    for (heading, command, args) in sections {
        body.push_str(&format!("## {heading}\n\n"));
        match probe(paths, command, &args, flag) {
            Some(raw) => {
                collected.insert(heading.to_owned(), raw.clone());
                body.push_str("```text\n");
                body.push_str(&raw.replace("```", "'''"));
                body.push_str("\n```\n\n");
            }
            None => body.push_str(
                "Unavailable (command missing, accounting disabled, or access denied).\n\n",
            ),
        }
    }
    let partitions = collected
        .get("Partition Configuration")
        .map(String::as_str)
        .unwrap_or("");
    let default = partitions
        .lines()
        .find(|line| line.split_whitespace().any(|p| p == "Default=YES"))
        .unwrap_or("");
    let value = |name: &str| {
        default
            .split_whitespace()
            .find_map(|w| {
                w.split_once('=')
                    .filter(|(n, _)| *n == name)
                    .map(|(_, v)| v)
            })
            .unwrap_or("(not configured)")
            .to_owned()
    };
    let preferred = value("PartitionName");
    let associations = collected
        .get("Associations (current user)")
        .map(String::as_str)
        .unwrap_or("");
    let association = |column: usize| {
        let mut values = std::collections::BTreeSet::new();
        for line in associations.lines() {
            let fields = line.split('|').collect::<Vec<_>>();
            if fields
                .get(3)
                .is_some_and(|p| *p == preferred || p.is_empty())
                && let Some(v) = fields.get(column).filter(|s| !s.is_empty())
            {
                values.insert((*v).to_owned());
            }
        }
        if values.is_empty() {
            "(not configured)".to_owned()
        } else {
            values.into_iter().collect::<Vec<_>>().join(", ")
        }
    };
    let defaults = value("JobDefaults");
    let cpu = defaults
        .split(',')
        .filter(|p| p.starts_with("DefCpu"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut memory = default
        .split_whitespace()
        .filter(|p| {
            p.starts_with("DefMemPerCPU=")
                || p.starts_with("DefMemPerNode=")
                || p.starts_with("DefMemPerGPU=")
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    memory.extend(
        defaults
            .split(',')
            .filter(|p| p.starts_with("DefMem"))
            .map(str::to_owned),
    );
    let partition_label = concat!("Preferred ", "partition");
    body.push_str(&format!("## Effective Submission Defaults\n\nValues are copied from configuration and associations; missing values are not inferred.\n\n- {partition_label}: {preferred}\n- Account: {}\n- QOS default: {}\n- CPU default: {}\n- GPU default: (not configured)\n- Memory default: {}\n- Time default: {}\n- Log path:\n- Checkpoint path:\n",association(1),association(4),if cpu.is_empty(){"(not configured)"}else{&cpu},if memory.is_empty(){"(not configured)".into()}else{memory.join(", ")},value("DefaultTime")));
    if env::var_os("OH_MY_SETTING_SLURM_WRITE_RAW").is_some_and(|s| s == "1") {
        let root = env::var_os("OH_MY_SETTING_SLURM_RAW_DIR")
            .map(PathBuf::from)
            .unwrap_or(paths.source.join("local/slurm"));
        for (heading, name) in [
            ("Partition Configuration", "partitions.txt"),
            ("Node Configuration", "nodes.txt"),
            (
                "Associations (current user)",
                "associations-current-user.psv",
            ),
            ("QOS", "qos.psv"),
        ] {
            if let Some(raw) = collected.get(heading) {
                tx.write(&root.join(name), format!("{raw}\n").as_bytes(), 0o600)?;
            }
        }
    }
    tx.write(&output, body.as_bytes(), 0o600)
}
