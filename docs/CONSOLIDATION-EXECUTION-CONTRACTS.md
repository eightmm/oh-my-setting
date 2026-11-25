# Execution 통합 계약: 항목 8–10

**상태: DESIGN / NOT IMPLEMENTED.** 아래 API와 migration slice는 후속 구현 제안이다.
소스 기준은 `4c7db603ecf33952598406532c9213cf3aa18293` (2026-11-25)이며 링크의 줄 번호도
그 revision을 따른다. 원래 감사 번호 8, 9, 10을 유지한다. 이 문서는 runtime 코드를 바꾸지 않는다.

항목 11의 borrowed cancellation은 별도 구현 범위다. 항목 12의 managed Lead → Plan
연결은 열린 [#522](https://github.com/eightmm/oh-my-setting/pull/522)에서 이어간다.
그 PR을 native Lead custody와 같은 구현으로 설명하거나 여기서 다시 구현하지 않는다.
기존 [CC-06](CALLABLE-CONSOLIDATION-WORK-PACKETS.md#cc-06--공통-supervisor로-실행기-정리)과
[신뢰성 패킷](ORCHESTRATOR-RELIABILITY-WORK-PACKETS.md)의 caller별 이전을 구체화한다.

## 8. Exact Git commit의 공통 kernel

**현재와 중복.** [Goal `commit_intent`](../crates/oms-command-autonomy/src/goal.rs#L520)와
[Panel `exact_commit`](../crates/oms-panel-core/src/acceptance.rs#L396)는 Git index 보존,
expected tree 확인, HEAD/ref 확인, commit 생성과 ref 갱신을 각각 구현한다.
두 경로의 업무 transaction은 같지 않다. Goal은 landing으로 Plan task가 완료된 뒤
commit intent를 수행하고, Panel acceptance는 commit과 acceptance를 만든 뒤 Plan을 완료한다.
이미 공유하는 것은 Plan/landing producer와 host의 bounded Git 실행이며 완료 transaction 전체가 아니다.

**선정 owner/API.** 양쪽이 의존하는 기존 `oms-command-plan`의 repository 계층에
좁은 Git-only kernel을 둔다. 제안 입력은 expected ref/base/tree, index snapshot,
commit message, cancellation과 caller가 제공하는 최종 fence 재검증이다.
출력은 created/already-present/ref-moved/unknown과 commit/tree 증거를 구분한다.
Kernel은 Plan task, Room, acceptance, goal intent ledger를 읽거나 갱신하지 않는다.

**유지할 경계.** Scope/lease/reviewer/authority fence와 기록 순서는 각 owner에 남는다.
Commit object를 만든 사실과 ref publication 성공을 구분하고, 불완전한 ACK를 성공으로
추정하지 않는다. 기존 staged bytes, detached HEAD 처리, index lock·inode 검사,
worktree 재검증, ref CAS, interrupted recovery를 보존한다. 자동 push/land는 추가하지 않는다.

**구현 slice.**

1. `goal.rs`의 index/tree helper와 `acceptance.rs::exact_commit`의 공통 Git 규칙을
   corpus로 고정한다. 두 domain transaction 순서가 다름을 먼저 테스트한다.
2. Index snapshot/lock와 expected tree 비교 helper만 repository 계층으로 옮긴다.
   기존 caller wrapper와 error/exit 형식을 유지한다.
3. Commit/ref CAS를 공통 kernel로 연결한다. Ref 갱신 직전에 caller fence를 재검증하고,
   Goal intent 또는 Panel acceptance의 durable record는 각 caller가 기존 순서로 쓴다.
4. 두 경로의 crash/retry 동등성 후에만 중복 Git helper를 제거한다.

**테스트·합격 조건.** [autonomy contract](../crates/oms-command-autonomy/tests/autonomy_contract.rs)의
기존 Repo/Services fixture와 [Panel parity](../crates/oms-panel-core/tests/parity.rs#L1482)의
`acceptance_recovery`, 같은 파일의 `composed_acceptance`를 확장한다.
새 별도 test framework는 만들지 않는다. Staged bytes/HEAD/worktree가 바뀌면 기존 index와
상대방 ref를 보존해야 한다. Commit 생성 후 ref 갱신 전, ref 갱신 후 domain ACK 전의
중단을 각각 재현해 재시도가 중복 commit이나 가짜 Plan 완료를 만들지 않아야 한다.
Goal/Panel의 서로 다른 완료 순서도 그대로 증명해야 한다.

**의존성.** 독립적으로 pure helper부터 진행한다. #522의 PlanRun 연결이나 다른 acceptance
변경과 같은 파일을 수정할 때는 정확한 head를 먼저 대조한다. 항목 9 이전을 기다리며
transaction 의미를 바꿀 필요는 없다.

## 9. 남은 일회성 subprocess 실행기

**현재와 중복.** [Graph `process_detail`](../crates/oms-command-graph/src/util.rs#L84),
[Task process](../crates/oms-command-task/src/process.rs#L100),
[Runs process](../crates/oms-command-runs/src/process.rs#L256),
[Lifecycle command 실행](../crates/oms-command-lifecycle/src/runtime.rs#L336)에
child 생성·pipe drain·timeout/cancel·group 종료·reap 코드가 남아 있다.
반면 [Plan Git](../crates/oms-command-plan/src/repository.rs#L88)은 이미
`oms_supervisor::process::Reactor`를 사용하고,
[recipe 주 작업](../crates/oms-command-state/src/recipe.rs#L1325)은 Runs 실행기를 사용한다.
이를 별도 미연결 엔진으로 다시 세지 않는다.

**선정 owner/API.** 기존 [Reactor/Request/Completion](../crates/oms-supervisor/src/process.rs#L258)을
재사용한다. 필요한 adapter만 추가하고 새 universal runner나 실행 ledger를 만들지 않는다.
Domain outcome은 caller가 supervisor의 exit, signal, cleanup/output completeness를 받아 기록한다.

**유지할 경계.** Task의 inherited stdin·live raw output, Runs의 query/stream/merged 모드,
bounded tail와 live PID callback, Lifecycle의 environment 제거/override, Graph의 출력 상한을
각각 보존한다. Stdout capture가 같아 보여도 입력·시그널·출력 계약이 동등하기 전에는
old loop를 제거하지 않는다. Cancel 후 다른 프로세스의 재사용 PID를 종료하면 안 된다.

**구현 slice.**

1. Graph의 stdin-null bounded query와 Lifecycle query를 각각 기존 Reactor adapter에
   연결한다. Timeout, output limit, cancellation, child cleanup 결과를 공개 exit로 변환한다.
2. Runs의 query 모드부터 연결하고 streaming/merged/on-spawn 모드를 순서대로 검증한다.
   Recipe 주 작업은 Runs consumer이므로 두 번째 executor를 만들지 않는다.
3. Task inherited stdin/live output을 지원하는 기존 supervisor 계약을 확인하고 부족한
   기능만 추가한다. 그 기능의 회귀가 통과한 뒤 Task를 이전한다.
4. Caller 단위 동등성 증거가 있는 loop만 제거한다. Long-lived Broker/companion은 항목 10이다.

**테스트·합격 조건.** [supervisor duplex tests](../crates/oms-supervisor/tests/duplex_whole_tree.rs),
[recovery tests](../crates/oms-supervisor/tests/recovery.rs),
[Graph host adapter](../crates/oms-command-graph/tests/host_adapter.rs),
[Task contract](../crates/oms-command-task/tests/task_contract.rs),
[Runs tests](../crates/oms-command-runs/src/tests.rs#L930),
[native lifecycle](../crates/oms-command-lifecycle/tests/native_lifecycle.rs)를 재사용한다.
Pipe를 붙잡는 자손, output overflow, stdin EOF/partial write, signal/timeout/cancel,
마지막 buffered chunk를 fake child로 재현한다. 취소 시 자식과 pipe가 남지 않아야 하며
incomplete output/cleanup은 성공 receipt로 기록되지 않아야 한다. Runs의
`execute_with_hooks_delivers_live_child_pid_immediately_after_spawn`도 유지한다.

**의존성.** 항목 11의 cancellation API head와 supervisor writer를 조정한다.
Plan의 이미 완료된 Reactor 이전을 다시 구현하지 않는다. 운영체제별 process-group 계약을
보존하고 Linux CI만으로 모든 host의 런타임 검증을 완료했다고 표시하지 않는다.

## 10. Long-lived Broker와 recipe companion 수명 관리

**현재와 중복.** [Broker `start`](../crates/oms-command-providers-skills/src/broker.rs#L36)는
Codex stdio process와 reader thread를 만들고, [`call`](../crates/oms-command-providers-skills/src/broker.rs#L98)은
RPC ID와 deadline을 관리하며, [`Drop`](../crates/oms-command-providers-skills/src/broker.rs#L245)은
직접 child를 종료한다. Recipe는 [`stop_child`/`CompanionGuard`](../crates/oms-command-state/src/recipe.rs#L1125)와
[직접 spawn](../crates/oms-command-state/src/recipe.rs#L1628)으로 companion group을 관리한다.
공통 부분은 소유한 process의 수명이며 RPC와 companion readiness는 서로 다른 계약이다.

**선정 owner/API.** 기존 Reactor의 job handle, duplex send/receive, close_input,
stop/poll/completion을 이용한다. Broker와 companion은 각각 작은 adapter를 가진다.
RPC framing/ID matching/authentication은 Broker에, ready-port·recipe 기록·역순 cleanup은
recipe owner에 남긴다. 하나의 긴 명령 실행 함수로 두 프로토콜을 합치지 않는다.

**유지할 경계.** Broker의 bounded frame/channel, initialization 순서, notification 처리,
RPC deadline, auth 자료의 수명과 폐기를 보존한다. Companion은 `env_clear`와 승인된
environment, process group, startup receipt와 ready-port timeout을 유지한다.
중단되거나 응답이 불명확한 RPC를 자동으로 재전송하지 않는다.

**구현 slice.**

1. Broker의 현재 frame/ID/deadline 규칙을 fake stdio 서버로 고정한다.
   `Broker`가 transport handle을 소유하도록 바꾸고 기존 `call` API를 유지한다.
2. `Broker::Drop`을 supervisor의 owned cleanup으로 연결한다. Reader backpressure·EOF와
   초기화 실패에서도 child를 회수한 뒤 기존 kill/wait 경로를 제거한다.
3. Recipe companion spawn/stop을 별도 adapter로 이전한다. Main recipe process는 이미
   Runs를 소비하므로 변경하지 않는다. Companion 기록과 readiness는 현재 owner가 수행한다.

**테스트·합격 조건.** [providers parity](../crates/oms-command-providers-skills/tests/project_parity.rs),
[recipe fixture](../crates/oms-command-state/tests/recipe_and_gc.rs#L118)의
`recipe_execute_resume_limits_companions_and_trigger_run`, 기존 supervisor duplex fixture를 확장한다.
Broker에 공개 fake-transport seam이 부족하면 기존 crate의 test module에만 추가한다.
Oversize/malformed frame, 잘못된 RPC ID, notification flood, blocked stdin, 조기 EOF,
초기화·ready-port timeout, cancellation에서 유실된 reader와 자손이 남지 않아야 한다.
Request/companion 실행 횟수는 증가하지 않으며 secret payload는 로그나 fixture에 노출하지 않는다.

**의존성.** 항목 9와 supervisor API writer를 공유하되 독립 PR로 옮긴다.
항목 5의 catalog probe는 transport 변경 전후 같은 결과 계약을 유지해야 한다.

## 검증과 완료 판정

이 문서 PR에서는 상대 링크와 source/test anchor를 확인한다. Runtime 동등성, process
cleanup, Git publication 합격 조건은 후속 코드 PR의 정확한 head에서 검증한다.
각 slice는 가까운 canonical fixture를 확장하고 root `./verify` 및 필요한 CI 결과를
별도 기록한다. 구현·실행·병합의 세 상태를 문서 채택 상태와 혼동하지 않는다.
