# Orchestrator lifecycle: 예약 반환·취소·복구 구현 패킷

기준: `f9a695ea2de90a160754252c1ee9cbaa4dd7a488` (2026-11-18).
상태: **부분 구현**. 아래 구현 현황을 제외한 LC-03 등의 항목은 후속 제안이다.
상위 계약: [Team runtime](TEAM-ORCHESTRATION-RUNTIME.md),
[validation](TEAM-ORCHESTRATION-VALIDATION.md).

## 구현 현황

Native resource front door에 `discard-write`, `retire-team`, `reconcile`이
연결되었다. `status`는 exact dispatch snapshot digest와 Team/write revision,
execution/candidate pending을 제공한다. Live Main/Room/owner-generation guard와
immutable native decision을 먼저 확보하고, dispatch writer 안에서는 이미 잡은
guard만 검사하며, writer 종료 후 full owner/base 증명을 다시 검사한다.

- 명시적 write discard는 sealed execution cleanup 뒤에만 WIP를 반환한다.
  candidate 파일과 usage receipt는 보존하며 task acceptance/completion을 만들지 않는다.
- Team withdrawal 또는 canonical successor가 확인되고 execution/WIP가 모두 끝난
  정확한 generation만 반환한다. `team-cancel` 후 자동 반환을 시도하고,
  write discard 후에도 같은 반환을 재시도한다. `pending`/`unknown`은 그대로 남는다.
- `reconcile`은 exact held/drained snapshot에서 같은 domain의 새 epoch만 시작한다.
  과거 policy/claim/decision/usage는 지우지 않는다.
- Panel acceptance는 canonical Plan review와 모든 component 등록 뒤 원본 candidate의
  operation/attempt/task와 현재 Main/host generation을 다시 검증해 WIP 반환을 시도한다.
  composition도 각 원본 operation을 독립 처리하며 sealed execution cleanup을 요구한다.
  반환 실패는 이미 저장된 acceptance를 되돌리지 않는다. `resource_retirement`의
  pending/unknown을 보존하고 `recorded` receipt의 동일 `--accept` + exact patch digest로
  정산만 재시도한다. canonical acceptance·현재 commit/tree·landing·component 증거가
  모두 일치해야 하며 patch/review를 재실행하거나 incomplete acceptance를 자동 확정하지 않는다.

공개 명령·재시도 계약은 [resource CLI](../crates/oms-runtime/README.md)에 있다.
기존 native resource fixture를 확장하여 public CLI → live Main/Room → native
decision → checked store를 검증한다. 로컬 child의 sealed cleanup, pending 거절,
candidate/unknown usage 보존, decision replay/conflict, 작은 capacity 재사용과
held epoch 재개를 포함한다.

**남은 범위:** active member cancel 전달, old Main/host-loss unknown claim 복구,
persistent session idle/close producer는
아직 연결하지 않았다. 기존 `team-cancel`의 `settled`는 계약 철회이며 실제
process cleanup 완료가 아니다. 아래 기준 source 표는 구현 전 분석 기록이다.

## 1. 해결할 실제 문제와 범위

기준 SHA의 Managed Team 경로는 실행 전에 자원을 예약하고, 확인된 process cleanup 뒤에는
invocation/process 예약을 반환한다. 그러나 Team 자체와 write candidate의 별도 예약을
반환하는 store API는 native 완료·취소·acceptance 경로에 연결되지 않았었다.
완료한 작업의 예약이 남으면 다음 팀이나 writer가 capacity에 막힐 수 있다.
이는 비용을 초과한다는 증거가 아니라, 안전하게 보류된 예약을 끝내는 경로의 공백이다.

현재 사용자가 보는 provider orchestrator pane과 managed Team은 별도 경로다.
[orchestrator.rs](../crates/oms-command-panel/src/orchestrator.rs)의 `require_main`
(307–333행)은 `panel_native_scheduling == unmanaged`를 요구하며,
`open`(487–550행)은 tmux에서 interactive provider를 시작한다.
이 pane을 등록된 Team이나 물리 실행 예약으로 소급 해석하지 않는다.
이 패킷은 기존 pane을 자동 전환하거나 native descendants의 자원 제한을 약속하지 않는다.

첫 구현 범위는 **현재 native physical producer로 생성한 managed 실행**의 lifecycle이다.
기존 저장소의 owner/Plan/Room/dispatch/usage producer를 재사용한다.
새 scheduler, daemon, 상태 DB, 외부 모델 호출, 기본값 활성화는 필요하지 않다.

## 2. 확인된 source와 현재 동작

| Source / 기준 행 | 확인한 동작 | 재사용할 경계 |
|---|---|---|
| [services_physical.rs](../crates/oms/src/services_physical.rs), `physical_contract`, 194–216 | Team slot 예약 → physical 등록 → owner/base/host fence | 실제 예약 producer와 canonical owner binding |
| [peer usage.rs](../crates/oms-command-peer/src/usage.rs), `settle_execution`, 300–327 | sealed cleanup이 있으면 invocation/process 정산 | 완료·취소 후 제한된 정산 deadline |
| [resources.rs](../crates/oms-command-telemetry/src/store/resources.rs), `count`, 491–655 | Team/invocation/process/write-WIP를 별도로 셈 | 같은 dispatch transaction의 occupancy |
| 같은 파일, `retire_team_checked`, 897–914 | 확인된 canonical decision과 revision으로 Team 예약 반환 | 남은 실행/session/WIP가 있으면 거절 |
| 같은 파일, `retire_write_checked`, 919–940 | candidate 처리 결정에 따라 WIP 반환 | process exit를 candidate 처리로 취급하지 않음 |
| 같은 파일, `idle_session_checked` / `close_session`, 1364–1483 | turn 종료와 실제 session 종료를 구분 | scoped transport decision / sealed cleanup |
| 같은 파일, `reconcile_checked`, 1067–1149 | held + 모든 예약 반환 후 새 epoch 시작 | history·usage 보존, 동일 epoch 재사용 거절 |
| [team.rs](../crates/oms-command-plan/src/team.rs), `execute`의 `TeamCancel`, 1040–1065 | 계약 phase를 `settled`로 바꿈 | 정확한 owner/Room/spec digest 검사 |
| [services_resources.rs](../crates/oms/src/services_resources.rs), `action`, 98–168 | host/status/activate/hold만 노출 | state-root 선택·host 재검증·parent guard |
| [acceptance.rs](../crates/oms-panel-core/src/acceptance.rs), `preflight`, 1–249 | 하나의 worker candidate와 review/lease/scope 검증 | candidate 처리 결정의 출처 |

기준 SHA에서 retirement/session-idle/session-close/epoch API의 호출 위치를 검색했을 때
store 정의와 tests 밖의 production consumer가 없었다.
[telemetry README](../crates/oms-command-telemetry/README.md)도 native retirement와 epoch
consumer를 별도 후속 작업으로 명시한다. 따라서 기존 API의 삭제나 재구현이 아닌 연결 작업이다.

`team-cancel`의 `settled`는 현재 계약 철회를 뜻한다. 실행 중 member의 종료 증거가 아니며,
`team-show`의 `cleanup_coverage`도 null이다. 기존 저장 record의 의미를 조용히 바꾸지 않는다.
또한 현재 physical producer들은 `persistent_session: false`를 사용하므로,
session-idle/close는 실제 persistent producer가 생긴 뒤 연결하는 조건부 후속 범위다.

## 3. 네 가지 상태를 분리한다

| 상태 축 | 시작 | 끝을 입증하는 producer | 끝났다고 추정하면 안 되는 근거 |
|---|---|---|---|
| Team 계약/admission | canonical TeamSpec | 해당 generation의 withdrawal/completion decision | 모든 member가 조용함 |
| Invocation/process | physical New + resource claim | 원래 Supervisor의 exact owned cleanup | exit code만 있음, PID가 안 보임 |
| Write candidate WIP | physical writer operation | reviewed acceptance 또는 명시적 discard/supersession decision | 작업 취소, 프로세스 종료 |
| Usage liability | 기존 usage/budget producer | 그 producer의 실제 usage 정산 규칙 | Team retirement, epoch 변경 |

표시용 `cancel_requested`, `cleanup_pending`, `cleanup_unknown`, `candidate_pending`,
`resource_retired`는 제안된 projection 명칭이며 현재 CLI/schema라고 주장하지 않는다.
새 projection은 canonical record와 정확한 claim을 join해서 만든다.
`contract_phase=settled`와 `cleanup=unknown`이 동시에 표시되는 경우를 허용한다.

## 4. LC-01 — decision guard와 예약의 읽기 경로

Native root service에 producer 간 연결을 둔다. telemetry crate가 Plan 또는 panel을
거꾸로 호출하게 만들지 않는다. 외부 JSON을 `DecisionEvidence`로 바꾸는 API도 만들지 않는다.

1. 기존 canonical Plan/Room/lifecycle writer에서 정확한 owner generation, spec,
   withdrawal 또는 candidate disposition을 읽고 유지 가능한 decision guard를 얻는다.
2. 기존 physical/dispatch reader로 대상의 정확한 binding, operation/call, custody revision을
   조회한다. 표시 이름·현재 PID·pane tag로 binding을 재구성하지 않는다.
3. 현재 `AdmissionGuard`를 취소 후 그대로 사용하면 안 된다. 이 guard는 새 admission의
   유효성·현재 base·미만료·비철회 상태를 요구한다. 종료 정산에는 별도의 bounded
   decision guard가 필요하며, 기존 producer의 잠금·owner 증명을 재사용한다.
4. guard는 이미 내려진 종료 결정을 인증한다. 만료된 Team에 새 실행 권한을 부여하지 않는다.
5. callback은 dispatch writer 내부에서 보유 guard와 고정된 증거만 검사한다.
   callback에서 Plan lock을 새로 잡거나 Git/native proof subprocess를 실행하지 않는다.
6. 외부 producer locks → dispatch → 필요한 usage writer 순서를 실제 caller와 대조한다.
   모든 acquisition은 deadline을 공유하며 dispatch 종료 뒤 outer authority를 다시 확인한다.

현재 `DecisionEvidence`는 `.oms` 내부 regular source의 digest/revision을 확인하지만,
그 파일이 실제 완료 결정인지 판별하는 역할은 adapter에 있다.
단순 파일 digest 일치나 `done: true`를 producer decision으로 인정하지 않는다.
새 receipt가 필요하면 기존 immutable artifact와 canonical Plan reference를 확장하고,
version barrier·구버전 reader·replay 규칙을 같은 PR에 적는다.

## 5. LC-02 — candidate WIP와 Team 예약 반환 연결

### Write candidate

확인된 acceptance/landing, 명시적 discard, 또는 검증된 supersession을 각각 처리한다.
어떤 producer가 그 결정을 소유하는지 먼저 고정하고 `retire_write_checked`로 연결한다.
`panel accept` 성공만으로 push/publish까지 완료된 것으로 표현하지 않는다.
후보를 생성하지 못한 writer도 실제 no-candidate 종료 결정이 필요하다.

반환 전에 같은 operation의 invocation/process custody가 모두 끝났는지 store가 재검증한다.
취소된 worker의 dirty worktree·partial patch·실패 로그·usage receipt는 보존한다.
WIP 반환 결정에 artifact 보존 위치와 discard/supersession 근거를 연결한다.
반환된 capacity가 artifact 삭제를 뜻하지 않는다.

### Team

계약 withdrawal/completion과 member execution/candidate 정리가 모두 확인되면
`retire_team_checked`를 호출한다. 한 member의 완료 callback에서 Team slot을 즉시 지우지 않는다.
동일 Team의 다른 member가 실행 중이거나 미처리 candidate가 있으면 그대로 pending이다.

`reserve_team` 이후 physical 등록 이전 실패도 처리해야 한다.
slot만 있고 call이 없는 상태를 읽어도 자동 반환하지 않는다. Parent가 동일 generation의
출시 철회/완료 결정을 기록하고 현재 claim 부재를 재검증한 뒤 반환한다.
동시 member launch와 경쟁하면 동일 producer/dispatch guard에 따라 한쪽이 재검증한다.

반환 성공 후 동일 decision/revision으로 재시도하면 기존 결과를 돌려준다.
같은 대상에 다른 decision digest를 넣으면 충돌을 보고하고 원래 evidence를 보존한다.
새 generation의 slot을 과거 generation 종료 작업으로 반환하지 않는다.

## 6. LC-03 — active cancel과 cleanup을 연결

기존 `team-cancel`은 새 admission을 막는 계약 철회로 보존한다.
실제 member 중단은 그 철회를 소비하는 parent-side 실행 제어 단계로 추가한다.
공개 spelling은 기존 panel/control front door에 맞춰 정하며, 임의의 PID kill API로 만들지 않는다.

순서는 다음과 같다.

1. 정확한 Team/spec/owner generation에 취소 의도를 canonical writer로 기록한다.
2. 새 launch/fallback/continue를 차단한다. 이미 시작한 call 목록은 정확한 physical identity로 조회한다.
3. 현재 owning Supervisor/transport가 제어 가능한 call에만 cancel을 전달한다.
4. bounded grace와 기존 escalation/reap을 사용하고 종료를 실제로 관측한다.
5. sealed cleanup으로 execution 예약만 반환한다. 미확인 descendants는 unknown으로 남긴다.
6. write candidate 처리는 LC-02의 별도 canonical decision을 기다린다.
7. 독립 custody가 모두 끝난 generation만 Team 예약을 반환한다.

실행 중 Reactor가 이미 사라졌다면 JSON receipt를 sealed proof로 재구성하지 않는다.
기존 native liveness 관측과 Supervisor ownership 증거를 대조하되, 새 reconciliation proof
producer가 필요하면 별도 구현으로 둔다. 부재 PID나 다른 boot만으로 자의적 kill/refund를 하지 않는다.
old Main의 취소 요청을 new Main의 새 실행에 전달하지 않는다.
중단 요청 수락과 종료 확인은 서로 다른 receipt다.

### 대표 경쟁 조건

| 경쟁 | 요구 동작 |
|---|---|
| 완료와 cancel 동시 발생 | 같은 call의 terminal 증거와 revision으로 한 번 정산 |
| cancel 직후 새로운 dispatch | canonical withdrawal fence에서 거절 |
| reservation 뒤 crash, launch 여부 불명 | intent·usage·capacity를 유지하고 확인 필요로 표시 |
| 반환 CAS 성공 후 outer guard 실패 | publication-uncertain 반환; exact receipt 조회 후 재개 |
| owner 교체·spec 변경 | 이전 generation에만 조치; 새 owner 결정 없이 새 실행 없음 |
| Team 종료 시 dirty candidate 존재 | process는 종료 가능, WIP와 Team 반환은 pending |
| slot만 존재하고 member 없음 | canonical withdrawal decision + claims 재검증 후 반환 |
| unknown cleanup | 성공/0건으로 치환하지 않고 예약 유지 |

## 7. LC-04 — held domain과 epoch의 재개

현재 `hold` 뒤 같은 policy의 `activate`는 성공적인 재개 경로가 아니다.
`activate`는 이미 존재하는 held epoch를 거절한다. 이를 reset으로 우회하지 않는다.
LC-01/02 뒤에 parent resource action을 확장하여 `reconcile_checked`를 연결한다.

입력은 새 policy 자체와 검토된 현 상태의 exact snapshot identity이다.
Native adapter는 실제 host generation, 동일 domain, 새 epoch, owner/operator decision을 검증한다.
store가 held 상태와 Team/invocation/process/write-WIP 네 카운터의 0을 재검증한다.
기존 policy/intent/claim/decision/usage history는 그대로 보존한다.

이 API는 **이미 반환된 자원의 새 epoch 시작**을 처리한다.
호스트 재부팅 뒤 남은 unknown claim을 강제로 지우는 복구 명령이 아니다.
관측할 수 없는 이전 실행을 정리하려면 별도 증명 producer와 검증 조건이 필요하며,
그것이 없으면 해당 claim의 recovery를 unsupported/held로 남긴다.
알 수 없는 비용은 epoch가 바뀌어도 unknown liability로 유지한다.

## 8. 필수 fixture와 완료 조건

기존 [usage tests](../crates/oms-command-telemetry/tests/usage.rs)의 store retirement,
session, epoch fixtures와 [native root tests](../crates/oms/src/services_control.rs)를 확장한다.
새로운 store-only 통과를 public native 연결 완료로 보고하지 않는다.

| Fixture | 완료 조건 |
|---|---|
| 작은 capacity에서 Team A 종료 후 Team B 실행 | 정상 종료·처리된 후보 뒤 예약 반환; 새 Team이 같은 domain에서 시작 |
| writer 완료 후 accept/discard | 후보 evidence 유지, WIP만 한 번 반환, 중복 callback 무해 |
| 취소된 Team의 살아 있는 child | admission 거절 즉시 확인, cleanup 전 반환 없음 |
| 두 member 중 한 명 종료 | Team slot 유지; 나머지 종료 및 후보 처리 뒤 반환 |
| claim 없는 Team slot | 명시적 철회 결정이 있을 때만 반환 |
| stale owner/spec/revision/decision | mutation 거절, 기존 bytes와 artifacts 보존 |
| completion/cancel 및 retirement/launch 경쟁 | capacity 초과·이중 반환·새 실행에 대한 잘못된 kill 없음 |
| CAS 전후 disk/lock 실패 | 성공으로 숨기지 않음; exact receipt로 재개 가능 |
| held domain → 새 epoch | 네 occupancy가 0일 때만 허용, usage/history 불변 |
| unmanaged provider pane | 기존 표시/동작 유지; managed 예약 또는 cleanup 성공으로 오인하지 않음 |

Fake local child·임시 Git repository·controlled clocks를 쓴다. 유료 provider, GPU,
실제 원격 서버, CI matrix 확장은 필요하지 않다. 기존 AGENTS.md의 affected/critical gate를 따른다.
완료 보고에는 **public entrypoint → canonical decision → store mutation → 재시도**의
실제 실행 증거, 실행 명령/exit, 미실행 검사와 이유를 함께 적는다.

## 9. 작업 순서·의존성·rollback

LC-01 decision guard/reader → LC-02 정상 완료 정산 → LC-03 active cancel → LC-04 epoch 순서다.
Persistent session idle/close와 host-loss proof는 실제 producer가 준비될 때 별도 후속으로 둔다.
LC-02를 마치기 전에 capacity 기본값을 넓히거나 managed mode를 기본 활성화하지 않는다.
Team request dispatch, 복수 candidate 통합, provider capability discovery와는 별도 PR로 검토한다.

Rollback은 새 admission부터 보류하고 기존 cleanup·retirement·usage 정산을 계속한다.
새 receipt schema를 구버전이 읽을 수 없으면 먼저 drain하고 compatible reader를 확보한다.
정산 불명 registry를 삭제하거나 `.oms`를 손으로 고치거나 빈 epoch로 재생성하지 않는다.
현재 unmanaged pane은 이 migration의 소유 대상에 포함하지 않는다.

검토 범위: 위 native resource producer, resource store, Team cancel/admission/view,
peer settlement 및 panel acceptance/orchestrator 경계의 source를 확인했다.
store source와 native resource action은 읽었으나 전체 provider·모든 OS/live 실행 검증은 하지 않았다.
미구현 항목은 분석·구현 지시이며, 미실행 runtime tests나 실제 복구 성공을 주장하지 않는다.
