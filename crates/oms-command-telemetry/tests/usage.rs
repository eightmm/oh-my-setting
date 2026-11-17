mod support;
use oms_command_telemetry::store::usage::{
    self, Cost, Cumulative, Identity, Phase, Receipt, Usage,
};
use std::sync::atomic::AtomicBool;
use support::Temp;

fn identity(call: &str) -> Identity {
    Identity {
        call_id: call.into(),
        operation_id: format!("op_{call}"),
        provider: "codex".into(),
        account: Some("account_one".into()),
        goal_id: Some("goal_one".into()),
        ..Identity::default()
    }
}
fn observe(mut receipt: Receipt, revision: u64, tokens: u64, cost: Cost) -> Receipt {
    receipt.revision = revision;
    receipt.phase = Phase::Final;
    receipt.usage = Usage {
        total_tokens: Some(tokens),
        ..Usage::default()
    };
    receipt.cost = cost;
    receipt
}
#[test]
fn receipts_are_absolute_idempotent_and_revision_ordered() {
    let root = Temp::new();
    let stop = AtomicBool::new(false);
    let initial = usage::begin(&root.0, identity("call_one"), None, &stop).unwrap();
    assert_eq!(usage::load(&root.0).unwrap().calls[0].tokens, None);
    let mut partial = observe(initial.clone(), 2, 40, Cost::Unknown);
    partial.phase = Phase::Partial;
    usage::record(&root.0, &partial, &stop).unwrap();
    let final_ = observe(initial.clone(), 4, 100, Cost::Reported(90));
    assert!(usage::record(&root.0, &final_, &stop).unwrap());
    assert!(!usage::record(&root.0, &final_, &stop).unwrap());
    usage::record(
        &root.0,
        &observe(initial.clone(), 3, 80, Cost::Reported(70)),
        &stop,
    )
    .unwrap();
    let current = usage::load(&root.0).unwrap();
    assert_eq!(current.calls[0].tokens, Some(100));
    assert_eq!(current.calls[0].cost_microusd, Some(90));
    let before = std::fs::read(root.0.join(usage::RECEIPTS)).unwrap();
    let conflict = observe(initial.clone(), 4, 101, Cost::Reported(90));
    assert!(
        usage::record(&root.0, &conflict, &stop)
            .unwrap_err()
            .to_string()
            .contains("conflicting usage receipt revision")
    );
    assert_eq!(before, std::fs::read(root.0.join(usage::RECEIPTS)).unwrap());
    usage::record(&root.0, &observe(initial, 5, 75, Cost::Reported(0)), &stop).unwrap();
    let corrected = usage::load(&root.0).unwrap();
    assert_eq!(corrected.calls[0].tokens, Some(75));
    assert_eq!(corrected.calls[0].cost_microusd, Some(0));
    // The existing read-only telemetry verb exposes the canonical lifetime
    // authority even when the artifact index has no retained rows.
    let report = root.call(&["--json"]);
    assert_eq!(report.0, 0);
    let report = oms_context::parse_json(&report.1).unwrap();
    let lifetime = support::field(&report, "lifetime_provider_usage");
    assert_eq!(
        support::field(lifetime, "tokens"),
        &oms_context::Json::Number("75".into())
    );
    assert_eq!(
        support::field(lifetime, "cost_microusd"),
        &oms_context::Json::Number("0".into())
    );
}
#[test]
fn cumulative_resume_has_a_baseline_and_separate_account_session_identity() {
    let root = Temp::new();
    let stop = AtomicBool::new(false);
    let session = Cumulative {
        session_id: "session_one".into(),
        baseline_unknown: false,
    };
    let a = usage::begin(&root.0, identity("call_one"), Some(session.clone()), &stop).unwrap();
    assert!(
        usage::begin(
            &root.0,
            identity("call_overlap"),
            Some(session.clone()),
            &stop
        )
        .is_err()
    );
    usage::record(
        &root.0,
        &observe(a.clone(), 1, 100, Cost::Reported(1000)),
        &stop,
    )
    .unwrap();
    let b = usage::begin(
        &root.0,
        identity("call_two"),
        Some(Cumulative {
            baseline_unknown: true,
            ..session.clone()
        }),
        &stop,
    )
    .unwrap();
    usage::record(
        &root.0,
        &observe(b.clone(), 1, 150, Cost::Reported(1400)),
        &stop,
    )
    .unwrap();
    let mut other_account = identity("call_three");
    other_account.account = Some("account_two".into());
    let c = usage::begin(&root.0, other_account, Some(session), &stop).unwrap();
    usage::record(&root.0, &observe(c, 1, 25, Cost::Reported(250)), &stop).unwrap();
    let d = usage::begin(
        &root.0,
        identity("call_four"),
        Some(Cumulative {
            session_id: "session_two".into(),
            baseline_unknown: true,
        }),
        &stop,
    )
    .unwrap();
    usage::record(&root.0, &observe(d, 1, 200, Cost::Reported(2000)), &stop).unwrap();
    let calls = usage::load(&root.0).unwrap().calls;
    assert_eq!(
        calls.iter().map(|c| c.tokens).collect::<Vec<_>>(),
        vec![Some(100), Some(50), Some(25), None]
    );
    assert_eq!(calls[1].cost_microusd, Some(400));
    // A late correction changes the old allocation and its following delta,
    // while the session's latest measured total still counts exactly once.
    usage::record(&root.0, &observe(a, 2, 90, Cost::Reported(800)), &stop).unwrap();
    let calls = usage::load(&root.0).unwrap().calls;
    assert_eq!(calls[0].tokens, Some(90));
    assert_eq!(calls[1].tokens, Some(60));
    assert_eq!(calls[1].cost_microusd, Some(600));
    usage::record(&root.0, &observe(b, 2, 85, Cost::Reported(700)), &stop).unwrap();
    let calls = usage::load(&root.0).unwrap().calls;
    assert!(calls[1].conflict);
    assert_eq!(calls[1].tokens, None);
}
#[test]
fn cumulative_scope_and_cost_basis_cannot_change_or_mix() {
    let root = Temp::new();
    let stop = AtomicBool::new(false);
    let initial = usage::begin(&root.0, identity("call_one"), None, &stop).unwrap();
    let mut reported = observe(initial.clone(), 4, 100, Cost::Reported(1000));
    reported.cumulative = Some(Cumulative {
        session_id: "session_one".into(),
        baseline_unknown: false,
    });
    // A new provider session can first be identified after its pending registration.
    usage::record(&root.0, &reported, &stop).unwrap();
    let before = std::fs::read(root.0.join(usage::RECEIPTS)).unwrap();
    let mut removed = observe(reported.clone(), 5, 100, Cost::Reported(1000));
    removed.cumulative = None;
    let mut reassigned = removed.clone();
    reassigned.cumulative = Some(Cumulative {
        session_id: "session_two".into(),
        baseline_unknown: false,
    });
    let mut changed_baseline = reported.clone();
    changed_baseline.revision = 5;
    changed_baseline
        .cumulative
        .as_mut()
        .unwrap()
        .baseline_unknown = true;
    let stale_per_call = observe(initial, 2, 90, Cost::Reported(900));
    for invalid in [removed, reassigned, changed_baseline, stale_per_call] {
        assert!(
            usage::record(&root.0, &invalid, &stop)
                .unwrap_err()
                .to_string()
                .contains("conflicting cumulative usage identity")
        );
        assert_eq!(before, std::fs::read(root.0.join(usage::RECEIPTS)).unwrap());
    }
    let estimate = observe(reported, 5, 100, Cost::ProviderEstimate(1000));
    assert!(
        usage::record(&root.0, &estimate, &stop)
            .unwrap_err()
            .to_string()
            .contains("conflicting cumulative cost basis")
    );
    let next = usage::begin(
        &root.0,
        identity("call_two"),
        Some(Cumulative {
            session_id: "session_one".into(),
            baseline_unknown: true,
        }),
        &stop,
    )
    .unwrap();
    let estimate = observe(next.clone(), 1, 150, Cost::ProviderEstimate(1400));
    assert!(
        usage::record(&root.0, &estimate, &stop)
            .unwrap_err()
            .to_string()
            .contains("conflicting cumulative cost basis")
    );
    usage::record(
        &root.0,
        &observe(next.clone(), 1, 150, Cost::Unknown),
        &stop,
    )
    .unwrap();
    // Missing cost may still be reconciled by later evidence with the same basis.
    usage::record(&root.0, &observe(next, 2, 150, Cost::Reported(1400)), &stop).unwrap();
    let calls = usage::load(&root.0).unwrap().calls;
    assert_eq!(calls[1].tokens, Some(50));
    assert_eq!(calls[1].cost_microusd, Some(400));
}
#[test]
fn concurrent_begins_and_repeated_final_receipts_do_not_lose_or_double_count_usage() {
    let root = Temp::new();
    std::thread::scope(|scope| {
        for n in 0..8 {
            let repo = &root.0;
            scope.spawn(move || {
                let stop = AtomicBool::new(false);
                let r = usage::begin(repo, identity(&format!("call_{n}")), None, &stop).unwrap();
                let r = observe(r, 1, 7, Cost::Reported(3));
                usage::record(repo, &r, &stop).unwrap();
                assert!(!usage::record(repo, &r, &stop).unwrap());
            });
        }
    });
    let calls = usage::load(&root.0).unwrap().calls;
    assert_eq!(calls.len(), 8);
    assert_eq!(calls.iter().map(|c| c.tokens.unwrap()).sum::<u64>(), 56);
    assert_eq!(
        calls.iter().map(|c| c.receipt.ordinal).collect::<Vec<_>>(),
        (1..=8).collect::<Vec<_>>()
    );
}
#[test]
fn unknown_cost_and_interrupted_completion_remain_explicit() {
    let root = Temp::new();
    let stop = AtomicBool::new(false);
    for (n, cost) in [Cost::Unknown, Cost::Unpriced, Cost::Subscription]
        .into_iter()
        .enumerate()
    {
        let mut r = usage::begin(&root.0, identity(&format!("call_{n}")), None, &stop).unwrap();
        r = observe(r, 1, 9, cost);
        r.phase = Phase::Unknown;
        usage::record(&root.0, &r, &stop).unwrap();
    }
    let calls = usage::load(&root.0).unwrap().calls;
    assert!(
        calls
            .iter()
            .all(|c| c.cost_microusd.is_none() && c.receipt.phase == Phase::Unknown)
    );
    let initial = usage::begin(&root.0, identity("call_interrupted"), None, &stop).unwrap();
    assert!(
        usage::record(
            &root.0,
            &observe(initial, 1, 0, Cost::Reported(0)),
            &AtomicBool::new(true)
        )
        .is_err()
    );
    assert_eq!(
        usage::load(&root.0).unwrap().calls[3].receipt.phase,
        Phase::Pending
    );
}
#[test]
fn decimal_microdollars_do_not_use_floating_point_or_unknown_as_zero() {
    for (text, expected) in [
        ("0", Some(0)),
        ("1.234567", Some(1234567)),
        ("0.00000001", Some(1)),
        ("1e-7", Some(1)),
        ("1.5E2", Some(150000000)),
        ("-1", None),
        ("NaN", None),
        ("18446744073710", None),
    ] {
        assert_eq!(usage::usd_to_microusd(text), expected, "{text}");
    }
}
#[test]
fn malformed_or_conflicting_receipt_history_refuses_accounting() {
    let root = Temp::new();
    let stop = AtomicBool::new(false);
    let receipt = usage::begin(&root.0, identity("call_one"), None, &stop).unwrap();
    let bytes = std::fs::read(root.0.join(usage::RECEIPTS)).unwrap();
    root.write(usage::RECEIPTS, &bytes[..bytes.len() - 1]);
    assert!(usage::load(&root.0).is_err());
    assert!(
        usage::record(
            &root.0,
            &observe(receipt.clone(), 1, 10, Cost::Reported(1)),
            &stop
        )
        .is_err()
    );
    let text = String::from_utf8(bytes.clone()).unwrap();
    let duplicate = text.replacen("\"call_id\":", "\"call_id\": \"conflict\", \"call_id\":", 1);
    assert_ne!(text, duplicate);
    root.write(usage::RECEIPTS, duplicate);
    assert!(usage::load(&root.0).is_err());
    root.write(usage::RECEIPTS, bytes);
    let mut impossible = observe(receipt, 1, 0, Cost::Unknown);
    impossible.usage.input_tokens = Some(10);
    impossible.usage.output_tokens = Some(1);
    assert!(usage::record(&root.0, &impossible, &stop).is_err());
}
