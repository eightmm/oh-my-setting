use oms_command_autonomy::{
    CommandResult, Invocation, Services, campaign::Ledger, run_with_services, value,
};
use oms_command_plan::storage::Store;
use oms_command_telemetry::store::usage::{self, Cost, Identity, Phase, Receipt, Usage};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

struct Fixture {
    repo: PathBuf,
    directory: String,
}
impl Fixture {
    fn new(usage: Option<Value>) -> Self {
        let repo = std::env::temp_dir().join(value::random("oms-campaign-usage-").unwrap());
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&repo)
            .unwrap();
        let mut manifest = json!({"problem":{"file":"problem.txt","sha256":"a".repeat(64)},"base":"b".repeat(40),
            "verify":{"command":"true","timeout_s":30,"inputs":[]},"candidate_formats":["text"],
            "groups":[{"id":"group","formulation":"fixture","k":1,"routes":[{"provider":"claude","model":"fixture-model"}]}],
            "budget":{"max_concurrent":4,"total_calls":10,"deadline_s":60,"max_waves":2,"stall_waves":2,
                "per_provider":{"codex":{"calls":5,"concurrency":1}},
                "verifier":{"concurrency":1,"timeout_s":30,"max_artifact_bytes":1024}}});
        if let Some(usage) = usage {
            manifest["budget"]["usage"] = usage;
        }
        let raw = value::canonical(&manifest).unwrap();
        let directory = format!(".oms/plan/campaigns/{}", &value::hash(&raw)[..16]);
        let store = Store::open(&repo).unwrap();
        store
            .publish_once(format!("{directory}/manifest.json"), &raw, 1024 * 1024)
            .unwrap();
        store
            .append(
                format!("{directory}/events.jsonl"),
                &value::plan(&json!({"type":"campaign_started","budget":manifest["budget"],"started_at":value::now()}))
                    .unwrap(),
            )
            .unwrap();
        Self { repo, directory }
    }
    fn ledger<'a>(&self, stop: &'a AtomicBool) -> Ledger<'a> {
        Ledger::open(&self.repo, &self.directory, stop).unwrap()
    }
    fn receipt(&self, operation: &str, phase: Phase, tokens: u64, cost: Cost) -> Receipt {
        let stop = AtomicBool::new(false);
        let mut receipt = usage::begin(
            &self.repo,
            Identity {
                call_id: value::random("call_").unwrap(),
                operation_id: operation.into(),
                provider: "claude".into(),
                attempt_id: Some(value::random("att_").unwrap()),
                ..Identity::default()
            },
            None,
            &stop,
        )
        .unwrap();
        receipt.revision = 1;
        receipt.phase = phase;
        receipt.usage = Usage {
            total_tokens: Some(tokens),
            ..Usage::default()
        };
        receipt.cost = cost;
        usage::record(&self.repo, &receipt, &stop).unwrap();
        receipt
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.repo);
    }
}
fn limits() -> Value {
    json!({"max_tokens":100,"max_cost_microusd":100,"per_call":{"tokens":60,"cost_microusd":60}})
}
#[test]
fn saved_campaign_reader_refuses_torn_and_conflicting_budget_rows() {
    let fixture = Fixture::new(Some(limits()));
    let stop = AtomicBool::new(false);
    let ledger = fixture.ledger(&stop);
    ledger.reserve("claude", "seat", None).unwrap();
    let path = fixture.repo.join(&fixture.directory).join("events.jsonl");
    let before = std::fs::read(&path).unwrap();
    let rows = ledger.events().unwrap();
    let encode = |rows: &[Value]| {
        rows.iter()
            .flat_map(|row| {
                let mut raw = value::canonical(row).unwrap();
                raw.push(b'\n');
                raw
            })
            .collect::<Vec<_>>()
    };
    let mut wrong_budget = rows.clone();
    wrong_budget[0]["budget"]["verifier"]["concurrency"] = true.into();
    let mut duplicate = rows.clone();
    duplicate.push(rows[1].clone());
    let mut foreign = rows.clone();
    foreign
        .push(json!({"type":"settled","id":format!("res_{}","0".repeat(32)),"outcome":"refused"}));
    for malformed in [
        before[..before.len() - 1].to_vec(),
        encode(&wrong_budget),
        encode(&duplicate),
        encode(&foreign),
    ] {
        std::fs::write(&path, &malformed).unwrap();
        assert_eq!(ledger.spent().unwrap_err().code, 2);
        assert_eq!(ledger.reserve("claude", "seat", None).unwrap_err().code, 2);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            malformed,
            "refusal preserves untrusted history"
        );
    }
    std::fs::write(path, before).unwrap();
    assert_eq!(ledger.spent().unwrap()["total_calls"], 1);
}
#[test]
fn settled_previous_attempts_stay_in_admission_and_late_corrections_reconcile() {
    let fixture = Fixture::new(Some(limits()));
    let stop = AtomicBool::new(false);
    let ledger = fixture.ledger(&stop);
    let first = ledger.reserve("claude", "seat", None).unwrap();
    assert_eq!(ledger.reserve("claude", "seat", None).unwrap_err().code, 3);
    let mut receipt = fixture.receipt(&first, Phase::Final, 50, Cost::ProviderEstimate(50));
    assert!(ledger.settle(&first, "ok").unwrap());
    assert_eq!(ledger.reserve("claude", "seat", None).unwrap_err().code, 3);
    let spent = ledger.spent().unwrap();
    assert_eq!(spent["usage"]["tokens"]["settled"], 50);
    assert_eq!(spent["usage"]["tokens"]["active_reserved"], 0);
    receipt.revision = 2;
    receipt.usage.total_tokens = Some(30);
    receipt.cost = Cost::ProviderEstimate(30);
    usage::record(&fixture.repo, &receipt, &stop).unwrap();
    let second = ledger.reserve("claude", "seat", None).unwrap();
    assert_ne!(first, second);
    assert_eq!(ledger.spent().unwrap()["usage"]["tokens"]["committed"], 90);
    fixture.receipt(&second, Phase::Final, 80, Cost::ProviderEstimate(80));
    ledger.settle(&second, "ok").unwrap();
    assert_eq!(ledger.spent().unwrap()["usage"]["tokens"]["settled"], 110);
    assert_eq!(ledger.reserve("claude", "seat", None).unwrap_err().code, 3);
}
#[test]
fn timeout_abandonment_and_unpriced_calls_keep_liabilities_across_restart() {
    let fixture = Fixture::new(Some(limits()));
    let stop = AtomicBool::new(false);
    let ledger = fixture.ledger(&stop);
    let first = ledger.reserve("claude", "seat", None).unwrap();
    let mut receipt = fixture.receipt(&first, Phase::Unknown, 20, Cost::Unknown);
    ledger.abandon_unsettled().unwrap();
    drop(ledger);
    let ledger = fixture.ledger(&stop);
    let spent = ledger.spent().unwrap();
    assert_eq!(spent["usage"]["tokens"]["unresolved_reserved"], 60);
    assert_eq!(spent["usage"]["cost_microusd"]["unknown_operations"], 1);
    assert_eq!(ledger.reserve("claude", "seat", None).unwrap_err().code, 3);
    receipt.revision = 2;
    receipt.phase = Phase::Final;
    receipt.cost = Cost::Subscription;
    usage::record(&fixture.repo, &receipt, &stop).unwrap();
    assert_eq!(ledger.spent().unwrap()["usage"]["tokens"]["settled"], 20);
    assert_eq!(ledger.reserve("claude", "seat", None).unwrap_err().code, 3);
    receipt.revision = 3;
    receipt.cost = Cost::Reported(0);
    usage::record(&fixture.repo, &receipt, &stop).unwrap();
    assert!(ledger.reserve("claude", "seat", None).is_ok());
}
#[test]
fn concurrent_campaign_reservations_share_one_admission_lock() {
    let fixture = Fixture::new(Some(limits()));
    let stop = AtomicBool::new(false);
    let outcomes = std::thread::scope(|scope| {
        (0..6)
            .map(|_| scope.spawn(|| fixture.ledger(&stop).reserve("claude", "seat", None)))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(outcomes.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(
        outcomes
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(|e| e.code == 3)
    );
    assert_eq!(fixture.ledger(&stop).spent().unwrap()["total_calls"], 1);
}
#[test]
fn each_unknown_physical_retry_retains_its_own_reservation() {
    let fixture = Fixture::new(Some(limits()));
    let stop = AtomicBool::new(false);
    let ledger = fixture.ledger(&stop);
    let refused = ledger.reserve("claude", "seat", None).unwrap();
    let environment = ledger.provider_environment(&refused).unwrap();
    let manifest =
        std::fs::read(fixture.repo.join(&fixture.directory).join("manifest.json")).unwrap();
    assert_eq!(environment.len(), 3);
    assert!(environment.contains(&(
        "OMS_CAMPAIGN_MANIFEST_SHA256".into(),
        value::hash(manifest).into()
    )));
    assert!(environment.contains(&("OMS_OPERATION_ID".into(), refused.clone().into())));
    ledger.settle(&refused, "refused").unwrap();
    assert_eq!(ledger.provider_environment(&refused).unwrap_err().code, 2);
    assert_eq!(
        ledger.spent().unwrap()["usage"]["cost_microusd"]["committed"],
        0
    );
    let operation = ledger.reserve("claude", "seat", None).unwrap();
    fixture.receipt(&operation, Phase::Unknown, 1, Cost::Unknown);
    fixture.receipt(&operation, Phase::Unknown, 1, Cost::Unknown);
    ledger.settle(&operation, "failed").unwrap();
    let spent = ledger.spent().unwrap();
    assert_eq!(spent["usage"]["physical_calls"], 2);
    assert_eq!(spent["usage"]["cost_microusd"]["unresolved_reserved"], 120);
    assert!(spent["usage"]["cost_microusd"]["measured"].is_null());
    assert_eq!(ledger.reserve("claude", "seat", None).unwrap_err().code, 3);
}
#[test]
fn physical_intents_without_receipts_stay_in_logical_admission_and_refusal() {
    use oms_command_telemetry::store::{canonical_index, physical};
    use oms_lock::{Lock, Mode, Settings};
    use std::path::Path;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};
    let fixture = Fixture::new(Some(json!({"max_tokens":150,"per_call":{"tokens":60}})));
    let stop = AtomicBool::new(false);
    let ledger = fixture.ledger(&stop);
    let operation = ledger.reserve("claude", "seat", None).unwrap();
    let target = canonical_index(
        &fixture.repo,
        Some(Path::new(usage::RECEIPTS)),
        false,
        false,
    )
    .unwrap();
    let mut settings = Settings::from_env().unwrap();
    settings.mode = Mode::LegacyAuto;
    let held = Lock::for_file(&target, settings)
        .unwrap()
        .try_acquire()
        .unwrap();
    for call in ["call_first", "call_retry"] {
        let cancel = AtomicBool::new(false);
        // The deadline only bounds a failed publication; cancellation follows
        // an actual atomic intent observation while receipt admission is held.
        let deadline = Instant::now() + Duration::from_secs(30);
        std::thread::scope(|scope| {
            let writer = scope.spawn(|| {
                physical::begin(
                    &fixture.repo,
                    Identity {
                        call_id: call.into(),
                        operation_id: operation.clone(),
                        provider: "claude".into(),
                        ..Identity::default()
                    },
                    None,
                    false,
                    &cancel,
                    deadline,
                )
            });
            let intent = loop {
                // A concurrent atomic replacement can invalidate a read; an
                // unsuccessful observation supplies no cancellation evidence.
                if let Some(intent) = physical::load(&fixture.repo, &stop)
                    .ok()
                    .into_iter()
                    .flatten()
                    .find(|intent| intent.identity.call_id == call)
                {
                    break intent;
                }
                assert!(
                    !writer.is_finished(),
                    "registration ended before publication"
                );
                assert!(
                    Instant::now() < deadline,
                    "physical intent was not published"
                );
                std::thread::yield_now();
            };
            assert_eq!(intent.phase, physical::Phase::Reserved);
            assert_eq!(intent.usage_ordinal, None);
            held.check().unwrap();
            cancel.store(true, Ordering::Release);
            let error = writer.join().unwrap().unwrap_err();
            assert!(error.publication_uncertain);
        });
    }
    drop(held);
    assert!(usage::load(&fixture.repo).unwrap().calls.is_empty());
    assert_eq!(
        ledger.reserve("claude", "seat", None).unwrap_err().code,
        3,
        "two unacknowledged physical intents cost 120; another logical escrow would cost 180"
    );
    let spent = ledger.spent().unwrap();
    assert_eq!(spent["usage"]["tokens"]["committed"], 120);
    assert_eq!(spent["usage"]["intent_only_calls"], 2);
    ledger.settle(&operation, "refused").unwrap();
    assert_eq!(
        ledger.spent().unwrap()["usage"]["tokens"]["unresolved_reserved"],
        120,
        "a terminal refusal cannot erase existing physical intents"
    );
}
#[derive(Default)]
struct CampaignHost(Mutex<Vec<(String, Duration, Option<String>)>>);
impl oms_command_plan::Services for CampaignHost {}
impl Services for CampaignHost {
    fn invoke(
        &self,
        call: &Invocation,
        _: &AtomicBool,
    ) -> oms_command_autonomy::Result<CommandResult> {
        assert_eq!(call.tool, "agent-call");
        let provider = call.args.windows(2).find(|pair| pair[0] == "--to").unwrap()[1]
            .to_string_lossy()
            .into_owned();
        let peer_timeout = call
            .environment
            .iter()
            .find_map(|(k, v)| (k == "OMS_PEER_TIMEOUT").then(|| v.to_string_lossy().into_owned()));
        self.0
            .lock()
            .unwrap()
            .push((provider.clone(), call.wall, peer_timeout));
        let answer = if provider == "codex" {
            b"```json\n[]\n```\n".as_slice()
        } else {
            b"```text\ncandidate note\n```\n".as_slice()
        };
        let dir = call.cwd.join(".oms/artifacts/campaign-test");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .unwrap();
        let name = format!("answer-{}.md", self.0.lock().unwrap().len());
        let path = dir.join(&name);
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .unwrap()
            .write_all(answer)
            .unwrap();
        Ok((
            0,
            format!("artifact: {}\n", path.display()).into_bytes(),
            Vec::new(),
        ))
    }
}

fn run_campaign(deadline_s: u64, extra: &[&str]) -> Vec<(String, Duration, Option<String>)> {
    let root = std::env::temp_dir().join(value::random("oms-campaign-wall-").unwrap());
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    let repo = root.canonicalize().unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "--quiet"]);
    git(&["config", "--local", "user.name", "Fixture"]);
    git(&["config", "--local", "user.email", "fixture@example.invalid"]);
    let problem = b"solve fixture\n";
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(repo.join("problem.txt"))
        .unwrap()
        .write_all(problem)
        .unwrap();
    git(&["add", "problem.txt"]);
    git(&["commit", "--quiet", "-m", "init"]);
    let base = git(&["rev-parse", "HEAD"]);
    let manifest = json!({
        "problem": {"file": "problem.txt", "sha256": value::hash(problem)},
        "base": base,
        "verify": {"command": "true", "timeout_s": 30, "inputs": []},
        "candidate_formats": ["text"],
        "consolidator": {"provider": "codex", "model": "fixture-consolidator"},
        "groups": [{
            "id": "group",
            "formulation": "fixture",
            "k": 1,
            "routes": [{"provider": "claude", "model": "fixture-seat"}],
        }],
        "budget": {
            "max_concurrent": 2,
            "total_calls": 4,
            "deadline_s": deadline_s,
            "max_waves": 2,
            "stall_waves": 2,
            "verifier": {"concurrency": 1, "timeout_s": 30, "max_artifact_bytes": 1024},
        },
    });
    let manifest_path = repo.join("campaign.json");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&manifest_path)
        .unwrap()
        .write_all(&value::pretty(&manifest).unwrap())
        .unwrap();
    let mut argv = vec![OsString::from("--campaign"), manifest_path.into_os_string()];
    argv.extend(extra.iter().map(OsString::from));
    let host = CampaignHost::default();
    let (code, out, err) =
        run_with_services("goal-drive", &argv, &repo, &AtomicBool::new(false), &host);
    let _ = std::fs::remove_dir_all(&repo);
    assert_eq!(
        code,
        3,
        "{}{}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    host.0.into_inner().unwrap()
}

#[test]
fn omitted_caps_preserve_lifetime_call_caps_and_enabled_caps_require_reservations() {
    const NAME: &str =
        "omitted_caps_preserve_lifetime_call_caps_and_enabled_caps_require_reservations";
    let ambient = |key: &std::ffi::OsStr| {
        let key = key.to_string_lossy();
        key.starts_with("OMS_") || key.starts_with("OH_MY_SETTING_")
    };
    if std::env::var("CAMPAIGN_PINNED_TIMEOUT_SUBTEST").as_deref() == Ok("90s") {
        assert_eq!(std::env::var("OMS_PEER_TIMEOUT").as_deref(), Ok("90s"));
        assert_eq!(
            run_campaign(3600, &[]),
            vec![
                ("claude".into(), Duration::from_secs(90), Some("90s".into()),),
                ("codex".into(), Duration::from_secs(90), Some("90s".into()),),
                ("claude".into(), Duration::from_secs(90), Some("90s".into()),),
            ]
        );
        return;
    }
    if std::env::vars_os().any(|(k, _)| ambient(&k)) {
        let mut child = Command::new(std::env::current_exe().unwrap());
        for (key, _) in std::env::vars_os().filter(|(k, _)| ambient(k)) {
            child.env_remove(key);
        }
        let out = child
            .args(["--exact", NAME, "--test-threads=1", "--nocapture"])
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{text}");
        assert!(text.contains("1 passed;"), "{text}");
        return;
    }
    let fixture = Fixture::new(None);
    let stop = AtomicBool::new(false);
    let ledger = fixture.ledger(&stop);
    for _ in 0..10 {
        let id = ledger.reserve("claude", "seat", None).unwrap();
        ledger.settle(&id, "failed").unwrap();
    }
    assert_eq!(
        ledger.reserve("claude", "seat", None).unwrap_err().message,
        "total_calls exhausted"
    );
    assert!(ledger.spent().unwrap().get("usage").is_none());
    for bad in [
        json!({"max_tokens":100,"per_call":{}}),
        json!({"max_cost_microusd":100,"per_call":{"cost_microusd":101}}),
        json!({"per_call":{"tokens":10}}),
    ] {
        let fixture = Fixture::new(Some(bad));
        assert!(Ledger::open(&fixture.repo, &fixture.directory, &stop).is_err());
    }

    let unpinned = run_campaign(3600, &[]);
    assert_eq!(
        unpinned
            .iter()
            .map(|(p, _, _)| p.as_str())
            .collect::<Vec<_>>(),
        vec!["claude", "codex", "claude"]
    );
    for (provider, wall, peer_timeout) in &unpinned {
        assert_eq!(*peer_timeout, None, "{provider}");
        assert!(
            (3595..=3600).contains(&wall.as_secs()),
            "{provider} wall={wall:?}"
        );
    }

    assert_eq!(
        run_campaign(3600, &["--provider-timeout", "25m"]),
        vec![
            (
                "claude".into(),
                Duration::from_secs(1500),
                Some("1500s".into()),
            ),
            (
                "codex".into(),
                Duration::from_secs(1500),
                Some("1500s".into()),
            ),
            (
                "claude".into(),
                Duration::from_secs(1500),
                Some("1500s".into()),
            ),
        ]
    );

    let capped = run_campaign(1800, &["--provider-timeout", "2h"]);
    assert_eq!(
        capped
            .iter()
            .map(|(p, _, _)| p.as_str())
            .collect::<Vec<_>>(),
        vec!["claude", "codex", "claude"]
    );
    for (provider, wall, peer_timeout) in &capped {
        assert!(
            (1795..=1800).contains(&wall.as_secs()),
            "{provider} wall={wall:?}"
        );
        assert_eq!(
            peer_timeout.as_deref(),
            Some(format!("{}s", wall.as_secs()).as_str()),
            "{provider}"
        );
    }

    let mut child = Command::new(std::env::current_exe().unwrap());
    for (key, _) in std::env::vars_os().filter(|(k, _)| ambient(k)) {
        child.env_remove(key);
    }
    let out = child
        .env("CAMPAIGN_PINNED_TIMEOUT_SUBTEST", "90s")
        .env("OMS_PEER_TIMEOUT", "90s")
        .args(["--exact", NAME, "--test-threads=1", "--nocapture"])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(text.contains("1 passed;"), "{text}");
}
