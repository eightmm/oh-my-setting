mod support;
use oms_command_telemetry::store::budget::{
    self, Limit, Operation, OperationState, PhysicalClaim, Policy,
};
use oms_command_telemetry::store::usage::{
    self, Cost, Cumulative, Identity, Phase, Receipt, Usage,
};
use std::os::unix::fs::MetadataExt;
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
fn physical_candidates_replace_escrow_and_join_durable_receipts() {
    let root = Temp::new();
    let stop = AtomicBool::new(false);
    let policy = Policy {
        tokens: Some(Limit {
            max: 100,
            per_call: 60,
        }),
        cost_microusd: Some(Limit {
            max: 100,
            per_call: 60,
        }),
    };
    let operations = [Operation {
        id: "op_one",
        state: OperationState::Active,
    }];
    let first = PhysicalClaim {
        call_id: "call_first",
        operation_id: "op_one",
    };
    let retry = PhysicalClaim {
        call_id: "call_retry",
        operation_id: "op_one",
    };
    let empty = usage::Projection::default();
    let logical = budget::project(&policy, &empty, &operations, &[]).unwrap();
    assert_eq!(logical.tokens.committed, 60);
    assert!(policy.admit_next_logical(logical.commitment()).is_err());
    let admitted = budget::admit_candidate(&policy, &empty, &operations, &[], first).unwrap();
    assert_eq!(
        admitted.tokens.committed, 60,
        "first physical claim replaces its escrow"
    );
    assert_eq!(admitted.intent_only_calls, 1);
    assert_eq!(
        budget::admit_candidate(&policy, &empty, &operations, &[first], retry).unwrap_err(),
        budget::Error::Exhausted("tokens")
    );
    let replay =
        budget::admit_candidate(&policy, &empty, &operations, &[first, first], first).unwrap();
    assert_eq!(replay, admitted);
    let receipt = usage::begin(
        &root.0,
        Identity {
            call_id: first.call_id.into(),
            operation_id: first.operation_id.into(),
            ..identity("unused")
        },
        None,
        &stop,
    )
    .unwrap();
    let projection = usage::load(&root.0).unwrap();
    let pending = budget::project(&policy, &projection, &operations, &[first]).unwrap();
    assert_eq!(
        pending.physical_calls, 1,
        "intent and pending receipt join once"
    );
    assert_eq!(pending.intent_only_calls, 0);
    assert_eq!(pending.tokens.committed, 60);
    let two_owners = [
        operations[0],
        Operation {
            id: "op_other",
            state: OperationState::Active,
        },
    ];
    assert_eq!(
        budget::project(
            &policy,
            &projection,
            &two_owners,
            &[PhysicalClaim {
                operation_id: "op_other",
                ..first
            }]
        )
        .unwrap_err(),
        budget::Error::Conflict,
        "a receipt and intent cannot assign one call to different operations"
    );
    let mut final_ = observe(receipt, 1, 20, Cost::Subscription);
    usage::record(&root.0, &final_, &stop).unwrap();
    let projection = usage::load(&root.0).unwrap();
    assert_eq!(
        budget::admit_candidate(&policy, &projection, &operations, &[first], retry).unwrap_err(),
        budget::Error::Exhausted("cost_microusd"),
        "final usage without measured cost retains cost liability"
    );
    final_.revision = 2;
    final_.cost = Cost::Reported(20);
    usage::record(&root.0, &final_, &stop).unwrap();
    let projection = usage::load(&root.0).unwrap();
    let after =
        budget::admit_candidate(&policy, &projection, &operations, &[first], retry).unwrap();
    assert_eq!(after.tokens.committed, 80);
    assert_eq!(after.cost_microusd.committed, 80);
    assert_eq!(after.physical_calls, 2);
    final_.revision = 3;
    final_.usage.total_tokens = Some(10);
    final_.cost = Cost::Reported(10);
    usage::record(&root.0, &final_, &stop).unwrap();
    let projection = usage::load(&root.0).unwrap();
    let corrected =
        budget::admit_candidate(&policy, &projection, &operations, &[first], retry).unwrap();
    assert_eq!(corrected.tokens.committed, 70);
    let settled = [Operation {
        id: "op_one",
        state: OperationState::Settled,
    }];
    let spent = budget::project(&policy, &projection, &settled, &[first]).unwrap();
    assert_eq!(spent.tokens.settled, 10);
    assert_eq!(spent.tokens.committed, 10);
    assert!(budget::admit_candidate(&policy, &projection, &settled, &[first], retry).is_err());
    let unknown = budget::project(&policy, &empty, &settled, &[first]).unwrap();
    assert_eq!(
        unknown.tokens.unresolved_reserved, 60,
        "orphan intent survives missing receipt and terminal operation"
    );
    let refused = [Operation {
        id: "op_one",
        state: OperationState::RefusedBeforeInvocation,
    }];
    assert_eq!(
        budget::project(&policy, &empty, &refused, &[])
            .unwrap()
            .tokens
            .committed,
        0
    );
    assert_eq!(
        budget::project(&policy, &empty, &refused, &[first])
            .unwrap()
            .tokens
            .committed,
        60,
        "an existing physical intent invalidates no-call refusal proof"
    );
}

#[test]
fn physical_claim_conflicts_and_overflow_refuse_projection() {
    let policy = Policy {
        tokens: Some(Limit {
            max: u64::MAX,
            per_call: u64::MAX,
        }),
        cost_microusd: None,
    };
    let operations = [
        Operation {
            id: "one",
            state: OperationState::Active,
        },
        Operation {
            id: "two",
            state: OperationState::Active,
        },
    ];
    let one = PhysicalClaim {
        call_id: "call_one",
        operation_id: "one",
    };
    let two = PhysicalClaim {
        call_id: "call_two",
        operation_id: "one",
    };
    let changed = PhysicalClaim {
        operation_id: "two",
        ..one
    };
    let missing = PhysicalClaim {
        operation_id: "missing",
        ..one
    };
    let empty = usage::Projection::default();
    assert_eq!(
        budget::project(&policy, &empty, &operations, &[one, changed]).unwrap_err(),
        budget::Error::Conflict
    );
    assert_eq!(
        budget::project(&policy, &empty, &operations[..1], &[one, two]).unwrap_err(),
        budget::Error::Overflow
    );
    assert!(budget::project(&policy, &empty, &operations, &[missing]).is_err());
    assert!(budget::project(&policy, &empty, &[operations[0], operations[0]], &[]).is_err());
    assert!(
        Policy {
            tokens: None,
            cost_microusd: None
        }
        .validate()
        .is_err()
    );
    assert!(
        Policy {
            tokens: Some(Limit {
                max: 0,
                per_call: 0
            }),
            cost_microusd: None
        }
        .validate()
        .is_err()
    );
}

#[test]
fn dispatch_reservations_share_exact_flock_and_preserve_partial_publications() {
    use oms_command_telemetry::store::dispatch::{self, Transaction};
    use oms_lock::{Lock, Mode, Settings};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    let root = Temp::new();
    for path in [
        root.0.clone(),
        root.0.join(".oms"),
        root.0.join(".oms/artifacts"),
    ] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let stop = AtomicBool::new(false);
    let timeout = Duration::from_millis(60);
    let mut settings = Settings::from_env().unwrap();
    settings.mode = Mode::Flock;
    let native =
        Lock::in_root(root.0.clone(), Path::new(dispatch::LOCK), settings.clone()).unwrap();
    let held = native.try_acquire().unwrap();
    settings.mode = Mode::Mkdir;
    let error = Transaction::with_settings(&root.0, settings.clone(), timeout, &stop)
        .err()
        .unwrap();
    assert!(matches!(
        error.source,
        oms_command_telemetry::store::StoreError::Lock(oms_lock::Error::Busy)
    ));
    assert!(!error.publication_uncertain);
    drop(held);
    let tx = Transaction::with_settings(&root.0, settings, timeout, &stop).unwrap();
    assert!(matches!(native.try_acquire(), Err(oms_lock::Error::Busy)));
    assert!(tx.snapshot().unwrap().is_none());
    let seed =
        br#"{"schema":1,"calls":{"legacy":{"state":"unknown"}},"extension":{"retained":true}}"#;
    tx.replace(None, seed).unwrap();
    assert_eq!(tx.snapshot().unwrap().unwrap(), seed);
    let before = std::fs::metadata(root.0.join(dispatch::REGISTRY)).unwrap();
    tx.replace(Some(seed), seed).unwrap();
    assert_eq!(
        std::fs::metadata(root.0.join(dispatch::REGISTRY))
            .unwrap()
            .ino(),
        before.ino()
    );
    let launch = br#"{"schema":2,"member":"call_orphan"}"#;
    let path = tx.artifact("call_orphan", "launch", launch).unwrap();
    let foreign = br#"{"schema":1,"calls":{},"foreign":"preserve"}"#;
    root.write(dispatch::REGISTRY, foreign);
    let error = tx.replace(Some(seed), seed).unwrap_err();
    assert_eq!(
        error.source,
        oms_command_telemetry::store::StoreError::Changed
    );
    assert!(
        error.publication_uncertain,
        "the launch artifact is already visible"
    );
    assert_eq!(
        std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
        foreign
    );
    assert_eq!(std::fs::read(path).unwrap(), launch);
    stop.store(true, Ordering::Release);
    let error = tx.snapshot().unwrap_err();
    assert_eq!(error.exit_code(), 130);
    assert!(error.publication_uncertain);
    drop(tx);
    let error = Transaction::acquire(&root.0, timeout, &stop).err().unwrap();
    assert_eq!(error.exit_code(), 130);
    assert!(!error.publication_uncertain);
    stop.store(false, Ordering::Release);
    let tx = Transaction::acquire(&root.0, timeout, &stop).unwrap();
    let error = tx.replace(Some(seed), seed).unwrap_err();
    assert!(
        !error.publication_uncertain,
        "a fresh failed CAS has no publication effect"
    );
    let error = tx
        .replace(Some(foreign), &vec![b' '; dispatch::MAX_REGISTRY + 1])
        .unwrap_err();
    assert!(!error.publication_uncertain);
    root.write(dispatch::REGISTRY, b"");
    assert!(tx.snapshot().is_err());
    assert!(tx.replace(None, seed).is_err());
    assert!(tx.replace(Some(b""), seed).is_err());
    assert!(
        std::fs::read(root.0.join(dispatch::REGISTRY))
            .unwrap()
            .is_empty()
    );
    for malformed in [
        br#"{"schema":2,"calls":{}}"#.as_slice(),
        br#"{"schema":1,"calls":null}"#,
        br#"{"schema":1,"schema":1,"calls":{}}"#,
        br#"{"schema":1,"calls":{"nested":{"x":1,"x":2}}}"#,
        br#"{"schema":1,"calls":{"nested":{"x":NaN}}}"#,
    ] {
        root.write(dispatch::REGISTRY, malformed);
        assert!(tx.snapshot().is_err());
        assert!(tx.replace(Some(malformed), seed).is_err());
        assert_eq!(
            std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
            malformed
        );
    }
    root.write(".oms/artifacts/panel-dispatch/call_empty-launch.json", b"");
    assert!(
        tx.artifact(
            "call_empty",
            "launch",
            br#"{"schema":2,"member":"call_empty"}"#
        )
        .is_err()
    );
    assert!(
        std::fs::read(
            root.0
                .join(".oms/artifacts/panel-dispatch/call_empty-launch.json")
        )
        .unwrap()
        .is_empty()
    );
    std::fs::rename(
        root.0.join(".oms/artifacts"),
        root.0.join(".oms/artifacts-held"),
    )
    .unwrap();
    root.write(dispatch::REGISTRY, foreign);
    let error = tx.replace(Some(foreign), seed).unwrap_err();
    assert!(!error.publication_uncertain);
    assert_eq!(
        std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
        foreign
    );
}

#[test]
fn pending_admission_bounds_receipt_and_ignore_lock_waits() {
    use oms_command_telemetry::store::{StoreError, canonical_index};
    use oms_lock::{Lock, Mode, Settings};
    use std::path::Path;
    use std::time::{Duration, Instant};
    let root = Temp::new();
    let stop = AtomicBool::new(false);
    usage::begin(&root.0, identity("call_initial"), None, &stop).unwrap();
    for (index, relative) in [usage::RECEIPTS, ".oms/.gitignore"].into_iter().enumerate() {
        let target = canonical_index(&root.0, Some(Path::new(relative)), false, false).unwrap();
        let mut settings = Settings::from_env().unwrap();
        settings.mode = Mode::LegacyAuto;
        let lock = Lock::for_file(&target, settings).unwrap();
        let held = lock.try_acquire().unwrap();
        let old = std::fs::read(root.0.join(usage::RECEIPTS)).unwrap();
        let started = Instant::now();
        let identity = identity(&format!("call_blocked_{index}"));
        let error = usage::begin_before(
            &root.0,
            identity.clone(),
            None,
            &stop,
            started + Duration::from_millis(70),
        )
        .unwrap_err();
        assert!(matches!(error, StoreError::Lock(oms_lock::Error::Busy)));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a nested lock must not reset to the legacy 300s default"
        );
        assert_eq!(std::fs::read(root.0.join(usage::RECEIPTS)).unwrap(), old);
        drop(held);
        let receipt = usage::begin_before(
            &root.0,
            identity,
            None,
            &stop,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(
            receipt.ordinal,
            index as u64 + 2,
            "refused registration did not consume an ordinal"
        );
    }
    let old = std::fs::read(root.0.join(usage::RECEIPTS)).unwrap();
    assert!(
        usage::begin_before(
            &root.0,
            identity("call_expired"),
            None,
            &stop,
            Instant::now()
        )
        .is_err()
    );
    assert_eq!(
        usage::begin_before(
            &root.0,
            identity("call_cancelled"),
            None,
            &AtomicBool::new(true),
            Instant::now() + Duration::from_secs(2)
        )
        .unwrap_err()
        .exit_code(),
        130
    );
    assert_eq!(std::fs::read(root.0.join(usage::RECEIPTS)).unwrap(), old);
}
#[test]
fn receipts_are_absolute_idempotent_and_revision_ordered() {
    let root = Temp::new();
    let stop = AtomicBool::new(false);
    let initial = usage::begin(&root.0, identity("call_one"), None, &stop).unwrap();
    let raw = std::fs::read(root.0.join(usage::RECEIPTS)).unwrap();
    let row = oms_context::parse_json(&raw).unwrap();
    assert_eq!(
        support::field(&row, "schema"),
        &oms_context::Json::Number("2".into())
    );
    assert_eq!(
        support::field(&row, "cleanup_complete"),
        &oms_context::Json::Null
    );
    // Schema 1 remains readable with unknown cleanup, and identical recording
    // preserves those historical bytes before schema-2 revisions are appended.
    let mut legacy = oms_json::parse_strict(&raw).unwrap();
    let oms_json::Value::Object(ref mut fields) = legacy else {
        panic!("receipt object");
    };
    fields.remove(&oms_json::PyString::from("cleanup_complete"));
    fields.insert("schema".into(), oms_json::Value::integer("1").unwrap());
    let mut legacy = oms_json::session_call_key_json_bytes(&legacy).unwrap();
    legacy.push(b'\n');
    root.write(usage::RECEIPTS, &legacy);
    assert_eq!(
        usage::load(&root.0).unwrap().calls[0]
            .receipt
            .cleanup_complete,
        None
    );
    assert!(!usage::record(&root.0, &initial, &stop).unwrap());
    assert_eq!(std::fs::read(root.0.join(usage::RECEIPTS)).unwrap(), legacy);
    assert_eq!(usage::load(&root.0).unwrap().calls[0].tokens, None);
    let mut partial = observe(initial.clone(), 2, 40, Cost::Unknown);
    partial.phase = Phase::Partial;
    usage::record(&root.0, &partial, &stop).unwrap();
    let final_ = observe(initial.clone(), 4, 100, Cost::Reported(90));
    assert!(usage::record(&root.0, &final_, &stop).unwrap());
    let before_duplicate = std::fs::metadata(root.0.join(usage::RECEIPTS)).unwrap();
    assert!(!usage::record(&root.0, &final_, &stop).unwrap());
    let after_duplicate = std::fs::metadata(root.0.join(usage::RECEIPTS)).unwrap();
    assert_eq!(before_duplicate.ino(), after_duplicate.ino());
    assert_eq!(
        before_duplicate.modified().unwrap(),
        after_duplicate.modified().unwrap()
    );
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
        r.cleanup_complete = [Some(true), Some(false), None][n];
        usage::record(&root.0, &r, &stop).unwrap();
    }
    let calls = usage::load(&root.0).unwrap().calls;
    assert!(
        calls
            .iter()
            .all(|c| c.cost_microusd.is_none() && c.receipt.phase == Phase::Unknown)
    );
    assert_eq!(
        calls
            .iter()
            .map(|c| c.receipt.cleanup_complete)
            .collect::<Vec<_>>(),
        vec![Some(true), Some(false), None]
    );
    let before = std::fs::read(root.0.join(usage::RECEIPTS)).unwrap();
    let mut correction = calls[0].receipt.clone();
    correction.revision += 1;
    correction.cleanup_complete = None;
    assert!(usage::record(&root.0, &correction, &stop).is_err());
    assert_eq!(std::fs::read(root.0.join(usage::RECEIPTS)).unwrap(), before);
    correction.cleanup_complete = Some(true);
    correction.phase = Phase::Final;
    correction.cost = Cost::Reported(1);
    usage::record(&root.0, &correction, &stop).unwrap();
    let mut known_usage = calls[1].receipt.clone();
    known_usage.revision += 1;
    known_usage.phase = Phase::Final;
    usage::record(&root.0, &known_usage, &stop).unwrap();
    assert_eq!(
        usage::load(&root.0).unwrap().calls[1]
            .receipt
            .cleanup_complete,
        Some(false)
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
    let report = root.call(&["--json"]);
    assert_eq!(report.0, 0);
    let report = oms_context::parse_json(&report.1).unwrap();
    let lifetime = support::field(&report, "lifetime_provider_usage");
    for (key, count) in [
        ("cleanup_confirmed_calls", "1"),
        ("cleanup_incomplete_calls", "1"),
        ("cleanup_unknown_calls", "2"),
    ] {
        assert_eq!(
            support::field(lifetime, key),
            &oms_context::Json::Number(count.into())
        );
    }
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
    let text = String::from_utf8(bytes.clone()).unwrap();
    let duplicate = text.replacen("\"call_id\":", "\"call_id\": \"conflict\", \"call_id\":", 1);
    assert_ne!(text, duplicate);
    let conflicting_ordinal = text.replace("\"call_one\"", "\"call_two\"");
    let conflicting_revision = text.replace("\"pending\"", "\"unknown\"");
    let unsupported = text.replace("\"schema\": 2", "\"schema\": 3");
    let invalid_cleanup = text.replace("\"cleanup_complete\": null", "\"cleanup_complete\": 1");
    let premature_cleanup =
        text.replace("\"cleanup_complete\": null", "\"cleanup_complete\": true");
    for modified in [&unsupported, &invalid_cleanup, &premature_cleanup] {
        assert_ne!(modified, &text);
    }
    assert_ne!(text, conflicting_ordinal);
    assert_ne!(text, conflicting_revision);
    let mut oversized = bytes[..bytes.len() - 1].to_vec();
    oversized.resize(oms_command_telemetry::store::MAX_ROW_BYTES, b' ');
    oversized.push(b'\n');
    for malformed in [
        bytes[..bytes.len() - 1].to_vec(),
        duplicate.into_bytes(),
        format!("{text}{conflicting_ordinal}").into_bytes(),
        format!("{text}{conflicting_revision}").into_bytes(),
        format!("{text}\n").into_bytes(),
        oversized,
        unsupported.into_bytes(),
        invalid_cleanup.into_bytes(),
        premature_cleanup.into_bytes(),
    ] {
        root.write(usage::RECEIPTS, &malformed);
        assert!(usage::load(&root.0).is_err());
        assert!(usage::begin(&root.0, identity("call_new"), None, &stop).is_err());
        assert!(
            usage::record(
                &root.0,
                &observe(receipt.clone(), 1, 10, Cost::Reported(1)),
                &stop
            )
            .is_err()
        );
        // An identical receipt must still validate every historical revision.
        assert!(usage::record(&root.0, &receipt, &stop).is_err());
        assert_eq!(
            malformed,
            std::fs::read(root.0.join(usage::RECEIPTS)).unwrap()
        );
    }
    root.write(usage::RECEIPTS, text.replace('\n', "\r\n"));
    assert_eq!(usage::load(&root.0).unwrap().calls.len(), 1);
    assert!(!usage::record(&root.0, &receipt, &stop).unwrap());
    root.write(usage::RECEIPTS, bytes);
    let mut impossible = observe(receipt, 1, 0, Cost::Unknown);
    impossible.usage.input_tokens = Some(10);
    impossible.usage.output_tokens = Some(1);
    assert!(usage::record(&root.0, &impossible, &stop).is_err());
}
