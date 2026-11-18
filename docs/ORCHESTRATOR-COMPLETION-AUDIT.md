# Overall Main / provider orchestrator: 구현 현황과 연결 작업

작성 기준: 2026-11-18. 감사 대상은 main
[`f9a695ea2de90a160754252c1ee9cbaa4dd7a488`](https://github.com/eightmm/oh-my-setting/commit/f9a695ea2de90a160754252c1ee9cbaa4dd7a488)이다.
이 문서는 현재 소스·PR의 연결 상태와 후속 구현 범위를 정리한다.
설계 채택, fixture 통과 기록, 실제 provider 호환성, 운영 활성화를 구분한다.
문서 추가 자체로 모델 호출, 기본 설정 변경, 설치 또는 자동 병합을 수행하지 않는다.

## 1. 결론과 조사 범위

Rust 전환으로 주요 구성요소가 사라졌다기보다, **기존 구성요소와 새 orchestrator 사이의
연결이 서로 다른 단계에 있다.** Native interactive provider session은 실제로 열 수 있지만,
그 세션의 native 자식 실행이 canonical Team의 task·usage·acceptance 계약에 연결되지는 않는다.
새 engine을 만드는 대신 이 경계를 드러내고 기존 plan/peer/supervisor/landing을 연결한다.

- 최신 100개 PR을 GitHub REST의 `state=all`, `sort=updated`로 조회했다.
- 조회 당시 open 0개, merged 34개, closed without merge 66개였다.
- 최신 main 변경은 [#257](https://github.com/eightmm/oh-my-setting/pull/257)의 native panel 기본 진입 수정이다.
- 최근 source와 관련 PR body를 대조했다. 모든 historical 기능의 동등성 전수 증명은 아니다.
- 이 감사에서 live vendor, 유료 모델, SSH/GPU 실행 또는 설치 검증을 수행하지 않았다.
- PR 본문의 과거 검증 기록은 해당 PR의 evidence이며 이번 감사의 재실행 결과가 아니다.

GitHub 상태만으로 이식 누락을 판정하지 않는다. 예를 들어
[#225](https://github.com/eightmm/oh-my-setting/pull/225)는 unmerged로 닫혔지만
[multi-host 계획](MULTI-AGENT-MULTI-HOST-PLAN.md)은 #224를 통해 main에 들어왔다.
반대로 #239는 병합됐어도 [feature scope](FEATURE-SCOPE.md)의 adoption은 deferred다.

## 2. 먼저 구분해야 할 두 실행 경로

| 항목 | Native interactive orchestrator | Canonical managed Team |
|---|---|---|
| 현재 진입 | `panel --orchestrator PROVIDER --room ROOM` | `agent-plan team-register`, 기존 peer의 명시적 Team opt-in |
| 목적 | Overall Main 아래에서 기존 provider 대화를 유지하고 native 기능 사용 | task/scope/generation/admission에 묶인 실행과 검증 |
| 실행 소유 | tmux native pane 및 provider의 내부 scheduling | 기존 parent runtime, physical-call producer, supervisor |
| 작업 전달 | idle prompt에 한 줄 `--task` 입력 | 검증된 binding과 bounded request/receipt |
| 현재 관측 | pane/model/quota 및 일부 native child 표시 | canonical contract, resource claim, physical intent, request evidence |
| 비용·fan-out | OMS campaign accounting 밖; 전체 native 후손 예산 증명 없음 | 공통 admission과 usage liability를 재사용하는 범위만 보장 |
| 결과 수락 | 대화 보고 자체는 patch acceptance가 아님 | 기존 review/admission/publication proof 필요 |
| 제약 | unmanaged Main만 허용하며 managed campaign에서는 거절 | built-in vendor TeamRequest control은 아직 지원하지 않음 |

근거: [orchestrator.rs](../crates/oms-command-panel/src/orchestrator.rs),
[panel README](../crates/oms-command-panel/README.md),
[Team runtime](TEAM-ORCHESTRATION-RUNTIME.md).

`orchestrator.rs::require_main`은 Room Main membership과 lifecycle record를 확인하고
`panel_native_scheduling=unmanaged`를 요구한다. Campaign 환경이 있으면 거절한다.
따라서 화면에 provider orchestrator가 보이는 것만으로 managed Team·예산·자식 cleanup을
구현했다고 설명하지 않는다. Provider header도 참여자나 제어 대상 identity가 아니다.

Provider 창들은 기본적으로 **같은 repository cwd**에서 시작한다. 각 창을 열어도 writer별
worktree가 생기지 않는다. Startup prompt의 bounded work, child scope, verification 지시는
실제 worktree 격리·resource reservation의 증거가 아니다. 기존 write-delegate worktree를
배정 단위로 재사용하는 것이 이 경계의 우선 구현이며 새 격리 engine은 필요하지 않다.
현재 명령은 native permission bypass flag를 전달하면서 OMS child marker는 유지한다.
후속 작업은 이 동작을 숨기거나 새 권한으로 확대하지 않고 실제 효과와 범위를 표시한다.

`--task`의 `Turn`은 Main attempt/provider/현재 시각/task 문자열을 해시한 prompt text다.
그 자체는 durable request ID나 result receipt가 아니다. 현재 입력 경로는 문자 입력 후
Enter를 보내고 `submitted`를 반환하며 canonical task/attempt/result receipt를 연결하지 않는다.
응답 손실 뒤 같은 명령을 다시 부르는 것을 멱등 재전송이라고 설명해서는 안 된다.

## 3. 현재 PR 상태와 재사용할 결과

| PR | 현재 main에 있는 결과 | 후속 작업의 처리 |
|---|---|---|
| [#224](https://github.com/eightmm/oh-my-setting/pull/224) | Rust runtime·native services·hooks·peer·graph·landing 통합 | Python runtime을 되살리지 않고 현재 Rust public entrypoint부터 확인 |
| [#226](https://github.com/eightmm/oh-my-setting/pull/226)–[#228](https://github.com/eightmm/oh-my-setting/pull/228) | permissions 보존, legacy takeover, workspace 정리 | install/repair/uninstall의 reversible ownership 유지 |
| [#229](https://github.com/eightmm/oh-my-setting/pull/229) | landing services, stage-G, panel와 concurrency 개선 | 새 TUI/상태 저장소 대신 현재 controller/Reader 확장 |
| [#235](https://github.com/eightmm/oh-my-setting/pull/235)–[#237](https://github.com/eightmm/oh-my-setting/pull/237) | recovery, scheduled intent, collaboration/publication 연결 | 기존 lifecycle·intent·receipt 경로 재사용 |
| [#238](https://github.com/eightmm/oh-my-setting/pull/238) / [#239](https://github.com/eightmm/oh-my-setting/pull/239) | feedback observation 및 제한된 repair 코드 | observation 유지; repair 확대는 별도 필요 확인 |
| [#240](https://github.com/eightmm/oh-my-setting/pull/240) / [#241](https://github.com/eightmm/oh-my-setting/pull/241) | physical usage receipt와 campaign soft admission | Team lead/reviewer/retry도 같은 accounting에 연결 |
| [#242](https://github.com/eightmm/oh-my-setting/pull/242) / [#243](https://github.com/eightmm/oh-my-setting/pull/243) | 선택형 Apptainer 및 SSH 단일 파일 resume | host 검증 없는 기본 의존성으로 승격하지 않음 |
| [#244](https://github.com/eightmm/oh-my-setting/pull/244)–[#248](https://github.com/eightmm/oh-my-setting/pull/248) | 연구 후속과 Team 구조·runtime·adapter·validation 계약 | 광범위한 설계 시리즈를 반복하지 않고 남은 bridge를 구현 |
| [#249](https://github.com/eightmm/oh-my-setting/pull/249) | usage 중복 validation 감소 | full-history replacement 비용은 별도 확인 |
| [#250](https://github.com/eightmm/oh-my-setting/pull/250) / [#251](https://github.com/eightmm/oh-my-setting/pull/251) | reviewed memory와 native capability evidence 정정 | Team 결과를 자동 승인된 memory나 live capability로 간주하지 않음 |
| [#252](https://github.com/eightmm/oh-my-setting/pull/252) / [#253](https://github.com/eightmm/oh-my-setting/pull/253) | 기본 discovery 축소 및 Team panel/model 정책 | 명령 확장보다 기존 front door와 실제 흐름 연결 |
| [#254](https://github.com/eightmm/oh-my-setting/pull/254) | shared admission, bounded duplex, custom report channel, Team/route projection | 해당 physical producer와 authority를 그대로 사용 |
| [#255](https://github.com/eightmm/oh-my-setting/pull/255) | explicit task proposal receipt, context attribution, scoped route metadata consumer | proposal 적용과 native catalog producer의 남은 경계 연결 |
| [#256](https://github.com/eightmm/oh-my-setting/pull/256) | single-task publication proof와 delegate context compilation | 여러 member 통합을 완료했다고 표시하지 않음 |
| [#257](https://github.com/eightmm/oh-my-setting/pull/257) | 기본 native panel 진입·재연결 | 기본 panel 자체를 새로 만드는 PR 불필요 |

## 4. 기능 상태: code 존재, 연결, 운영 증거를 분리한다

`wired`는 public entrypoint에서 producer로 연결된다는 뜻이며 live vendor 검증을 뜻하지 않는다.
`opt-in`은 명시적으로 선택해야 한다는 뜻이다. `fixture`는 로컬 가짜 provider/실제 Git 객체 등의
검사 범위다. `spec`은 후속 계약이며 runtime 완료 상태가 아니다.

| 기능 | 기준 SHA 상태 | 아직 없는 연결 또는 증거 |
|---|---|---|
| Overall Main + provider native panes | wired, unmanaged | canonical per-task attempt/usage/result와의 연결 |
| native pane focus/dock/reuse | wired | provider 내부 자식 전체의 lifecycle 제어 증거 |
| immutable TeamSpec/generation | wired, opt-in, fixture | 전체 TM-01 fault matrix·실제 provider 운영 검증 |
| shared physical admission | wired 범위 존재 | unmanaged native 후손까지 포함한 hard cap으로 일반화 불가 |
| custom `report_blocker` | wired, opt-in, fixture | built-in Codex/Claude/AGY protocol bridge |
| custom `propose_tasks` | wired, explicit opt-in, fixture | 검토된 task로 atomic 적용하고 dispatch하는 bridge |
| Team request의 dispatch/continue/message/cancel/result | spec | 허용 action·target·expiry·budget을 검증하는 실제 consumer |
| 여러 member의 통합 acceptance | spec/prerequisite partial | ordered composite artifact와 composed-tree review |
| single-task publication verification | wired, fixture | live remote publication 증거와 Team composition은 별개 |
| `panel --teams` | opt-in read projection | 기본 board의 Team 작업 흐름·controls·live refresh 검증 |
| `panel --routes`와 route snapshots | wired read evidence | account-scoped native catalog producer·전체 eligibility 정책 |
| context manifest compile/attribution | wired, fixture | 실제 모델 품질·과금 개선의 paired evidence |
| SSH/Slurm/Apptainer | 개별 기존 producer·선택형 기능 | provider Team의 원격 자식 제어로 자동 확장되지 않음 |

근거 경로는 [Team projection](../crates/oms-panel-core/src/teams.rs),
[request producer](../crates/oms-command-plan/src/team_request.rs),
[native request consumer](../crates/oms/src/services_team_requests.rs),
[routing](../crates/oms-panel-core/src/routing.rs)이다.

## 5. Python 시절 기능의 의미와 Rust 재사용 판단

| 보존할 의미 | 현재 재사용 대상 | 처리와 피할 중복 |
|---|---|---|
| 작업 분해·dependency·claim·review·완료 | `oms-command-plan` | 재사용; provider별 독립 plan DB 금지 |
| 제안 검토 후 atomic task 추가 | `oms-command-plan/src/proposal.rs` | Team receipt와 기존 `apply-proposal` 연결 |
| 읽기/쓰기 worker, 분리된 patch와 evidence | `oms-command-peer`, `oms-panel-core/src/dispatch.rs` | 재사용; native 대화 보고를 patch로 추정하지 않음 |
| 자식 process·취소·재시작 복구 | `oms-supervisor`, `oms-command-operations` | 재사용; provider별 kill/retry engine 추가 금지 |
| 메시지 전달·cursor·ack | `oms-command-room`, native collaboration services | 재사용; ack를 승인/작업 완료로 승격하지 않음 |
| patch review·admission·publication | `oms-panel-core`, `oms-command-landing`, Plan finish | 재사용; multi-member 합성만 추가 |
| 문맥 선택·저장본 조회·attribution | `oms-runtime`, `oms-command-peer`, `oms-mcp-server` | 재사용; 각 provider에 전체 transcript 복제 금지 |
| journal·handoff·resume note | `oms-command-journal`, `oms-command-task`, `oms-command-hooks` | 재사용; 기록을 두 번째 실행 scheduler로 사용하지 않음 |
| 장기 지식과 failure lesson | `oms-command-memory` 및 #250 review 계약 | 재사용; 미검토 Team summary 자동 주입 제외 |
| 그래프·연구 run·benchmark·HPC | `oms-command-graph`, `oms-command-runs`, `oms-runtime` | 실제 과제에서 선택; ordinary coding 필수 단계로 만들지 않음 |
| usage·resource·dispatch ledger | `oms-command-telemetry` 기존 stores | 재사용; unknown 비용을 0으로 보정하지 않음 |
| panel board·Reader·focus·input pane | `oms-command-panel`, `oms-panel-core` | 재사용; 별도 웹 UI나 provider별 항상 열린 pane은 보류 |
| Python shim·retired evidence 링크 | 현재 native producer/fixture로 대체 | production fallback으로 복원하지 않음; 잔여 링크는 소스 근거로 정정 |

이 표는 historical parity 시험을 대신하지 않는다. 의심되는 누락은 old public behavior를
고정한 뒤 current CLI→service→producer→fixture를 추적한다. Python 구현과 이름이 다르다는
이유로 새 기능을 만들거나, PR이 닫혔다는 이유로 현재 기능을 제거하지 않는다.

## 6. 우선순위가 높은 구현 bridge

### P0-A — Managed Team의 예약 반환·취소·복구를 연결한다

현재 native physical producer는 Team과 writer 자원을 예약하지만
`retire_team_checked` / `retire_write_checked`는 production consumer가 없다.
`team-cancel`은 계약을 철회할 뿐 실행 중 member의 종료를 보증하지 않는다.
실행은 끝났어도 Team/write-WIP가 남아 다음 작업이 capacity에 막힐 수 있다.

정확한 source, canonical decision guard, 실행 정리와 candidate 처리의 분리,
held domain의 epoch 재개는 [lifecycle 구현 패킷](ORCHESTRATOR-LIFECYCLE-IMPLEMENTATION.md)에 있다.
이 패킷은 managed 경로의 정산을 다루며 기존 unmanaged pane의 native 자식을 자동 관리하지 않는다.
현재 producer는 persistent session 예약을 만들지 않으므로 session-idle/close의 미연결을
현재 session 누수로 단정하지 않는다. 두 경로를 혼동하지 않고 아래 native 연결을 별도로 만든다.

### P0-B — Native assignment를 task·worktree·결과에 연결한다

사용자가 실제로 쓰는 경로는 이미 Overall Main → native provider pane이다.
실행을 표시하는 것과 attempt identity, 결과 보존, 종료 확인, acceptance를 연결하는 것은 다르다.
기존 managed Team 정산과 달리, 이 작업은 native 대화와 canonical 업무의 상관관계를 만든다.
새로운 mutable task DB 대신 기존 Plan/Room/private operation binding을 확장한다.

1. 전송 전에 parent가 stable assignment ID, task/criterion digest, owner generation,
   provider/native-session binding, base SHA, scope와 result contract를 기록한다.
   시간 기반 Turn 문구나 pane label은 durable identity를 대체하지 않는다.
2. Lead가 작업을 나눌 때 기존 `apply-proposal`과 `peer-delegate` worktree 경로를 우선 연결한다.
   Native child의 격리를 이용하는 경우 실제 cwd/worktree identity를 관측해야 한다.
   이미 실행 중인 native 세션에 `cd`를 지시한 것만으로 격리 성공을 표시하지 않는다.
3. 서로 다른 provider의 writer가 같은 checkout을 사용하면 기존 write-WIP/scope/lease로
   충돌을 드러낸다. 격리되지 않은 동시 write는 명시적 shared-workspace 모드로 표시하고,
   native 대화를 managed containment나 hard capacity로 재분류하지 않는다.
4. 전송 intent를 먼저 저장하고 submit/transport acknowledgement/model consumption/result를
   구분한다. Enter 이후 응답 손실은 unknown이며 같은 assignment를 자동 재전송하지 않는다.
5. Native 보고는 검증할 입력으로 읽는다. Parent가 실제 artifact/base/tree를 캡처하고
   기존 review/admit/landing 경로로 넘겨야 완료다. Team-level 결과 합성은 아래 P1과 연결한다.

최소 계약은 identity·목적·scope·base·기존 evidence reference이며 전체 transcript 복사나
provider 내부 tool 호출을 임의로 통제하는 새 프로토콜이 아니다. 기존 session의 권한/default를
바꾸는 migration은 별도이며, 사용자가 선택한 native 작업 흐름을 조용히 재작성하지 않는다.

완료 판단에는 native pane 종료와 native 자식 정리의 차이, owner 변경, 모호한 전송,
dirty work 보존, result evidence와 현재 task의 binding을 포함한다.
Known limitation을 숨기기 위해 `unmanaged` 값을 지우거나 campaign guard를 완화하지 않는다.
최소 수직 fixture는 task binding → 기존 write worktree → durable send intent → 불명 전송 보존
→ exact result artifact → 기존 admission을 연결해야 한다. 같은 요청 재시도는 기존 결과를
읽고, unknown 전송은 새 작업을 만들지 않으며, 종료·예산 증거가 없으면 unknown을 유지한다.

### P1 — Lead proposal → reviewed plan → existing dispatch

현재 `propose_tasks`는 bounded task 분해를 receipt로 남길 뿐 task graph를 바꾸지 않는다.
task에는 id/title/allowed/depends만 있으며 verifier·criterion coverage·assignment를 제공하지 않는다.
이를 곧장 실행 권한으로 해석하지 않고 다음 연결을 만든다.

1. Parent가 canonical Team receipt와 현재 generation/task digest를 읽는다.
2. Parent가 acceptance, verifier, 필요한 assignment/coverage를 명시적으로 보충한다.
3. 원본 receipt/digest와 검토된 내용의 차이를 보여 주는 bounded proposal을 만든다.
4. 기존 `apply-proposal`의 plan/proposal CAS, scope, duplicate/dependency 검사로 atomic 적용한다.
5. 기존 dependency-ready dispatch가 새 task를 실행한다. 두 번째 scheduler를 만들지 않는다.

재사용: [proposal.rs](../crates/oms-command-plan/src/proposal.rs),
[plan usage](../crates/oms-command-plan/src/usage.txt), 기존 panel/plan dispatch.
완료 조건은 replay/no-double-add, stale receipt/owner/plan 거절, verifier 없는 제안의 보류,
scope 확대 거절, partial failure 후 기존 plan 보존, public entrypoint fixture다.

### P1 — 여러 member의 결과를 검증 가능한 하나의 결과로 만든다

현재 [acceptance.rs](../crates/oms-panel-core/src/acceptance.rs)는 한 task에 unique worker
candidate 하나를 요구한다. #256의 native finish 수정은 이 single-task proof를 연결했다.
TeamResult, ordered component composition, required/optional member 정책까지 완성하지 않았다.

첫 변경은 component별 base/patch/tree/attempt/task/evidence를 묶는 manifest와 상태 투영이다.
다음 변경은 기존 disposable integration worktree에서 정해진 순서로 적용하고,
**합쳐진 exact tree**에 기존 verifier와 독립 review를 실행하는 acceptance 연결이다.
각 member의 green을 합쳐진 tree의 green으로 재사용하지 않는다.
Crash 중간 tree, 동일 경로 충돌, missing required member, moved base, replay를 fixture로 확인한다.

### P1 — 한 built-in provider에 최소한의 managed request 경로를 연결한다

현재 custom read adapter의 `oms-team-request-v1` 선언·binary observation·owned channel을 재사용한다.
Codex/Claude/Antigravity built-in은 이 opt-in을 거절하므로 단순 설정 변경으로 활성화하지 않는다.
공식 transport가 제공하는 실제 기능 하나를 고르고, supported version·identity·queue bounds와
취소/ack 의미를 검증한다. Native terminal 텍스트 parsing을 authenticated protocol로 취급하지 않는다.
Read-only lead/report/proposal부터 연결하고 scoped writer는 별도 acceptance가 준비된 뒤 추가한다.
기존 child guard를 해제하는 재귀 worker 호출은 구현 방법으로 사용하지 않는다.

### P2 — 모델·effort 선택과 실제 실행을 연결한다

기준 SHA의 native orchestrator 기본 모델은 Codex `gpt-6.2-sol`, Claude `opus`,
Antigravity `gemini-pro-orchestrator`이며 `orchestrator_command`에서 고정한다.
[CLI parser](../crates/oms-command-panel/src/cli.rs)는 `--orchestrator`에 대한
**`--model`과 `--reasoning-effort` 조합을 모두 거절**한다. 조용히 무시하는 동작이 아니다.

작은 runtime 보완 [PR #258](https://github.com/eightmm/oh-my-setting/pull/258)은
새 orchestrator 생성 시 명시적 model/effort를 전달하는 구현 draft다.
기존 대화를 재개하면서 모델이 바뀐 것처럼 응답하거나 unsupported provider effort를
추정해 전달하지 않는다. 이 보완 PR의 구현·검증 상태는 해당 PR에서 별도로 기록한다.
본 감사의 pinned baseline에는 explicit override가 없다는 사실을 유지한다.

더 넓은 전체 조합 선택은 별도 작업이다. 현재 routing schema 2는 low/medium/high만 허용하고
tier 안의 동일 provider 중복을 거절한다. 모델 목록을 설정에 추가하는 것만으로 해결되지 않는다.
기존 tier는 추천 profile로 유지할 수 있지만 선택 가능 집합은 model·effort·provider·account의
실제 capability와 연결해야 한다. Native catalog의 account/endpoint/observed/expiry producer가
아직 없어 scoped evidence는 unknown이다. Snapshot consumer fixture를 entitlement로 표현하지 않는다.
최종 resolved Fable eligibility와 exact effort 검사는 이미 source에 있으므로 재구현하지 않는다.

### P2 — 기본 board에서 Team/task 흐름을 읽고 판단하게 한다

기존 `--teams`와 `--routes` projection, Reader, task-first panel 설계를 연결한다.
Task → Team → member → result evidence와 blocked reason/next actor를 보여 주고,
native unmanaged와 managed Team을 같은 완료·비용 숫자로 합치지 않는다.
조작은 producer capability에 따라 노출하며 read/inspect는 provider 호출을 만들지 않는다.
Unknown child/usage/cleanup은 null 또는 unreported로 유지한다.
Live refresh는 기존 incremental 관측을 재사용하고 rotate/truncate·두 panel·좁은 화면을 검증한다.

### P2 — 사용량 저장의 반복 I/O를 줄인다

#249는 validation 중복을 줄였지만 usage writer의 full-history atomic replacement는 남았다.
[usage store](../crates/oms-command-telemetry/src/store/usage.rs)의 비용을 실제 보존량과
update 횟수로 측정하고, 기존 canonical writer 안에서 incremental persistence/compaction을 검토한다.
Historical spend, late correction, pending/unknown physical call은 모두 보존해야 한다.
파일을 단순 truncate하거나 새 ledger를 병행해 숫자를 맞추지 않는다.
Rust 전환이나 source-derived bytes만으로 서버 physical I/O 개선을 주장하지 않는다.

## 7. 공식 provider interface에서 추가 확인한 경계

2026-11-18 공식 문서 조사이며 설치된 CLI의 live 검증 결과가 아니다.
아래 기능을 OMS에서 쓸 때는 설치 버전·transport·account와 actual receipt를 다시 확인한다.

| 공식 source | 기능과 제약 | OMS 작업에 반영할 항목 |
|---|---|---|
| [Codex App Server](https://learn.chatgpt.com/docs/app-server) | model/list의 model별 effort와 thread/turn identity, expectedTurnId를 사용하는 steer | 기존 catalog를 account별 관측과 연결; steer를 모델/cwd 변경으로 해석하지 않음 |
| [Codex subagents](https://learn.chatgpt.com/docs/agent-configuration/subagents) | 자식은 부모 permission/sandbox를 상속 | 역할 prompt만으로 읽기 전용 격리 또는 worktree 분리를 보장하지 않음 |
| [Claude Agent Teams](https://code.claude.com/docs/en/agent-teams) | interactive teammate와 headless ordinary subagent가 다름; native teammate resume 제약 | 실행 mode별 capability, reconnect와 새 team 생성의 구분 |
| [AGY Swarm](https://antigravity.google/docs/swarm/) | 기본 workspace 공유, 별도 working copy는 선택형; worker 삭제 시 작업 공간 보존 주의 | 삭제 전 candidate/artifact 캡처; shared checkout 충돌 검사 |
| [AGY headless](https://antigravity.google/docs/cli/headless/) | step usage와 최종 cumulative usage의 범위가 다름; stdin close는 즉시 cancel이 아님 | usage 중복 합산 금지; owned cleanup을 별도 관측 |
| [AGY effort](https://antigravity.google/docs/cli/commands/effort), [Claude CLI](https://code.claude.com/docs/en/cli-reference) | 모델별 지원 effort가 다름; AGY Orchestrator는 fixed effort | 전체 모델에 동일 effort enum 강제 금지; requested와 applied 구분 |

## 8. 보류할 확장과 문서 정리

- 별도 distributed scheduler, provider별 독립 DB, 새 daemon은 현재 bridge의 선행 조건이 아니다.
- ACP 전체 도입, 웹 frontend, 자동 feedback repair 확대는 named recurring need가 있을 때 진행한다.
- Remote/GPU/Slurm Team은 로컬 task identity·result acceptance가 검증된 다음 기존 producer에 연결한다.
- Paid model 비교와 default promotion은 구현 fixture와 구분하고 별도 과제·예산으로 수행한다.
- 무조건 모든 provider를 동시에 띄우거나 모든 task를 multi-agent로 바꾸는 기본값은 추가하지 않는다.

기존 문서 첫머리의 `DESIGN ONLY`와 당시 PR 상태는 현재 코드 상태를 대표하지 않을 수 있다.
예를 들어 Team runtime은 초기 설계 문구와 이후 native 구현 subsection이 함께 있다.
후속 PR은 해당 기능의 구현 범위·미연결 범위·fixture/live evidence를 같은 곳에 갱신한다.
과거 baseline과 미래 acceptance는 삭제하지 않되 현재 완료 상태와 섞지 않는다.

## 9. 구현 에이전트 인계와 완료 조건

작업 시작 시 current main과 열린 PR을 다시 읽고 이 pinned 감사 이후의 변경을 대조한다.
P0 lifecycle 패킷, 작은 model override 변경, P1 proposal/integration을 독립 범위로 다룬다.
같은 canonical writer를 고치는 PR은 순서를 정하고 변경된 base에서 다시 검증한다.

| 구현 순서 | 기존 소유자/재사용 | 완료를 판단할 사용자 흐름 |
|---|---|---|
| LC-01/02: 정산 guard와 Team/WIP 반환 | root services + Plan + telemetry checked APIs | 작은 cap에서 작업 A 처리 뒤 작업 B가 실제로 시작 |
| LC-03/04: active cancel과 epoch | Supervisor + physical binding + resource CAS | cancel 요청/종료/후보 보존/재개를 별도로 확인 |
| Native assignment + proposal bridge | Plan/Room + apply-proposal + peer dispatch | Main→lead 제안→검토된 worker 실행→기록된 결과 |
| Composite result | 기존 results/review/admission/landing | 개별 green이지만 합성 실패한 후보를 수락하지 않음 |
| Board와 catalog | 기존 Reader/Team/route projections | 어느 task가 무엇 때문에 막혔고 누가 조치할지 확인 |
| Bounded persistence | 기존 usage writer | 같은 history와 correction을 보존하면서 실제 I/O 감소 측정 |

각 PR에는 다음을 포함한다.

1. 실제 사용자가 막히는 public 동작과 그 원인을 먼저 설명한다.
2. 기존 Rust producer·lock·receipt·fixture를 재사용한 위치를 명시한다.
3. 성공 경로와 함께 stale/replay/partial failure/cleanup unknown을 nearest fixture에서 검사한다.
4. 실행한 checks와 수행하지 못한 live/host 검증을 분리한다. 전체 suite 통과를 추정하지 않는다.
5. Published diff와 source HEAD를 확인하고 코드 존재·default 활성화·운영 호환성을 구분한다.

완료는 문서나 worker report 생성만으로 판단하지 않는다. 실제 entrypoint가 기존 producer를
통과하고, 검증 가능한 evidence가 남으며, 실패 뒤에도 기존 task/작업물/책임이 보존되어야 한다.
이 감사 PR은 위 후속 runtime 기능을 구현했다는 주장을 하지 않는다.
