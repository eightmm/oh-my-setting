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
fn physical_request(
    identity: Identity,
    write: bool,
) -> oms_command_telemetry::store::physical::Request {
    oms_command_telemetry::store::physical::Request {
        identity,
        cumulative: None,
        write,
        campaign_manifest_sha256: None,
    }
}

fn resource_policy() -> oms_command_telemetry::store::resources::Policy {
    oms_command_telemetry::store::resources::Policy {
        domain: "fixture-domain".into(),
        epoch: "epoch-one".into(),
        host_generation_sha256: "a".repeat(64),
        max_teams: 1,
        max_invocations: 1,
        max_open_processes: 1,
        max_write_wip: 1,
        reviewer_headroom: 0,
    }
}
fn resource_binding(
    policy: &oms_command_telemetry::store::resources::Policy,
) -> oms_command_telemetry::store::resources::Binding {
    oms_command_telemetry::store::resources::Binding {
        policy_sha256: policy.sha256().unwrap(),
        host_generation_sha256: policy.host_generation_sha256.clone(),
        root_owner: "main-one".into(),
        root_generation: "created-one".into(),
        team: None,
        reviewer: false,
        persistent_session: false,
        cleanup_scopes: vec![oms_supervisor::process::Scope::ProcessGroup],
    }
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
    use std::path::Path;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    let root = Temp::private();
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
fn physical_intents_replay_without_resending_and_keep_unacknowledged_liability() {
    use oms_command_telemetry::store::{
        dispatch,
        physical::{self, Phase as IntentPhase, Registration},
    };
    use oms_lock::{Lock, Mode, Settings};
    use std::path::Path;
    use std::time::{Duration, Instant};
    let root = Temp::private();
    let stop = AtomicBool::new(false);
    let deadline = || Instant::now() + Duration::from_secs(2);
    for changed_operation in [false, true] {
        let existing_root = Temp::private();
        let mut request = identity("call_receipt_only");
        usage::begin(&existing_root.0, request.clone(), None, &stop).unwrap();
        let before = std::fs::read(existing_root.0.join(usage::RECEIPTS)).unwrap();
        if changed_operation {
            request.operation_id = "op_conflicting".into();
        }
        let error =
            physical::begin(&existing_root.0, request, None, false, &stop, deadline()).unwrap_err();
        assert!(
            !error.publication_uncertain,
            "a known receipt-only ID must refuse before publishing another intent"
        );
        assert!(!existing_root.0.join(dispatch::REGISTRY).exists());
        assert_eq!(
            std::fs::read(existing_root.0.join(usage::RECEIPTS)).unwrap(),
            before
        );
    }
    assert!(physical::load(&root.0, &stop).unwrap().is_empty());
    assert!(
        !root.0.join(dispatch::LOCK).exists(),
        "observation does not create a lock or state"
    );
    let invalid =
        physical::begin(&root.0, Identity::default(), None, false, &stop, deadline()).unwrap_err();
    assert!(!invalid.publication_uncertain);
    assert!(!root.0.join(dispatch::REGISTRY).exists());
    root.write(
        dispatch::REGISTRY,
        br#"{"schema":1,"calls":{"legacy":{"state":"unknown"}},"opaque":{"retain":true}}"#,
    );
    let request = identity("call_registered");
    let Registration::New(receipt) =
        physical::begin(&root.0, request.clone(), None, true, &stop, deadline()).unwrap()
    else {
        panic!("fresh registration");
    };
    assert_eq!(receipt.phase, Phase::Pending);
    let intents = physical::load(&root.0, &stop).unwrap();
    assert_eq!(intents.len(), 1);
    assert_eq!(intents[0].identity, request);
    assert_eq!(intents[0].phase, IntentPhase::Ready);
    assert_eq!(intents[0].usage_ordinal, Some(receipt.ordinal));
    assert!(intents[0].write);
    let before = std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap();
    let replay = physical::begin(&root.0, request.clone(), None, true, &stop, deadline()).unwrap();
    assert!(matches!(
        replay,
        Registration::Existing {
            phase: IntentPhase::Ready,
            receipt: Some(_)
        }
    ));
    assert_eq!(usage::load(&root.0).unwrap().calls.len(), 1);
    assert_eq!(
        std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
        before
    );
    assert!(
        physical::begin(&root.0, request, None, false, &stop, deadline()).is_err(),
        "changed access cannot reuse the same physical identity"
    );
    assert_eq!(
        std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
        before
    );
    let target = oms_command_telemetry::store::canonical_index(
        &root.0,
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
    let orphan = identity("call_orphan");
    let failed = physical::begin(
        &root.0,
        orphan.clone(),
        None,
        false,
        &stop,
        Instant::now() + Duration::from_millis(70),
    )
    .unwrap_err();
    assert!(
        failed.publication_uncertain,
        "the intent survives refused pending registration"
    );
    assert_eq!(usage::load(&root.0).unwrap().calls.len(), 1);
    let pending = physical::load(&root.0, &stop)
        .unwrap()
        .into_iter()
        .find(|intent| intent.identity == orphan)
        .unwrap();
    assert_eq!(pending.phase, IntentPhase::Reserved);
    assert_eq!(pending.usage_ordinal, None);
    drop(held);
    let raw = std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap();
    assert!(matches!(
        physical::begin(&root.0, orphan, None, false, &stop, deadline()).unwrap(),
        Registration::Existing {
            phase: IntentPhase::Reserved,
            receipt: None
        }
    ));
    assert_eq!(std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(), raw);
    let value = oms_json::parse_strict(&raw).unwrap();
    assert_eq!(
        value
            .get("opaque")
            .unwrap()
            .get("retain")
            .unwrap()
            .as_bool(),
        Some(true)
    );
    assert!(value.get("calls").unwrap().get("legacy").is_some());
    let mut future = oms_json::parse_strict(&raw).unwrap();
    // Unsupported producer versions retain the existing history instead of resetting it.
    future
        .as_object_mut()
        .unwrap()
        .get_mut(&oms_json::PyString::from("physical"))
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("schema".into(), oms_json::Value::integer("99").unwrap());
    let future = oms_json::session_call_key_json_bytes(&future).unwrap();
    root.write(dispatch::REGISTRY, &future);
    assert!(physical::load(&root.0, &stop).is_err());
    assert!(
        physical::begin(
            &root.0,
            identity("call_future"),
            None,
            false,
            &stop,
            deadline()
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
        future
    );
}
#[test]
fn resource_domain_reserves_mixed_calls_and_keeps_independent_custody() {
    use oms_command_telemetry::store::{
        dispatch,
        physical::{self, Registration},
        resources::{self, Binding, DecisionEvidence, DecisionTarget, Policy, Team},
    };
    use oms_supervisor::process::{OwnedCleanup, PhysicalCall, Progress, Reactor, Request, Scope};
    use sha2::{Digest, Sha256};
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};
    fn cleanup(call: &str, scope: Scope) -> OwnedCleanup {
        let mut reactor = Reactor::new();
        let mut request = Request::new(vec!["/bin/sh".into(), "-c".into(), "true".into()]);
        request.scope = scope;
        request.physical_call = Some(PhysicalCall::new(call).unwrap());
        request.deadline = Some(Instant::now() + Duration::from_secs(2));
        request.terminate_grace = Duration::ZERO;
        let job = reactor.start(request).unwrap();
        let until = Instant::now() + Duration::from_secs(4);
        loop {
            match reactor.poll(job, Duration::from_millis(10)).unwrap() {
                Progress::Complete(done) => return done.owned_cleanup().unwrap().clone(),
                Progress::Pending(_) => assert!(Instant::now() < until),
            }
        }
    }
    let stop = AtomicBool::new(false);
    let deadline = || Instant::now() + Duration::from_secs(3);
    let policy = resource_policy();
    let decision_body = br#"{"schema":1,"fixture":"canonical-owner-decision"}"#;
    let decision = DecisionEvidence {
        record: ".oms/plan/tasks.json".into(),
        sha256: format!("{:x}", Sha256::digest(decision_body)),
        revision: "decision-one".into(),
    };
    let legacy = resource_binding(&policy);
    let team = Binding {
        team: Some(Team {
            id: "team-one".into(),
            spec_sha256: "b".repeat(64),
            member: "worker-one".into(),
            member_generation: i128::from(u64::MAX) + 1,
        }),
        ..legacy.clone()
    };
    let root = Temp::private();
    assert!(resources::snapshot(&root.0, &stop).unwrap().is_none());
    assert!(!root.0.join(dispatch::LOCK).exists());
    resources::activate(&root.0, &policy, None, &stop, deadline()).unwrap();
    let initial = std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap();
    resources::activate(&root.0, &policy, Some(&initial), &stop, deadline()).unwrap();
    assert!(
        usage::begin(&root.0, identity("call_direct_receipt"), None, &stop).is_err(),
        "an active resource domain must not admit a receipt-only call"
    );
    assert_eq!(
        std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
        initial
    );
    assert!(
        physical::begin(
            &root.0,
            identity("call_unbound"),
            None,
            false,
            &stop,
            deadline()
        )
        .is_err()
    );
    assert!(!root.0.join(usage::RECEIPTS).exists());
    assert_eq!(
        std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
        initial
    );
    resources::reserve_team(&root.0, &team, &stop, deadline()).unwrap();
    let team_reserved = std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap();
    resources::reserve_team(&root.0, &team, &stop, deadline()).unwrap();
    assert_eq!(
        std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
        team_reserved
    );
    let mut other_team = team.clone();
    other_team.team.as_mut().unwrap().spec_sha256 = "c".repeat(64);
    assert!(resources::reserve_team(&root.0, &other_team, &stop, deadline()).is_err());
    let barrier = Arc::new(Barrier::new(3));
    let results = std::thread::scope(|scope| {
        let workers = [legacy.clone(), team.clone()]
            .into_iter()
            .enumerate()
            .map(|(index, binding)| {
                let barrier = barrier.clone();
                let repo = &root.0;
                let stop = &stop;
                scope.spawn(move || {
                    barrier.wait();
                    let identity = identity(&format!("call_mixed_{index}"));
                    let result = physical::begin_bound_checked(
                        repo,
                        physical_request(identity.clone(), true),
                        &binding,
                        stop,
                        deadline(),
                        |_, _| Ok(()),
                    );
                    (identity, binding, result)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        results
            .iter()
            .filter(|(_, _, result)| matches!(result, Ok(Registration::New(_))))
            .count(),
        1
    );
    let (winning, binding, result) = results
        .into_iter()
        .find(|(_, _, result)| result.is_ok())
        .unwrap();
    assert!(matches!(result, Ok(Registration::New(_))));
    let total = resources::occupancy(&root.0, &stop).unwrap().unwrap();
    assert_eq!(
        (
            total.teams,
            total.invocations,
            total.open_processes,
            total.write_wip
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(usage::load(&root.0).unwrap().calls.len(), 1);
    let admitted = std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap();
    let mut old_registry = oms_json::parse_strict(&admitted).unwrap();
    let old_resource = old_registry
        .as_object_mut()
        .unwrap()
        .get_mut(&oms_json::PyString::from("resources"))
        .unwrap()
        .as_object_mut()
        .unwrap();
    for collection in ["teams", "write_wip"] {
        for entry in old_resource
            .get_mut(&oms_json::PyString::from(collection))
            .unwrap()
            .as_object_mut()
            .unwrap()
            .values_mut()
        {
            entry
                .as_object_mut()
                .unwrap()
                .remove(&oms_json::PyString::from("revision"));
        }
    }
    root.write(
        dispatch::REGISTRY,
        oms_json::session_call_key_json_bytes(&old_registry).unwrap(),
    );
    assert_eq!(
        resources::occupancy(&root.0, &stop).unwrap().unwrap(),
        total
    );
    root.write(dispatch::REGISTRY, &admitted);
    for damage in [
        "downgrade",
        "missing-claims",
        "missing-intents",
        "legacy-dispatch",
        "resource-version",
    ] {
        let mut value = oms_json::parse_strict(&admitted).unwrap();
        let fields = value.as_object_mut().unwrap();
        match damage {
            "downgrade" => {
                fields.insert("schema".into(), oms_json::Value::integer("1").unwrap());
                fields.remove(&oms_json::PyString::from("resources"));
            }
            "missing-claims" => {
                fields
                    .get_mut(&oms_json::PyString::from("resources"))
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert("claims".into(), oms_json::parse_strict(b"{}").unwrap());
            }
            "missing-intents" => {
                fields.remove(&oms_json::PyString::from("physical"));
            }
            "legacy-dispatch" => {
                fields.insert(
                    "calls".into(),
                    oms_json::parse_strict(br#"{"legacy":{"state":"unknown"}}"#).unwrap(),
                );
            }
            "resource-version" => {
                fields
                    .get_mut(&oms_json::PyString::from("resources"))
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert("schema".into(), oms_json::Value::integer("99").unwrap());
            }
            _ => unreachable!(),
        }
        let damaged = oms_json::session_call_key_json_bytes(&value).unwrap();
        root.write(dispatch::REGISTRY, &damaged);
        assert!(dispatch::read_snapshot(&root.0, &stop).is_err(), "{damage}");
        assert!(physical::load(&root.0, &stop).is_err(), "{damage}");
        assert!(resources::snapshot(&root.0, &stop).is_err(), "{damage}");
        assert!(
            physical::begin(
                &root.0,
                identity("call_after_damage"),
                None,
                false,
                &stop,
                deadline()
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
            damaged
        );
        root.write(dispatch::REGISTRY, &admitted);
    }
    assert!(matches!(
        physical::begin_bound_checked(
            &root.0,
            physical_request(winning.clone(), true),
            &binding,
            &stop,
            deadline(),
            |_, _| panic!("replay never readmits")
        ),
        Ok(Registration::Existing { .. })
    ));
    assert_eq!(
        std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
        admitted
    );
    let mut changed = binding.clone();
    changed.root_generation = "changed-generation".into();
    assert!(
        physical::begin_bound_checked(
            &root.0,
            physical_request(winning.clone(), true),
            &changed,
            &stop,
            deadline(),
            |_, _| Ok(())
        )
        .is_err()
    );
    let call = PhysicalCall::new(&winning.call_id).unwrap();
    resources::fence(&root.0, &binding, &call, &stop, deadline()).unwrap();
    let acknowledgement = std::fs::read(root.0.join(usage::RECEIPTS)).unwrap();
    root.write(usage::RECEIPTS, b"");
    assert!(
        resources::fence(&root.0, &binding, &call, &stop, deadline()).is_err(),
        "ready intent cannot replace its missing usage acknowledgement"
    );
    let mut changed_ordinal = oms_json::parse_strict(&acknowledgement).unwrap();
    changed_ordinal
        .as_object_mut()
        .unwrap()
        .insert("ordinal".into(), oms_json::Value::integer("9").unwrap());
    let mut changed_ordinal = oms_json::session_call_key_json_bytes(&changed_ordinal).unwrap();
    changed_ordinal.push(b'\n');
    root.write(usage::RECEIPTS, &changed_ordinal);
    assert!(resources::fence(&root.0, &binding, &call, &stop, deadline()).is_err());
    root.write(usage::RECEIPTS, &acknowledgement);
    let foreign = cleanup("call_foreign", Scope::ProcessGroup);
    assert!(
        resources::settle_execution(&root.0, &binding, &call, &foreign, 1, &stop, deadline())
            .is_err()
    );
    assert_eq!(
        std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
        admitted
    );
    let receipt_bytes = std::fs::read(root.0.join(usage::RECEIPTS)).unwrap();
    let proof = cleanup(&winning.call_id, Scope::ProcessGroup);
    resources::settle_execution(&root.0, &binding, &call, &proof, 1, &stop, deadline()).unwrap();
    assert_eq!(
        std::fs::read(root.0.join(usage::RECEIPTS)).unwrap(),
        receipt_bytes
    );
    let settled = std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap();
    resources::settle_execution(&root.0, &binding, &call, &proof, 1, &stop, deadline()).unwrap();
    assert_eq!(
        std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
        settled
    );
    let total = resources::occupancy(&root.0, &stop).unwrap().unwrap();
    assert_eq!(
        (total.invocations, total.open_processes, total.write_wip),
        (0, 0, 1)
    );
    assert!(
        physical::begin_bound_checked(
            &root.0,
            physical_request(identity("call_new_writer"), true),
            &binding,
            &stop,
            deadline(),
            |_, _| Ok(())
        )
        .is_err()
    );
    let retry = Identity {
        call_id: "call_same_operation_retry".into(),
        ..winning.clone()
    };
    physical::begin_bound_checked(
        &root.0,
        physical_request(retry.clone(), true),
        &binding,
        &stop,
        deadline(),
        |_, _| Ok(()),
    )
    .unwrap();
    assert!(
        resources::retire_write_checked(
            &root.0,
            &binding,
            &winning.operation_id,
            1,
            &stop,
            deadline(),
            |_, _| panic!("running custody cannot reach a decision callback")
        )
        .is_err()
    );
    assert_eq!(
        resources::occupancy(&root.0, &stop)
            .unwrap()
            .unwrap()
            .write_wip,
        1
    );
    resources::hold(&root.0, &policy.sha256().unwrap(), &stop, deadline()).unwrap();
    let held_busy = std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap();
    assert!(
        resources::reconcile_checked(
            &root.0,
            &held_busy,
            &Policy {
                epoch: "busy-epoch".into(),
                ..policy.clone()
            },
            &stop,
            deadline(),
            |_, _| panic!("held unknown work cannot reach epoch decision checks")
        )
        .is_err()
    );
    assert_eq!(
        resources::fence(
            &root.0,
            &binding,
            &PhysicalCall::new(&retry.call_id).unwrap(),
            &stop,
            deadline()
        )
        .unwrap_err()
        .exit_code(),
        75
    );
    assert!(!resources::snapshot(&root.0, &stop).unwrap().unwrap().active);
    assert!(
        physical::begin_bound_checked(
            &root.0,
            physical_request(identity("call_held"), false),
            &binding,
            &stop,
            deadline(),
            |_, _| Ok(())
        )
        .is_err()
    );
    let proof = cleanup(&retry.call_id, Scope::ProcessGroup);
    resources::settle_execution(
        &root.0,
        &binding,
        &PhysicalCall::new(&retry.call_id).unwrap(),
        &proof,
        1,
        &stop,
        deadline(),
    )
    .unwrap();
    assert_eq!(
        resources::occupancy(&root.0, &stop)
            .unwrap()
            .unwrap()
            .invocations,
        0
    );

    root.write(&decision.record, decision_body);
    let linked_decision = root.0.join(".oms/plan/linked-decision.json");
    std::fs::hard_link(root.0.join(&decision.record), &linked_decision).unwrap();
    assert!(
        resources::retire_write_checked(
            &root.0,
            &binding,
            &winning.operation_id,
            1,
            &stop,
            deadline(),
            |_, _| Ok(decision.clone())
        )
        .is_err(),
        "a matching hash cannot bypass canonical source ownership and single-link checks"
    );
    std::fs::remove_file(linked_decision).unwrap();
    let before_retirement = std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap();
    let mut foreign_owner = binding.clone();
    foreign_owner.root_generation = "old-owner-generation".into();
    assert!(
        resources::retire_write_checked(
            &root.0,
            &foreign_owner,
            &winning.operation_id,
            1,
            &stop,
            deadline(),
            |_, _| panic!("stale owner must refuse before checking evidence")
        )
        .is_err()
    );
    let mut forged = decision.clone();
    forged.sha256 = "0".repeat(64);
    assert!(
        resources::retire_write_checked(
            &root.0,
            &binding,
            &winning.operation_id,
            1,
            &stop,
            deadline(),
            |_, _| Ok(forged.clone())
        )
        .is_err()
    );
    assert!(
        resources::retire_write_checked(
            &root.0,
            &binding,
            &winning.operation_id,
            2,
            &stop,
            deadline(),
            |_, _| panic!("stale revision must refuse before checking evidence")
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
        before_retirement
    );
    resources::retire_write_checked(
        &root.0,
        &binding,
        &winning.operation_id,
        1,
        &stop,
        deadline(),
        |_, target| {
            assert!(
                matches!(target, DecisionTarget::Write { operation_id, revision: 1, .. }
                if *operation_id == winning.operation_id)
            );
            Ok(decision.clone())
        },
    )
    .unwrap();
    let retired_write = std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap();
    resources::retire_write_checked(
        &root.0,
        &binding,
        &winning.operation_id,
        1,
        &stop,
        deadline(),
        |_, _| Ok(decision.clone()),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
        retired_write
    );
    let total = resources::occupancy(&root.0, &stop).unwrap().unwrap();
    assert_eq!(
        (total.teams, total.write_wip),
        (1, 0),
        "member decisions do not retire the Team"
    );
    let mut checks = 0;
    let lost_guard =
        resources::retire_team_checked(&root.0, &team, 1, &stop, deadline(), |_, _| {
            checks += 1;
            if checks == 2 {
                Err(oms_command_telemetry::store::StoreError::Changed)
            } else {
                Ok(decision.clone())
            }
        })
        .unwrap_err();
    assert!(
        lost_guard.publication_uncertain,
        "post-CAS producer failure remains uncertain"
    );
    resources::retire_team_checked(&root.0, &team, 1, &stop, deadline(), |_, _| {
        Ok(decision.clone())
    })
    .unwrap();
    assert_eq!(
        resources::occupancy(&root.0, &stop).unwrap().unwrap(),
        resources::Occupancy::default()
    );
    let retained_usage = std::fs::read(root.0.join(usage::RECEIPTS)).unwrap();
    let next_policy = Policy {
        epoch: "epoch-two".into(),
        host_generation_sha256: "d".repeat(64),
        ..policy.clone()
    };
    let drained = std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap();
    assert!(
        resources::reconcile_checked(
            &root.0,
            &retired_write,
            &next_policy,
            &stop,
            deadline(),
            |_, _| Ok(decision.clone())
        )
        .is_err(),
        "epoch reconciliation requires exact source bytes"
    );
    resources::reconcile_checked(
        &root.0,
        &drained,
        &next_policy,
        &stop,
        deadline(),
        |_, target| {
            assert!(
                matches!(target, DecisionTarget::PolicyEpoch { current, next }
                if current.epoch == "epoch-one" && next.epoch == "epoch-two")
            );
            Ok(decision.clone())
        },
    )
    .unwrap();
    let evolved = resources::snapshot(&root.0, &stop).unwrap().unwrap();
    assert!(evolved.active);
    assert_eq!(evolved.policy, next_policy);
    assert_eq!(
        physical::load(&root.0, &stop).unwrap().len(),
        2,
        "old physical claims stay readable"
    );
    assert_eq!(
        std::fs::read(root.0.join(usage::RECEIPTS)).unwrap(),
        retained_usage,
        "unknown spend is never refunded"
    );
    resources::hold(&root.0, &next_policy.sha256().unwrap(), &stop, deadline()).unwrap();
    let held_epoch = std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap();
    for reused in [
        &policy,
        &Policy {
            epoch: policy.epoch.clone(),
            ..next_policy.clone()
        },
    ] {
        assert!(
            resources::reconcile_checked(
                &root.0,
                &held_epoch,
                reused,
                &stop,
                deadline(),
                |_, _| panic!("old epoch identifiers cannot reach producer checks")
            )
            .is_err()
        );
    }
    assert_eq!(
        std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
        held_epoch
    );
    let third_policy = Policy {
        epoch: "epoch-three".into(),
        ..next_policy.clone()
    };
    resources::reconcile_checked(
        &root.0,
        &held_epoch,
        &third_policy,
        &stop,
        deadline(),
        |_, _| Ok(decision.clone()),
    )
    .unwrap();
    let history =
        oms_json::parse_strict(&std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap()).unwrap();
    let history = history.get("resources").unwrap();
    assert_eq!(
        history
            .get("previous_policies")
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        history
            .get("epoch_decisions")
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(usage::load(&root.0).unwrap().calls.len(), 2);

    let fresh_binding = resource_binding(&third_policy);
    physical::begin_bound_checked(
        &root.0,
        physical_request(identity("call_fresh_epoch"), false),
        &fresh_binding,
        &stop,
        deadline(),
        |_, _| Ok(()),
    )
    .unwrap();
    assert_eq!(
        resources::occupancy(&root.0, &stop)
            .unwrap()
            .unwrap()
            .invocations,
        1
    );

    let sessions = Temp::private();
    let session_policy = Policy {
        max_invocations: 2,
        ..policy.clone()
    };
    resources::activate(&sessions.0, &session_policy, None, &stop, deadline()).unwrap();
    let session_binding = Binding {
        policy_sha256: session_policy.sha256().unwrap(),
        persistent_session: true,
        ..legacy.clone()
    };
    let session_id = identity("call_persistent");
    physical::begin_bound_checked(
        &sessions.0,
        physical_request(session_id.clone(), false),
        &session_binding,
        &stop,
        deadline(),
        |_, _| Ok(()),
    )
    .unwrap();
    let proof = cleanup(&session_id.call_id, Scope::ProcessGroup);
    resources::settle_execution(
        &sessions.0,
        &session_binding,
        &PhysicalCall::new(&session_id.call_id).unwrap(),
        &proof,
        1,
        &stop,
        deadline(),
    )
    .unwrap();
    let total = resources::occupancy(&sessions.0, &stop).unwrap().unwrap();
    assert_eq!((total.invocations, total.open_processes), (0, 1));
    assert!(
        physical::begin_bound_checked(
            &sessions.0,
            physical_request(identity("call_open_cap"), false),
            &session_binding,
            &stop,
            deadline(),
            |_, _| Ok(())
        )
        .is_err()
    );
    let session_call = PhysicalCall::new(&session_id.call_id).unwrap();
    let narrow_proof = cleanup(&session_id.call_id, Scope::Leader);
    assert!(
        resources::close_session(
            &sessions.0,
            &session_binding,
            &session_call,
            &narrow_proof,
            2,
            &stop,
            deadline()
        )
        .is_err(),
        "leader cleanup cannot close a process-group session"
    );
    assert!(
        resources::close_session(
            &sessions.0,
            &session_binding,
            &session_call,
            &proof,
            1,
            &stop,
            deadline()
        )
        .is_err(),
        "session close uses its current independent revision"
    );
    assert!(
        resources::close_session(
            &sessions.0,
            &session_binding,
            &session_call,
            &foreign,
            2,
            &stop,
            deadline()
        )
        .is_err()
    );
    resources::close_session(
        &sessions.0,
        &session_binding,
        &session_call,
        &proof,
        2,
        &stop,
        deadline(),
    )
    .unwrap();
    let closed = std::fs::read(sessions.0.join(dispatch::REGISTRY)).unwrap();
    resources::close_session(
        &sessions.0,
        &session_binding,
        &session_call,
        &proof,
        2,
        &stop,
        deadline(),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(sessions.0.join(dispatch::REGISTRY)).unwrap(),
        closed
    );
    assert_eq!(
        resources::occupancy(&sessions.0, &stop)
            .unwrap()
            .unwrap()
            .open_processes,
        0
    );
    let idle_id = identity("call_idle_session");
    physical::begin_bound_checked(
        &sessions.0,
        physical_request(idle_id.clone(), false),
        &session_binding,
        &stop,
        deadline(),
        |_, _| Ok(()),
    )
    .unwrap();
    sessions.write(&decision.record, decision_body);
    let idle_call = PhysicalCall::new(&idle_id.call_id).unwrap();
    let before_idle = std::fs::read(sessions.0.join(dispatch::REGISTRY)).unwrap();
    assert!(
        resources::idle_session_checked(
            &sessions.0,
            &session_binding,
            &idle_call,
            1,
            &stop,
            deadline(),
            |_, _| Err(oms_command_telemetry::store::StoreError::Changed)
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(sessions.0.join(dispatch::REGISTRY)).unwrap(),
        before_idle
    );
    resources::idle_session_checked(
        &sessions.0, &session_binding, &idle_call, 1, &stop, deadline(), |_, target| {
            assert!(matches!(target, DecisionTarget::SessionIdle { call, revision: 1, .. } if *call == &idle_call));
            Ok(decision.clone())
        }
    ).unwrap();
    resources::idle_session_checked(
        &sessions.0,
        &session_binding,
        &idle_call,
        1,
        &stop,
        deadline(),
        |_, _| Ok(decision.clone()),
    )
    .unwrap();
    let total = resources::occupancy(&sessions.0, &stop).unwrap().unwrap();
    assert_eq!((total.invocations, total.open_processes), (0, 1));
    let idle_proof = cleanup(&idle_id.call_id, Scope::ProcessGroup);
    resources::close_session(
        &sessions.0,
        &session_binding,
        &idle_call,
        &idle_proof,
        2,
        &stop,
        deadline(),
    )
    .unwrap();

    let strict = Temp::private();
    resources::activate(&strict.0, &policy, None, &stop, deadline()).unwrap();
    let strict_binding = Binding {
        cleanup_scopes: vec![Scope::WholeTree],
        ..legacy.clone()
    };
    let strict_id = identity("call_narrow_proof");
    physical::begin_bound_checked(
        &strict.0,
        physical_request(strict_id.clone(), false),
        &strict_binding,
        &stop,
        deadline(),
        |_, _| Ok(()),
    )
    .unwrap();
    let proof = cleanup(&strict_id.call_id, Scope::ProcessGroup);
    assert!(
        resources::settle_execution(
            &strict.0,
            &strict_binding,
            &PhysicalCall::new(&strict_id.call_id).unwrap(),
            &proof,
            1,
            &stop,
            deadline()
        )
        .is_err()
    );
    assert_eq!(
        resources::occupancy(&strict.0, &stop)
            .unwrap()
            .unwrap()
            .invocations,
        1
    );

    for only_receipt in [false, true] {
        let old = Temp::private();
        if only_receipt {
            usage::begin(&old.0, identity("call_old_receipt"), None, &stop).unwrap();
        } else {
            physical::begin(
                &old.0,
                identity("call_old_main"),
                None,
                true,
                &stop,
                deadline(),
            )
            .unwrap();
        }
        let old_registry = dispatch::read_snapshot(&old.0, &stop).unwrap();
        let old_usage = std::fs::read(old.0.join(usage::RECEIPTS)).unwrap();
        assert!(
            resources::activate(&old.0, &policy, old_registry.as_deref(), &stop, deadline())
                .is_err()
        );
        assert_eq!(
            dispatch::read_snapshot(&old.0, &stop).unwrap(),
            old_registry
        );
        assert_eq!(
            std::fs::read(old.0.join(usage::RECEIPTS)).unwrap(),
            old_usage
        );
    }
}

#[test]
fn resource_partial_registration_and_reviewer_headroom_keep_liability() {
    use oms_command_telemetry::store::{
        dispatch,
        physical::{self, Registration},
        resources::{self, Policy},
    };
    use oms_lock::{Lock, Mode, Settings};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};
    let stop = AtomicBool::new(false);
    let deadline = || Instant::now() + Duration::from_secs(3);
    let policy = resource_policy();
    let binding = resource_binding(&policy);
    assert_eq!(Policy::parse(&policy.to_json().unwrap()).unwrap(), policy);
    let unsupported = String::from_utf8(policy.to_json().unwrap())
        .unwrap()
        .replace("repository-host-local-observed", "provider-account-global");
    assert!(Policy::parse(unsupported.as_bytes()).is_err());
    assert!(
        Policy {
            max_invocations: 0,
            ..policy.clone()
        }
        .sha256()
        .is_err()
    );
    let root = Temp::private();
    assert!(resources::activate(&root.0, &policy, None, &stop, Instant::now()).is_err());
    assert!(
        resources::activate(&root.0, &policy, None, &AtomicBool::new(true), deadline()).is_err()
    );
    assert!(!root.0.join(dispatch::REGISTRY).exists());
    resources::activate(&root.0, &policy, None, &stop, deadline()).unwrap();
    root.write(usage::RECEIPTS, b"");
    std::fs::set_permissions(
        root.0.join(".oms/usage"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::set_permissions(
        root.0.join(usage::RECEIPTS),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let target = oms_command_telemetry::store::canonical_index(
        &root.0,
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
    let request = || physical_request(identity("call_partial_resource"), false);
    let failed = physical::begin_bound_checked(
        &root.0,
        request(),
        &binding,
        &stop,
        Instant::now() + Duration::from_millis(70),
        |_, _| Ok(()),
    )
    .unwrap_err();
    assert!(failed.publication_uncertain);
    assert_eq!(
        physical::load(&root.0, &stop).unwrap()[0].phase,
        physical::Phase::Reserved
    );
    assert_eq!(
        resources::occupancy(&root.0, &stop)
            .unwrap()
            .unwrap()
            .invocations,
        1
    );
    assert!(usage::load(&root.0).unwrap().calls.is_empty());
    drop(held);
    let partial = std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap();
    assert!(matches!(
        physical::begin_bound_checked(
            &root.0,
            request(),
            &binding,
            &stop,
            deadline(),
            |_, _| panic!("partial replay cannot resend")
        ),
        Ok(Registration::Existing {
            phase: physical::Phase::Reserved,
            receipt: None
        })
    ));
    assert_eq!(
        std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
        partial
    );
    assert!(
        physical::begin_bound_checked(
            &root.0,
            physical_request(identity("call_after_partial"), false),
            &binding,
            &stop,
            deadline(),
            |_, _| Ok(())
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(root.0.join(dispatch::REGISTRY)).unwrap(),
        partial
    );

    let reviews = Temp::private();
    let policy = Policy {
        max_invocations: 2,
        max_open_processes: 2,
        reviewer_headroom: 1,
        ..policy.clone()
    };
    let binding = resource_binding(&policy);
    resources::activate(&reviews.0, &policy, None, &stop, deadline()).unwrap();
    let Registration::New(receipt) = physical::begin_bound_checked(
        &reviews.0,
        physical_request(identity("call_worker"), false),
        &binding,
        &stop,
        deadline(),
        |_, _| Ok(()),
    )
    .unwrap() else {
        panic!("worker admission");
    };
    let mut final_ = observe(receipt, 1, 0, Cost::Unknown);
    final_.cleanup_complete = Some(true);
    usage::record(&reviews.0, &final_, &stop).unwrap();
    assert_eq!(
        resources::occupancy(&reviews.0, &stop)
            .unwrap()
            .unwrap()
            .invocations,
        1,
        "a usage boolean cannot release a resource claim"
    );
    let exhausted = physical::begin_bound_checked(
        &reviews.0,
        physical_request(identity("call_worker_overflow"), false),
        &binding,
        &stop,
        deadline(),
        |_, _| Ok(()),
    )
    .unwrap_err();
    assert_eq!(exhausted.exit_code(), 3);
    let reviewer = resources::Binding {
        reviewer: true,
        ..binding.clone()
    };
    physical::begin_bound_checked(
        &reviews.0,
        physical_request(identity("call_reviewer"), false),
        &reviewer,
        &stop,
        deadline(),
        |_, _| Ok(()),
    )
    .unwrap();
    assert_eq!(
        resources::occupancy(&reviews.0, &stop)
            .unwrap()
            .unwrap()
            .invocations,
        2
    );
    assert!(
        physical::begin_bound_checked(
            &reviews.0,
            physical_request(identity("call_reviewer_overflow"), false),
            &reviewer,
            &stop,
            deadline(),
            |_, _| Ok(())
        )
        .is_err()
    );

    let racing = Temp::private();
    let barrier = Arc::new(Barrier::new(3));
    let (activation, receipt) = std::thread::scope(|scope| {
        let repo = &racing.0;
        let policy = &policy;
        let stop = &stop;
        let start = barrier.clone();
        let activate = scope.spawn(move || {
            start.wait();
            resources::activate(repo, policy, None, stop, deadline())
        });
        let start = barrier.clone();
        let receipt = scope.spawn(move || {
            start.wait();
            usage::begin(repo, identity("call_racing_receipt"), None, stop)
        });
        barrier.wait();
        (activate.join().unwrap(), receipt.join().unwrap())
    });
    assert_ne!(
        activation.is_ok(),
        receipt.is_ok(),
        "activation and direct receipt must serialize"
    );
    if activation.is_ok() {
        assert!(usage::load(&racing.0).unwrap().calls.is_empty());
        assert_eq!(
            resources::occupancy(&racing.0, &stop)
                .unwrap()
                .unwrap()
                .invocations,
            0
        );
    } else {
        assert!(resources::snapshot(&racing.0, &stop).unwrap().is_none());
        assert_eq!(usage::load(&racing.0).unwrap().calls.len(), 1);
    }
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
