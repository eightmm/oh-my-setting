use oms_command_autonomy::{campaign::Ledger, value};
use oms_command_plan::storage::Store;
use oms_command_telemetry::store::usage::{self, Cost, Identity, Phase, Receipt, Usage};
use serde_json::{Value, json};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

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
        let error = physical::begin(
            &fixture.repo,
            Identity {
                call_id: call.into(),
                operation_id: operation.clone(),
                provider: "claude".into(),
                ..Identity::default()
            },
            None,
            false,
            &stop,
            Instant::now() + Duration::from_millis(70),
        )
        .unwrap_err();
        assert!(error.publication_uncertain);
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
#[test]
fn omitted_caps_preserve_lifetime_call_caps_and_enabled_caps_require_reservations() {
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
}
