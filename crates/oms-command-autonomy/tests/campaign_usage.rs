use oms_command_autonomy::{campaign::Ledger, value};
use oms_command_plan::storage::Store;
use oms_command_telemetry::store::usage::{self, Cost, Identity, Phase, Receipt, Usage};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

struct Fixture {
    repo: PathBuf,
    directory: String,
}
impl Fixture {
    fn new(usage: Option<Value>) -> Self {
        let repo = std::env::temp_dir().join(value::random("oms-campaign-usage-").unwrap());
        std::fs::create_dir(&repo).unwrap();
        let mut manifest = json!({"problem":{"file":"problem.txt","sha256":"a".repeat(64)},"base":"b".repeat(40),
            "verify":{"command":"true","timeout_s":30,"inputs":[]},"candidate_formats":["text"],
            "groups":[{"id":"group","formulation":"fixture","k":1,"routes":[{"provider":"claude","model":"fixture-model"}]}],
            "budget":{"max_concurrent":4,"total_calls":10,"deadline_s":60,"max_waves":2,"stall_waves":2,
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
                &value::plan(&json!({"type":"campaign_started","budget":manifest["budget"]}))
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
    ledger.settle(&refused, "refused").unwrap();
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
