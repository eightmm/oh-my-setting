# Context·evidence 통합 계약: 항목 13–16, 18–20

**상태: DESIGN / NOT IMPLEMENTED.** 소스 기준은
`4c7db603ecf33952598406532c9213cf3aa18293` (2026-11-25)이며 링크의 줄 번호도 이를 따른다.
새 API 이름은 제안이고 runtime 변경·검증 완료를 뜻하지 않는다. 원래 1–27 번호를 유지한다.
항목 1의 artifact-resolution projector와 항목 17의 statistics 공유는 별도 구현 범위다.
이 문서는 그 변경을 복제하지 않으며 새 evidence DB나 통합 store를 만들지 않는다.

기존 [공유 projection 지침](../custom-skills/oms-agent-harness/references/shared-projections.md),
[CC 패킷](CALLABLE-CONSOLIDATION-WORK-PACKETS.md),
[완료된 연결 목록](COMPONENTS.md#consolidated-paths)을 전제로 남은 consumer를 연결한다.
Projection은 현재 사실의 읽기 모델이며 lease·completion·삭제 권한을 대신하지 않는다.

## 13. Graph context pack 검증과 Peer orientation

**현재와 중복.** [Graph `validate_context_pack`](../crates/oms-command-graph/src/project_context.rs#L722)과
[Peer `orientation`](../crates/oms-command-peer/src/context_pack.rs#L65)이 bounded file read,
JSON·경로·list·digest 형식·outbound content를 각각 검사한다. Peer는 strict JSON을 거친다.
Graph context 선택과 runtime compiler 연결은 이미 존재하며
[`compile_bundle_with_host` 호출](../crates/oms-command-graph/src/project_context.rs#L710)을 다시 만들 필요가 없다.

**Owner/API·경계.** 기존 `oms-context`에 순수 `ValidatedOrientationPack` 계약을 두고
file acquisition은 `oms-safe-fs` 경계를 유지한다. Graph는 검증된 pack을 반환하고 Peer는
그 값으로 orientation과 target 목록을 렌더링한다. Pack의 reference data는 write scope가
아니다. `pack_digest`의 형식 검사를 실제 source freshness·lease·내용 무결성 증명으로
승격하지 않는다. 기존 compiler와 private worker의 state/execution root 구분을 유지한다.

**구현 slice.**

1. 두 validator의 corpus를 대조한다. Duplicate JSON key, malformed UTF-8, private/absolute
   path, unknown list type, 200-entry cap과 262144-byte cap 차이를 먼저 명시한다.
2. 기존 strict JSON 기반으로 순수 shape/path 검증을 공유한다. Graph와 Peer의 bounded
   acquisition·secret scan을 우회하지 않고 각 wrapper에서 검증된 값만 전달한다.
3. Peer rendering의 40/20 등 표시 cap과 실제 target 추출을 분리해 유지한 뒤 중복
   recursive inspection을 제거한다. Graph 재생성이나 provider 호출은 추가하지 않는다.

**테스트·합격 조건.** [Graph pack tests](../crates/oms-command-graph/src/project_context.rs#L924)의
`pack_accepts_and_refuses`, [Peer dry-run](../crates/oms-command-peer/tests/peer_contract.rs#L2684),
[context binding test](../crates/oms-command-peer/tests/peer_contract.rs#L4603)를 확장한다.
동일한 pack은 공통 검증 결과를 내며 잘못된 입력은 provider 전송 전에 거절해야 한다.
Symlink·변경 중인 파일·절대 경로·private state가 통과하지 않고, 표시에서 생략된 target이나
source hash를 새 권한으로 사용하지 않아야 한다.

**의존성.** 기존 compiler API를 사용한다. 항목 14와 공유 crate writer를 순차 반영하되
pack schema 변경과 current-source 관측은 별도 계약으로 검토한다.

## 14. Current-source 검증 집계

**현재와 중복.** [Root `source_status`](../crates/oms/src/main.rs#L684)와
[runtime context의 `verify-current`](../crates/oms-runtime/src/context.rs#L646)가 selected sources를
읽고 hash를 비교해 unchanged/changed/missing/unreadable와 bounded path 목록을 각각 만든다.
Manifest inspect/compare 자체는 이미 `oms-context`를 공유한다.

**Owner/API·경계.** 기존 `oms-context-source`에 current-source 관측 함수를 두고,
검증된 selected path/hash와 caller의 root resolver·cancellation을 입력받는다.
결과는 전체 counts와 제한된 paths/omitted를 반환한다. Root의 raw path 취급과 runtime의
source-root 선택을 lossy 문자열 변환으로 합치지 않는다. Bundle을 다시 읽거나 token/billing
절감, provider 전체 context의 동일성을 추정하지 않는다.

**구현 slice.**

1. 현재 두 출력의 counts·정렬·missing/unreadable 구분·표시 limit를 fixture로 고정한다.
2. Safe source read와 비교 집계를 `oms-context-source`에 옮기고 Root `source_status`를
   호환 wrapper로 연결한다.
3. Runtime `verify-current`를 연결한 뒤 중복 집계만 제거한다. Manifest selection과
   caller별 repository/root resolution은 현재 owner에 남긴다.

**테스트·합격 조건.** [context manifest tests](../crates/oms/tests/context_manifest_contract.rs#L409)의
`compare_current_text_and_denials`, 같은 파일의 `sources_beyond_display_limit_and_path_safety`,
[acquisition contract](../crates/oms-context-source/tests/acquisition_contract.rs)를 확장한다.
Limit 밖 source 변화도 total에 포함하고, unreadable을 missing으로 바꾸지 않아야 한다.
Cancellation·symlink·source 교체에서 false unchanged를 내지 않으며 읽기가 파일을 쓰지 않아야 한다.

**의존성.** [CC-08](CALLABLE-CONSOLIDATION-WORK-PACKETS.md#cc-08--current-source-verification-집계-공유)의
기존 owner를 따른다. 항목 13이나 1이 완료되어야만 시작할 작업은 아니다.

## 15. AGY handoff history acquisition

**현재와 중복.** [Hook `antigravity`](../crates/oms-command-hooks/src/handoff.rs#L329)와
[CLI `antigravity_handoff`](../crates/oms-command-peer/src/session.rs#L1247)가 같은 history에서
workspace/conversation과 최근 user turn을 고른다. Hook은 advisory task snapshot을 덧붙이고,
CLI는 `source_kind: prompts-only`로 기록한다. Assistant output은 두 경로 모두 해당 history에서
얻지 못한다고 표시한다. 기존 generic session/context acquisition을 재사용할 여지가 있다.

**Owner/API·경계.** 기존 `oms-context-source`의 acquisition 계층에 AGY prompt-history
selection을 추가한다. 제안 결과는 session ID, 선택된 user prompts, 읽기 상태/생략 정보다.
Hook과 CLI의 출력 문서·min-user-turns 정책·missing-session 정책·note·advisory snapshot은
각 caller에 둔다. Prompts-only 기록을 assistant summary나 검증 evidence로 바꾸지 않는다.

**구현 slice.**

1. Workspace/session 선택, session 생략 시 마지막 matching conversation, row/file cap,
   malformed row와 sensitive text의 현재 차이를 corpus로 고정한다.
2. Bounded safe acquisition/selection을 공유하고 CLI를 먼저 연결한다. Hook은 같은 selected
   prompts에 기존 advisory snapshot을 덧붙이도록 연결한다.
3. 중복 history parsing을 제거한다. Handoff storage, redaction, owner-resume guard와
   CLI의 explicit output path 계약은 변경하지 않는다.

**테스트·합격 조건.** [Peer session handoff](../crates/oms-command-peer/tests/peer_contract.rs#L6412)의
`session_handoff_prompt_projects_bounded_native_reads`,
[Hook host adapter](../crates/oms-command-hooks/tests/host_adapter.rs#L216), 기존
[transcript acquisition fixture](../crates/oms-context-source/tests/acquisition_contract.rs)를 확장한다.
같은 source와 selection이면 같은 user turns를 고르고 다른 workspace/session을 섞지 않아야 한다.
빈 history, 부분 마지막 row, 너무 큰 row, 취소, sensitive text에서 기존 skip/error 정책을
보존한다. Provider 호출이나 assistant 내용 생성은 없어야 한다.

**의존성.** Main resume/identity의 열린 [#525](https://github.com/eightmm/oh-my-setting/pull/525)와
session 파일 writer를 조정한다. Resume 권한 변경은 이 acquisition 통합 범위 밖이다.

## 16. Slurm 상태 normalization과 terminal 분류

**현재와 중복.** [Slurm submit `classify`](../crates/oms-command-slurm-submit/src/reconcile.rs#L122)는
첫 단어·`+`·대문자를 정규화하고 `REVOKED`를 cancelled로 분류한다.
[Runs terminal 목록](../crates/oms-command-runs/src/reconcile.rs#L16)과
[accounting row 처리](../crates/oms-command-runs/src/reconcile.rs#L179)는 다른 규칙을 갖는다.
Runs의 parsed observation은 이미 digest rendering에 공유된다. Submission ledger와 run ledger는
서로 다른 소유권 기록이므로 합치지 않는다.

**Owner/API·경계.** 기존 Runs reconcile 계층에 순수 scheduler-state normalization과
known/active/terminal-kind 결과를 둔다. Submit adapter가 이를 소비한다.
Raw state와 exit evidence는 남기며 normalized terminal이라는 이유만으로 success를 확정하지 않는다.
특히 현재 Submit의 `COMPLETED`+exit 부재 처리와 Runs의 결과 정책을 normalization 변경에
끼워서 바꾸지 않는다. 그 차이는 fixture와 명시적 호환 결정이 필요하다.

**구현 slice.**

1. `CANCELLED by …`, `CANCELLED+`, mixed case, `REVOKED`, requeue, unknown,
   `COMPLETED`의 exit 유무/값을 공유 corpus로 고정한다.
2. Pure normalization/terminal-kind만 Runs에 모으고 Submit/Runs 양쪽 parser를 연결한다.
   Job/array/component identity와 query 실패 판단은 각 reconciler에 남긴다.
3. 기존 ledger transition·idempotent receipt를 검증한 뒤 중복 state 목록을 제거한다.

**테스트·합격 조건.** [Submit reconcile tests](../crates/oms-command-slurm-submit/tests/reconcile_contract.rs#L255)의
`terminal_states_never_map_to_success_and_lag_or_ambiguity_is_unknown`,
`terminal_reconcile_is_recorded_once_and_provenance_is_kept`,
[Runs tests](../crates/oms-command-runs/src/tests.rs#L570)의 reconcile/requeue fixture를 확장한다.
알 수 없는 상태·실패한 query·불완전한 accounting은 완료 성공으로 바뀌지 않아야 한다.
같은 terminal observation을 두 번 읽어도 각 ledger의 receipt는 한 번만 생성되어야 한다.

**의존성.** `oms-command-slurm-submit` → 기존 Runs 계약의 의존 방향을 확인하고 작은
순수 API만 노출한다. 실제 Slurm 제출은 테스트하지 않고 기존 fake scheduler를 사용한다.

## 18. Job metric summary의 중복·미사용 public API

**현재와 중복.** [State `job_metrics::gather`](../crates/oms-command-state/src/job_metrics.rs#L84)와
[Job digest `Series`](../crates/oms-command-job-digest/src/lib.rs#L443),
[`metrics`](../crates/oms-command-job-digest/src/lib.rs#L1306)가 summary를 각각 계산한다.
State module은 [public export](../crates/oms-command-state/src/lib.rs#L15)이지만 기준 revision의
in-tree production caller는 확인되지 않는다. Active command owner는 job-digest다.
State의 [step 정수 cast](../crates/oms-command-state/src/job_metrics.rs#L141)와 job-digest의
float step 계약은 다르므로 이름만 보고 같은 함수로 대체할 수 없다.

**Owner/API·경계.** Active metric producer는 `oms-command-job-digest`로 정한다.
먼저 외부 public API 호환성을 확인한다. 필요하면 bounded metric summary 값만 노출하고
State public 함수는 호환 adapter 또는 명시적으로 deprecate하는 순서로 처리한다.
항목 17의 평균/통계 helper 공유는 이미 별도 작업이며 이 수집·표시 계약과 다르다.

**구현 slice.**

1. State public API의 export·문서·소비자를 조사하고 제거 가능 여부를 기록한다.
   In-tree caller 부재만으로 public 함수를 즉시 삭제하지 않는다.
2. Float/integer step, nonfinite, slope, JSONL/TSV, recent-window cap을 job-digest의 기존
   corpus로 고정한다. State 호환 adapter가 필요한 경우 차이를 명시한다.
3. 합의된 adapter/폐기 경로를 적용하고 중복 `Series`/parser를 제거한다. 새 metrics command는 만들지 않는다.

**테스트·합격 조건.** [Job digest edge tests](../crates/oms-command-job-digest/tests/edge.rs#L40)의
`float_steps_keep_python_integer_range`, `nonfinite_steps_abort_metrics_gathering`,
`slope_overflow_and_signed_nonfinite_follow_python`,
[command tests](../crates/oms-command-job-digest/tests/command.rs#L17)를 확장한다.
큰 step의 saturation/truncation, signed nonfinite, 정렬·rounding·파일 cap을 보존한다.
Metric staleness를 process liveness로 해석하지 않고 API 제거/호환 여부가 검토 가능해야 한다.

**의존성.** 항목 17의 canonical statistics owner를 사용하되 metric 형식 변경은 별도 검토한다.

## 19. Archive·retention·GC의 reference 보호 목록

**현재와 중복.** [Archive `referenced_text`](../crates/oms-command-state/src/archive.rs#L144)는
live state의 basename 언급을 모아 [compact 대상](../crates/oms-command-state/src/archive.rs#L234)을 보호한다.
[Telemetry `completion_references`](../crates/oms-command-telemetry/src/store/retention.rs#L259)는
receipt identity·digest·budget을 검사하며 불완전하면 전체 보호를 반환한다.
[GC capsule 참조](../crates/oms-command-state/src/gc.rs#L734)도 claims/figures를 별도로 읽는다.
겹치는 것은 reference 수집과 completeness 표현이며 삭제 transaction 자체가 아니다.

**Owner/API·경계.** 기존 Telemetry retention의 읽기 계약을 확장해
`ReferenceInventory { references, completeness, reasons }`를 제안한다.
Source family별 parser를 둔 채 caller가 필요한 family만 요청한다. Partial/unreadable/changed
scan은 absence가 아니다. Archive의 보수적인 basename 보호도 호환 adapter로 남긴다.
GC·archive·telemetry의 lock, age, namespace, 삭제/압축 writer는 그대로 유지한다.

**구현 slice.**

1. 각 caller가 보호하는 namespace와 unknown 시 보존 규칙을 목록화한다.
   기준 main의 completion GC 보호를 회귀 없이 유지한다.
2. Completion receipt와 live reference 수집 결과에 completeness를 명시하고 Telemetry,
   Archive, GC가 순서대로 소비하게 한다. 삭제 직전의 기존 identity 재확인은 유지한다.
3. 같은 family의 중복 scan을 한 query 안에서 재사용한다. Persistent reference cache나
   전역 garbage collector를 추가하지 않고 old scan만 제거한다.

**테스트·합격 조건.** [Telemetry actions](../crates/oms-command-telemetry/tests/actions.rs#L284)의
retention fixture, [Plan completion GC](../crates/oms-command-plan/tests/completion_gc.rs),
[State GC](../crates/oms-command-state/tests/recipe_and_gc.rs#L432),
[Archive tests](../crates/oms-command-state/src/archive.rs#L713)를 확장한다.
Malformed/truncated/linked/교체 중 receipt나 scan budget 초과는 관련 파일을 보존해야 한다.
Live reference를 지우지 않고 dry-run은 쓰지 않으며, archive 저장 후 원본 제거는 기존
blob integrity와 restore 가능성을 유지해야 한다.

**의존성.** 항목 1의 artifact resolution과 이유/identity 표현을 맞추되 해결된 reference를
삭제 허가로 사용하지 않는다. 이미 병합된 GC 수정의 보호 조건을 이 통합으로 약화하지 않는다.

## 20. Hook ownership/convergence의 production owner

**현재와 중복.** [Hooks `owned_script`](../crates/oms-command-hooks/src/surfaces.rs#L86)와
[`converge`](../crates/oms-command-hooks/src/surfaces.rs#L131)는 ownership·migration 로직을 가지지만
확인된 외부 호출자는 tests다. 실제 설치는
[Lifecycle `owned_hook`](../crates/oms-command-lifecycle/src/integrations.rs#L166)과
[`hooks`](../crates/oms-command-lifecycle/src/integrations.rs#L226)가 수행한다.
따라서 두 live installer가 동시에 돌아간다는 주장은 하지 않는다. Shared surface 설정과
기존 fixture는 이미 있으나 production 계약과 test helper가 따로 유지된다.

**Owner/API·경계.** Pure ownership/migration 판정은 기존 `oms-command-hooks::surfaces`에서
한 계약으로 만들고 Lifecycle이 소비하게 한다. Install/remove/repair의 transaction,
backup·ownership receipt·capability probe는 Lifecycle owner에 남긴다.
Public helper를 없앨 경우에도 외부 호환성 확인과 wrapper 단계를 거친다.

**구현 slice.**

1. Native entry, 현재/retired wrapper, foreign hook, timeout/matcher/onFailure,
   sibling order의 corpus를 live installer fixture로 옮겨 두 구현을 대조한다.
2. 공통 pure classifier/convergence 함수를 Lifecycle `integrations`에서 호출한다.
   Live 경로 연결을 확인한 뒤 기존 test-only 기대값을 같은 producer로 검증한다.
3. 중복 ownership parser만 제거한다. Install lock 밖 capability 준비와 reversible writer를 유지한다.

**테스트·합격 조건.** [Hook surface fixture](../crates/oms-command-hooks/tests/host_adapter.rs#L370)의
`surface_rows_are_native_and_legacy_rows_migrate_to_them`,
[Lifecycle integration diagnostics](../crates/oms-command-lifecycle/tests/native_lifecycle.rs#L2848),
[install lifecycle smoke](../tests/install-lifecycle-smoke.sh)를 확장한다.
두 번째 convergence는 no-op이고 foreign rows·metadata·sibling order를 보존해야 한다.
Remove/repair는 OMS가 소유한 항목만 바꾸며, capability 미확인에서 fail-closed 지원을
가짜로 선언하지 않아야 한다. Test helper만 통과한 결과를 live wiring 완료로 표시하지 않는다.

**의존성.** [CC-05](CALLABLE-CONSOLIDATION-WORK-PACKETS.md#cc-05--hook-ownershipconvergence의-production-owner-정리)를
따른다. 열린 [#520](https://github.com/eightmm/oh-my-setting/pull/520)의 Main coordination
내용과 installer 소유권 변경을 섞지 않는다.

## 검증과 완료 판정

이 제안의 합격은 source/test anchor와 상대 링크가 유효하고 각 item의 owner·호환성·
후속 slice가 독립적으로 검토 가능한 것이다. Runtime 합격은 후속 구현에서 각 기존
fixture와 root `./verify`를 실행한 정확한 head의 결과로만 기록한다. Read-only projection,
비교 corpus, durable mutation의 성공을 서로 대신하는 증거로 사용하지 않는다.
