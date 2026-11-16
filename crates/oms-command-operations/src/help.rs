// Canonical parser help/usage, captured without executing any operation.
use crate::App;
pub(crate) fn text(app: App, command: &str) -> (&'static str, &'static str) {
    match (app, command) {
        (App::Events, "") => (
            r###"usage: agent-events.sh [-h] [--repo REPO]
                       {start,transition,recover-verify-heartbeat,heartbeat,usage,show,list,resume,follow,reconcile,validate,compact}
                       ...

Append and project durable agent lifecycle events.

positional arguments:
  {start,transition,recover-verify-heartbeat,heartbeat,usage,show,list,resume,follow,reconcile,validate,compact}
    start               create a queued attempt
    transition          append a validated state transition
    recover-verify-heartbeat
                        recover an exact heartbeat-expired generation after
                        verification passed
    heartbeat           append runner liveness
    usage               append measured usage deltas
    show                project one attempt
    list                list projected attempts
    resume              create a new attempt from a durable spec
    follow              stream lifecycle events as JSONL
    reconcile           find stale live attempts
    validate            validate the append-only stream
    compact             drop long-terminal attempts' event streams (whole
                        streams only)

options:
  -h, --help            show this help message and exit
  --repo REPO
"###,
            r###"usage: agent-events.sh [-h] [--repo REPO]
                       {start,transition,recover-verify-heartbeat,heartbeat,usage,show,list,resume,follow,reconcile,validate,compact}
                       ...
"###,
        ),
        (App::Events, "start") => (
            r###"usage: agent-events.sh start [-h] [--provider PROVIDER] --tool TOOL
                             [--task-id TASK_ID] [--run-id RUN_ID]
                             [--parent-attempt-id PARENT_ATTEMPT_ID]
                             [--ref NAME=VALUE]
                             [--max-wall-seconds MAX_WALL_SECONDS]
                             [--max-tokens MAX_TOKENS]
                             [--max-cost-microusd MAX_COST_MICROUSD]
                             [--idempotency-key IDEMPOTENCY_KEY]
                             [--then STATE[:KEY]] [--then-actor THEN_ACTOR]
                             [--then-actor-kind THEN_ACTOR_KIND]

options:
  -h, --help            show this help message and exit
  --provider PROVIDER
  --tool TOOL
  --task-id TASK_ID
  --run-id RUN_ID
  --parent-attempt-id PARENT_ATTEMPT_ID
  --ref NAME=VALUE      bounded relative reference metadata; never changes
                        authority
  --max-wall-seconds MAX_WALL_SECONDS
  --max-tokens MAX_TOKENS
  --max-cost-microusd MAX_COST_MICROUSD
  --idempotency-key IDEMPOTENCY_KEY
  --then STATE[:KEY]    append this state transition right after creation, in
                        the same process; KEY is its idempotency key
                        (repeatable, in order)
  --then-actor THEN_ACTOR
  --then-actor-kind THEN_ACTOR_KIND
"###,
            r###"usage: agent-events.sh start [-h] [--provider PROVIDER] --tool TOOL
                             [--task-id TASK_ID] [--run-id RUN_ID]
                             [--parent-attempt-id PARENT_ATTEMPT_ID]
                             [--ref NAME=VALUE]
                             [--max-wall-seconds MAX_WALL_SECONDS]
                             [--max-tokens MAX_TOKENS]
                             [--max-cost-microusd MAX_COST_MICROUSD]
                             [--idempotency-key IDEMPOTENCY_KEY]
                             [--then STATE[:KEY]] [--then-actor THEN_ACTOR]
                             [--then-actor-kind THEN_ACTOR_KIND]
"###,
        ),
        (App::Events, "transition") => (
            r###"usage: agent-events.sh transition [-h] --attempt ATTEMPT --state STATE
                                  [--expected-state {abandoned,blocked,cancelled,done,failed,queued,review,starting,timed_out,verifying,waiting_approval,waiting_input,working}]
                                  [--expected-seq EXPECTED_SEQ]
                                  [--ref NAME=VALUE]
                                  [--reason-code REASON_CODE]
                                  [--actor-kind ACTOR_KIND] [--actor ACTOR]
                                  [--idempotency-key IDEMPOTENCY_KEY]
                                  [--then STATE[:KEY]]

options:
  -h, --help            show this help message and exit
  --attempt ATTEMPT
  --state STATE
  --expected-state {abandoned,blocked,cancelled,done,failed,queued,review,starting,timed_out,verifying,waiting_approval,waiting_input,working}
  --expected-seq EXPECTED_SEQ
                        require this attempt sequence under the lifecycle lock
                        (first transition only)
  --ref NAME=VALUE
  --reason-code REASON_CODE
  --actor-kind ACTOR_KIND
  --actor ACTOR
  --idempotency-key IDEMPOTENCY_KEY
  --then STATE[:KEY]
"###,
            r###"usage: agent-events.sh transition [-h] --attempt ATTEMPT --state STATE
                                  [--expected-state {abandoned,blocked,cancelled,done,failed,queued,review,starting,timed_out,verifying,waiting_approval,waiting_input,working}]
                                  [--expected-seq EXPECTED_SEQ]
                                  [--ref NAME=VALUE]
                                  [--reason-code REASON_CODE]
                                  [--actor-kind ACTOR_KIND] [--actor ACTOR]
                                  [--idempotency-key IDEMPOTENCY_KEY]
                                  [--then STATE[:KEY]]
"###,
        ),
        (App::Events, "recover-verify-heartbeat") => (
            r###"usage: agent-events.sh recover-verify-heartbeat [-h] --attempt ATTEMPT
                                                --expected-sequence
                                                EXPECTED_SEQUENCE

options:
  -h, --help            show this help message and exit
  --attempt ATTEMPT
  --expected-sequence EXPECTED_SEQUENCE
"###,
            r###"usage: agent-events.sh recover-verify-heartbeat [-h] --attempt ATTEMPT
                                                --expected-sequence
                                                EXPECTED_SEQUENCE
"###,
        ),
        (App::Events, "heartbeat") => (
            r###"usage: agent-events.sh heartbeat [-h] --attempt ATTEMPT [--actor ACTOR]
                                 [--idempotency-key IDEMPOTENCY_KEY]

options:
  -h, --help            show this help message and exit
  --attempt ATTEMPT
  --actor ACTOR
  --idempotency-key IDEMPOTENCY_KEY
"###,
            r###"usage: agent-events.sh heartbeat [-h] --attempt ATTEMPT [--actor ACTOR]
                                 [--idempotency-key IDEMPOTENCY_KEY]
"###,
        ),
        (App::Events, "usage") => (
            r###"usage: agent-events.sh usage [-h] --attempt ATTEMPT [--tokens TOKENS]
                             [--duration-ms DURATION_MS]
                             [--cost-microusd COST_MICROUSD]
                             [--ref NAME=VALUE] [--actor ACTOR]
                             [--idempotency-key IDEMPOTENCY_KEY]

options:
  -h, --help            show this help message and exit
  --attempt ATTEMPT
  --tokens TOKENS
  --duration-ms DURATION_MS
  --cost-microusd COST_MICROUSD
  --ref NAME=VALUE      bounded relative reference metadata; never changes
                        authority
  --actor ACTOR
  --idempotency-key IDEMPOTENCY_KEY
"###,
            r###"usage: agent-events.sh usage [-h] --attempt ATTEMPT [--tokens TOKENS]
                             [--duration-ms DURATION_MS]
                             [--cost-microusd COST_MICROUSD]
                             [--ref NAME=VALUE] [--actor ACTOR]
                             [--idempotency-key IDEMPOTENCY_KEY]
"###,
        ),
        (App::Events, "show") => (
            r###"usage: agent-events.sh show [-h] --attempt ATTEMPT [--json] [--bundle]
                            [--transcript PATH]

options:
  -h, --help         show this help message and exit
  --attempt ATTEMPT
  --json
  --bundle           read-only post-mortem: lifecycle events, artifact-index
                     rows, fail-ledger rows and the tool-call timeline, as
                     short facts with their source files
  --transcript PATH  with --bundle: Claude Code JSONL transcript when none is
                     recorded for the attempt
"###,
            r###"usage: agent-events.sh show [-h] --attempt ATTEMPT [--json] [--bundle]
                            [--transcript PATH]
"###,
        ),
        (App::Events, "list") => (
            r###"usage: agent-events.sh list [-h] [--active]
                            [--state {abandoned,blocked,cancelled,done,failed,queued,review,starting,timed_out,verifying,waiting_approval,waiting_input,working}]
                            [--limit LIMIT] [--json]

options:
  -h, --help            show this help message and exit
  --active
  --state {abandoned,blocked,cancelled,done,failed,queued,review,starting,timed_out,verifying,waiting_approval,waiting_input,working}
  --limit LIMIT
  --json
"###,
            r###"usage: agent-events.sh list [-h] [--active]
                            [--state {abandoned,blocked,cancelled,done,failed,queued,review,starting,timed_out,verifying,waiting_approval,waiting_input,working}]
                            [--limit LIMIT] [--json]
"###,
        ),
        (App::Events, "resume") => (
            r###"usage: agent-events.sh resume [-h] --attempt ATTEMPT

options:
  -h, --help         show this help message and exit
  --attempt ATTEMPT
"###,
            r###"usage: agent-events.sh resume [-h] --attempt ATTEMPT
"###,
        ),
        (App::Events, "follow") => (
            r###"usage: agent-events.sh follow [-h] [--after-event AFTER_EVENT] [--limit LIMIT]
                              [--timeout TIMEOUT] [--interval INTERVAL]

options:
  -h, --help            show this help message and exit
  --after-event AFTER_EVENT
  --limit LIMIT
  --timeout TIMEOUT
  --interval INTERVAL
"###,
            r###"usage: agent-events.sh follow [-h] [--after-event AFTER_EVENT] [--limit LIMIT]
                              [--timeout TIMEOUT] [--interval INTERVAL]
"###,
        ),
        (App::Events, "reconcile") => (
            r###"usage: agent-events.sh reconcile [-h] [--stale-seconds STALE_SECONDS]
                                 [--apply] [--json]

options:
  -h, --help            show this help message and exit
  --stale-seconds STALE_SECONDS
  --apply
  --json
"###,
            r###"usage: agent-events.sh reconcile [-h] [--stale-seconds STALE_SECONDS]
                                 [--apply] [--json]
"###,
        ),
        (App::Events, "validate") => (
            r###"usage: agent-events.sh validate [-h]

options:
  -h, --help  show this help message and exit
"###,
            r###"usage: agent-events.sh validate [-h]
"###,
        ),
        (App::Events, "compact") => (
            r###"usage: agent-events.sh compact [-h] [--days DAYS] [--apply]

options:
  -h, --help   show this help message and exit
  --days DAYS
  --apply
"###,
            r###"usage: agent-events.sh compact [-h] [--days DAYS] [--apply]
"###,
        ),
        (App::Approvals, "") => (
            r###"usage: approval-inbox.sh [-h] [--repo REPO]
                         {request,decide,begin-consume,finish-consume,expire,reconcile,show,list,validate,path}
                         ...

Durable compare-and-set approvals outside worker-writable state.

positional arguments:
  {request,decide,begin-consume,finish-consume,expire,reconcile,show,list,validate,path}
    request             request one bounded privileged effect
    decide              approve or deny with version CAS
    begin-consume       reserve a one-time grant before the effect
    finish-consume      terminalize a reserved effect
    expire              find or append expired unreserved approvals
    reconcile           find stale consuming approvals without guessing their
                        outcome
    show                show one approval projection
    list                list approvals
    validate            validate approval state and permissions
    path                print this repository's private approval store

options:
  -h, --help            show this help message and exit
  --repo REPO
"###,
            r###"usage: approval-inbox.sh [-h] [--repo REPO]
                         {request,decide,begin-consume,finish-consume,expire,reconcile,show,list,validate,path}
                         ...
"###,
        ),
        (App::Approvals, "request") => (
            r###"usage: approval-inbox.sh request [-h] [--attempt ATTEMPT] --action ACTION
                                 --object-id OBJECT_ID --summary SUMMARY
                                 [--expires-in EXPIRES_IN]
                                 [--base-sha BASE_SHA] [--patch-sha PATCH_SHA]
                                 [--task-id TASK_ID] [--lease-id LEASE_ID]
                                 [--profile PROFILE]
                                 [--parameters-json PARAMETERS_JSON]

options:
  -h, --help            show this help message and exit
  --attempt ATTEMPT
  --action ACTION
  --object-id OBJECT_ID
  --summary SUMMARY
  --expires-in EXPIRES_IN
  --base-sha BASE_SHA
  --patch-sha PATCH_SHA
  --task-id TASK_ID
  --lease-id LEASE_ID
  --profile PROFILE
  --parameters-json PARAMETERS_JSON
"###,
            r###"usage: approval-inbox.sh request [-h] [--attempt ATTEMPT] --action ACTION
                                 --object-id OBJECT_ID --summary SUMMARY
                                 [--expires-in EXPIRES_IN]
                                 [--base-sha BASE_SHA] [--patch-sha PATCH_SHA]
                                 [--task-id TASK_ID] [--lease-id LEASE_ID]
                                 [--profile PROFILE]
                                 [--parameters-json PARAMETERS_JSON]
"###,
        ),
        (App::Approvals, "decide") => (
            r###"usage: approval-inbox.sh decide [-h] --approval APPROVAL --decision
                                {approve,deny,reject} --expected-version
                                EXPECTED_VERSION --actor ACTOR
                                [--changes-json CHANGES_JSON]

options:
  -h, --help            show this help message and exit
  --approval APPROVAL
  --decision {approve,deny,reject}
  --expected-version EXPECTED_VERSION
  --actor ACTOR
  --changes-json CHANGES_JSON
"###,
            r###"usage: approval-inbox.sh decide [-h] --approval APPROVAL --decision
                                {approve,deny,reject} --expected-version
                                EXPECTED_VERSION --actor ACTOR
                                [--changes-json CHANGES_JSON]
"###,
        ),
        (App::Approvals, "begin-consume") => (
            r###"usage: approval-inbox.sh begin-consume [-h] --approval APPROVAL --token GRANT
                                       --expected-version EXPECTED_VERSION
                                       --consumer CONSUMER

options:
  -h, --help            show this help message and exit
  --approval APPROVAL
  --token GRANT
  --expected-version EXPECTED_VERSION
  --consumer CONSUMER
"###,
            r###"usage: approval-inbox.sh begin-consume [-h] --approval APPROVAL --token GRANT
                                       --expected-version EXPECTED_VERSION
                                       --consumer CONSUMER
"###,
        ),
        (App::Approvals, "finish-consume") => (
            r###"usage: approval-inbox.sh finish-consume [-h] --approval APPROVAL
                                        --expected-version EXPECTED_VERSION
                                        --result {consumed,failed} --consumer
                                        CONSUMER

options:
  -h, --help            show this help message and exit
  --approval APPROVAL
  --expected-version EXPECTED_VERSION
  --result {consumed,failed}
  --consumer CONSUMER
"###,
            r###"usage: approval-inbox.sh finish-consume [-h] --approval APPROVAL
                                        --expected-version EXPECTED_VERSION
                                        --result {consumed,failed} --consumer
                                        CONSUMER
"###,
        ),
        (App::Approvals, "expire") => (
            r###"usage: approval-inbox.sh expire [-h] [--apply] [--json]

options:
  -h, --help  show this help message and exit
  --apply     append terminal expiry rows (default: report only)
  --json
"###,
            r###"usage: approval-inbox.sh expire [-h] [--apply] [--json]
"###,
        ),
        (App::Approvals, "reconcile") => (
            r###"usage: approval-inbox.sh reconcile [-h]
                                   [--older-than-seconds OLDER_THAN_SECONDS]
                                   [--apply] [--json]

options:
  -h, --help            show this help message and exit
  --older-than-seconds OLDER_THAN_SECONDS
                        minimum reservation age, 1-604800 seconds (default:
                        300)
  --apply               append interrupted/unknown rows (default: report only)
  --json
"###,
            r###"usage: approval-inbox.sh reconcile [-h]
                                   [--older-than-seconds OLDER_THAN_SECONDS]
                                   [--apply] [--json]
"###,
        ),
        (App::Approvals, "show") => (
            r###"usage: approval-inbox.sh show [-h] --approval APPROVAL [--json]

options:
  -h, --help           show this help message and exit
  --approval APPROVAL
  --json
"###,
            r###"usage: approval-inbox.sh show [-h] --approval APPROVAL [--json]
"###,
        ),
        (App::Approvals, "list") => (
            r###"usage: approval-inbox.sh list [-h] [--pending]
                              [--state {approved,consumed,consuming,denied,expired,failed,interrupted,requested}]
                              [--effective-state {approved,consumed,consuming,denied,expired,failed,interrupted,requested}]
                              [--json]

options:
  -h, --help            show this help message and exit
  --pending
  --state {approved,consumed,consuming,denied,expired,failed,interrupted,requested}
  --effective-state {approved,consumed,consuming,denied,expired,failed,interrupted,requested}
  --json
"###,
            r###"usage: approval-inbox.sh list [-h] [--pending]
                              [--state {approved,consumed,consuming,denied,expired,failed,interrupted,requested}]
                              [--effective-state {approved,consumed,consuming,denied,expired,failed,interrupted,requested}]
                              [--json]
"###,
        ),
        (App::Approvals, "validate") => (
            r###"usage: approval-inbox.sh validate [-h]

options:
  -h, --help  show this help message and exit
"###,
            r###"usage: approval-inbox.sh validate [-h]
"###,
        ),
        (App::Approvals, "path") => (
            r###"usage: approval-inbox.sh path [-h]

options:
  -h, --help  show this help message and exit
"###,
            r###"usage: approval-inbox.sh path [-h]
"###,
        ),
        _ => text(app, ""),
    }
}
