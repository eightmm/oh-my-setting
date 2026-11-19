# Overall Main / provider orchestrator: 구현 현황과 연결 작업

작성 기준: 2026-11-18. 감사 대상은 main
[`f9a695ea2de90a160754252c1ee9cbaa4dd7a488`](https://github.com/eightmm/oh-my-setting/commit/f9a695ea2de90a160754252c1ee9cbaa4dd7a488)이다.
이 문서는 현재 소스·PR의 연결 상태와 후속 구현 범위를 정리한다.
설계 채택, fixture 통과 기록, 실제 provider 호환성, 운영 활성화를 구분한다.
문서 추가 자체로 모델 호출, 기본 설정 변경, 설치 또는 자동 병합을 수행하지 않는다.

추가 조사에서 확인한 17개 성능·신뢰성·검증·원격 산출물 작업은
[구현 대기 큐](ORCHESTRATOR-WORK-QUEUE.md)에 PR #260–#263과 함께 연결했다.
Graph 저장·검색의 비교 분석과 13개 정확성·평가 작업은
[#264](https://github.com/eightmm/oh-my-setting/pull/264)로 추가했으며 같은 큐에서
PERF-04와 연결한다. 이 명세들은 runtime 구현 완료를 뜻하지 않는다.
아래 P0를 유지하면서 중복 구현과 같은 파일의 병렬 변경을 피하는 순서로 진행한다.

2026-11-19 후속 인계는 main
[`7d9b291592a926ce389693141d11ef8269eccfcd`](https://github.com/eightmm/oh-my-setting/commit/7d9b291592a926ce389693141d11ef8269eccfcd)의
Plan/Room/lifecycle/result/skill-eval 경계를 다시 확인해 P0-B와 아래 event·평가 패킷을
구체화했다. **추가된 내용은 구현 명세이며 runtime 연결·provider 실행 검증은 미완료다.**
이 범위의 Overall Main 선택은 `codex|agy|claude`다. Main host, 추론 model, 작업 권한은
별도 축이며 Main과 같은 provider의 Lead도 별도 participant/attempt로 사용할 수 있다.
Provider 이름이 같다는 이유로 역할을 합치거나 모든 provider Lead를 항상 열지 않는다.

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
일반 native 대화는 task binding 없이 그대로 사용한다. 이 binding은 durable assignment와
결과 acceptance가 필요한 작업에 연결하며, 작은 대화마다 새 Plan 생성을 요구하지 않는다.
새로운 assignment ID 계열·mutable task DB·scheduler 대신 다음 producer를 연결한다.

| 기존 owner / source | 이미 있는 계약 | 이번 연결에서 맡을 일 |
|---|---|---|
| [Plan engine](../crates/oms-command-plan/src/engine.rs), `claim` / `Snapshot` | `plan_id`, task, `lease_id`/`lease_epoch`, owner·scope·완료 검사 | task/criterion/base와 현재 claim을 전송 전후에 재검증 |
| [Lifecycle events](../crates/oms-command-operations/src/events.rs), `StartRequest` / `create_attempt` | task·parent attempt·refs, durable spec, start/event idempotency | native 배정의 attempt와 전송 intent를 기존 writer에 기록 |
| [Room core](../crates/oms-command-room/src/core.rs), `send` / `bind` / `acknowledge`; [cursor](../crates/oms-command-room/src/cursor.rs) | participant/consumer binding, message ID, bounded updates, 명시적 `received` | addressed 전달과 수신 확인, Main 교체·room lineage 연결 |
| [Dispatch](../crates/oms-panel-core/src/dispatch.rs), [results](../crates/oms-panel-core/src/results.rs), [acceptance](../crates/oms-panel-core/src/acceptance.rs) | 기존 worktree/worker lineage, exact patch·evidence, review/admission | 결과 보존과 검증·수락의 별도 결정 |

`oms-command-task::assignment`는 현재 provider/model/effort/workload의 route validation이다.
전송 상태나 session record를 이 object에 넣지 않는다. 필요한 versioned binding은 기존
operation의 private evidence/`refs`에 연결하고 canonical writer가 검증한다. 임의의 JSON이나
native 출력이 lifecycle·Room·Plan을 직접 쓰는 권한이 되지 않는다.

**단계 1 — claim과 전송 intent.** P0-B의 Plan/operations owner가 먼저 작은 PR로 연결한다.
입력은 canonical plan/task와 criterion digest, 현재 lease, Overall Main attempt/Room membership,
Lead participant, provider/account/host 및 관측 가능한 native session·pane/process generation,
base SHA, 허용 scope, 실제 cwd/worktree, 결과·verifier 계약이다. 지원하지 않는 identity는
명시하고 표시 이름·timestamp·PID만으로 빈 native session binding을 채우지 않는다.
같은 provider의 Main/Lead도 각 identity를 따로 검증한다.

Parent는 현재 Plan claim을 읽어 동일 배정에서는 다시 claim하지 않고, `create_attempt`의
idempotency로 재요청을 같은 attempt/spec에 묶는다. 같은 key에 다른 task/lease/scope/body를
넣으면 거절한다.
Transport side effect 전에 binding과 payload digest의 intent를 저장하고, 전달 직전에
owner/lease/native target을 다시 확인한다. 기존 writer의 lock 순서를 지키며 interrupted
prefix를 보존한다. Claim만 성공했거나 attempt만 생긴 경우를 submitted로 추정하지 않는다.
출력은 그 attempt와 기존 evidence reference이며 새로운 업무 원장이 아니다.

**단계 2 — 전달과 소비 증거.** P0-B의 Room/host owner가 첫 지원 native surface부터 연결한다.
아래는 증거의 의미이며 새로운 공통 runtime enum이나 모든 provider의 동일 기능을 가정하지 않는다.

| 관측 | 이를 입증할 최소 근거 | 이를 근거로 주장할 수 없는 것 |
|---|---|---|
| intent 보존 | canonical attempt/spec와 같은 binding/payload digest | provider 전송 또는 실행 |
| submitted / transport ack | 해당 target의 실제 입력/transport receipt | 모델의 소비, task 성공 |
| consumed | exact Room message의 수신자 ack 또는 지원 transport의 명시적 소비 증거 | 실행 시작·결과 수락; 단순 cursor 이동은 ack도 아님 |
| result available | 같은 task/lease/attempt에 묶인 immutable artifact와 실제 verification evidence | review/admission 통과 |
| accepted | 기존 acceptance producer의 exact base/patch/tree/verifier 결정 | push/publish 또는 모든 native child 정리 |

Room의 `received`는 참여자가 소비를 확인한 기록이다. 모델 내부 실행을 추정하는 센서가 아니다.
Native transcript의 “완료” 문구, idle prompt, pane 종료는 별도의 낮은 수준 관측으로 둔다.
Enter 이후 receipt 손실·timeout이면 intent와 partial evidence를 보존하고 unknown으로 읽는다.
기존 attempt/message/transport evidence를 조회해 확인하기 전에는 자동 resend·새 attempt·새 Lead를
만들지 않는다. 확인된 미전송 또는 검증된 transport idempotency가 있을 때만 같은 의도를 재개한다.

**단계 3 — 실제 결과와 acceptance.** P0-B와 TM-05의 기존 result owner가 연결한다.
Lead가 OMS canonical task graph나 OMS-managed worker 구성을 확장하는 proposal은 검토된
`apply-proposal` 뒤 기존 dispatch를 사용한다. Provider 내부 subagent는 Lead에 상속된
scope·권한 안에서 native loop가 관리하며, 각 native child를 OMS proposal·`peer-delegate`로
전환할 필요는 없다. OMS-managed writer는 기존 `peer-delegate` worktree와 scope/lease 경로를
우선 재사용하며, 결과를 연결하는 native working copy는 실제 identity를 확인한다.
`cd` 지시나 서로 다른 pane은 격리 증거가 아니다. Shared checkout의
writer 충돌은 기존 write-WIP/scope 정책에 남기고 unmanaged 실행을 managed로 재분류하지 않는다.
Parent가 task/lease/attempt, base, patch/tree digest, verifier 결과, 원문/로그 reference를 캡처한다.
Read-only 결과는 기존 research/evidence 완료 계약을 따르며 가짜 patch를 만들지 않는다.
검증 실패·partial result는 보존하고 task를 accepted로 바꾸지 않는다. 여러 member 합성은 아래
P1에 남겨 single-task native 연결의 첫 PR을 막지 않는다.

**단계 4 — 취소와 재개.** 기존 LC-03/04, REL-01/02, TM-04 owner의 증거를 재사용한다.
취소 의도·신규 dispatch 차단, 제어 가능한 owned subtree의 cancel, 실제 종료 관측,
candidate 처리와 자원 정산을 구분한다. Native parent 종료만으로 모든 child 정리·usage 정산을
확정하지 않는다. 관측되지 않거나 제어할 수 없는 후손은 unknown/unsupported로 유지하고,
현재 pane/PID 검색으로 소유하지 않은 작업을 kill하지 않는다.
재개는 이전 Main/Lead attempt, Room successor/membership, 현재 task lease와 preserved result를
읽은 뒤 동일 native session의 재결합 가능성을 판단한다. 지원되지 않는 native teammate resume를
새 team 자동 생성으로 대신하지 않는다. Old owner의 메시지·결과·취소가 새 lease에 적용되지 않게 한다.
Dirty work와 failed evidence를 먼저 보존하며 claim/receipt를 지워 복구를 가장하지 않는다.

**완료 확인:** 새 suite 대신 operations `lifecycle_idempotence_usage_cas_and_recovery`,
room `room_lifecycle_and_cursor`, plan `lifecycle_from_unrelated_cwd`, 기존 panel/native entry
fixture를 해당 producer PR에서 확장한다. 최소 수직 경로는 task claim → worktree/binding →
durable intent → ambiguous send → exact result → 기존 admission이다. 같은 key의 changed body,
stale lease/owner, duplicate/out-of-order ack, cancel/result race, surviving child, Main 재시작을
검사한다. Unknown 전송 재조회는 provider 호출을 만들지 않고, result와 ack만으로 task를 완료하지 않는다.
`unmanaged`/campaign guard를 유지하며 fixture 성공과 live provider 지원은 따로 보고한다.

### P1 — Native event를 Main의 짧은 근거로 전달한다

**Owner:** REL-01의 관측 coverage + TM-06의 task/result projection, 기존 Room/hook consumer.
[routing.rs](../crates/oms-command-hooks/src/routing.rs)의 consumer 검증·cursor·명시적 ack,
기존 [results reader](../crates/oms-panel-core/src/results.rs)와 [CC-04](CALLABLE-CONSOLIDATION-WORK-PACKETS.md)를
재사용한다. 새 notification bus·LLM 요약 heartbeat·provider별 transcript DB를 추가하지 않는다.

입력은 P0-B binding과 native 관측의 source/event identity·시각·coverage, 검증된 result reference다.
출력은 task/attempt, 무엇이 바뀌었는지, blocked reason/next actor, 결과·검증 상태, source reference,
unknown/omission을 담은 짧은 카드다. 원문은 기존 bounded reader로 열고 Room message의
4,000 UTF-8 byte 한도 안에 요약과 참조를 보낸다. Worker 설명은 untrusted peer data로 남긴다.
반복 progress는 기존 cursor와 같은 source event 기준으로 묶고 변경된 blocker·result·승인 필요만
전달한다. 단순 read/refresh가 새 model call이나 자동 wakeup을 만들지 않게 한다.
요약에서 일부 로그를 생략해도 failed/unknown·미소비 메시지·필수 결과 부재를 숨기지 않는다.

완료 fixture는 Main 세 종류와 same-provider Lead의 서로 다른 consumer, compaction/resume,
room rollover, duplicate event, rotated/truncated source, 긴 로그·CJK·control character를 포함한다.
재개한 Main은 같은 task/attempt의 최신 evidence를 읽고, 잘못된 consumer에게는 mail을 보내지 않는다.
전달 cursor가 이동해도 수신 ack나 acceptance를 자동 기록하지 않는다. Native 관측 누락의 의미는
REL-01이 소유하고, 결과 원문 reader는 CC-04가 소유하므로 이 패킷에서 다시 구현하지 않는다.

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

### P2 — 변경한 skill을 실제 작업 시나리오로 검증한다

**Owner:** 기존 `skill-forge eval`/runtime benchmark와 TM-10·RF-05/06.
[lifecycle.rs::evaluate](../crates/oms-command-providers-skills/src/lifecycle.rs)는 schema-1 suite의
`trigger_cases`/`task_cases`, baseline/treatment 및 `--candidate` 비교, `--allow-host-commands`,
선택적 `--record`를 이미 제공한다. [benchmark.rs](../crates/oms-runtime/src/benchmark.rs)는 그
aggregate를 읽는다. 별도 evaluator·점수 DB·필수 paid benchmark를 만들지 않는다.

첫 PR은 실제로 변경하는 skill 한 개를 임시 repo의 기존 skill lifecycle로 준비하고 작은 suite를
추가한다. Trigger에는 독립 작업 분해·명시적 native 복구 요청의 positive와, 단일 파일 수정·단순
현황 읽기의 negative를 함께 둔다. Skill 이름을 언급했다는 이유만으로 모든 prompt에 팀을 켜지 않는다.
Task case는 고정 입력에서 기존 실행 command와 별도 verifier를 사용한다.

| 실제 과제 | 관측할 결과 / 독립 oracle |
|---|---|
| 작은 순차 수정 | 한 candidate가 acceptance를 만족하고 불필요한 Lead/fan-out이 생기지 않음 |
| 독립 두 writer와 same-provider Main/Lead | 서로 다른 attempt/worktree·허용 scope; 합친 exact tree의 verifier 통과 |
| 제출 직후 receipt 손실·중복 알림 | 원래 intent/attempt 보존, 재조회만 수행, 두 번째 provider 호출 없음 |
| “완료” 보고 뒤 failing verifier 또는 살아 있는 child | task 미수락, failed artifact 보존, cleanup unknown을 성공으로 바꾸지 않음 |
| compaction/Main 재시작·stale lease 결과 | 현재 claim과 evidence만 재결합, old 결과·ack의 새 task 적용 없음; 전체 transcript 재주입 불필요 |
| 실행 surface의 지원 밖 요청 | 실제 capability에 따라 unsupported 표시; native fixed-effort/불가한 resume를 성공으로 가장하지 않음 |

Verifier는 파일·Git tree, 기존 lifecycle/Room/result projection과 fake transport의 호출 기록을
검사한다. Skill 문구를 그대로 찾는 테스트나 모델 자신의 “성공” 판정으로 대체하지 않는다.
Nearest fixture는 providers-skills `tests/project_parity.rs`의 skill lifecycle helper와 위
P0-B canonical suite다. 현재 black-box eval 동작의 직접 fixture는 이 조사에서 확인하지 못했으므로,
새 test runner를 만들지 않고 그 기존 suite에 작은 fake router/command/verifier 사례를 추가한다.

비교에는 같은 suite·skill/candidate digest, task/base, verifier와 command identity를 남긴다.
현재 evaluator는 skill/suite/candidate bytes를 재확인하지만 외부 router/command/verifier 구현을
고정하지 않는다. 해당 구현은 fixture에서 고정하거나 기존 run/capture reference로 보완하고,
aggregate만으로 oracle의 독립성·OS sandbox·모델 품질 향상을 주장하지 않는다.
실패·취소·unknown case를 결과에서 빼지 않으며 새 semantic evidence가 필요하면 기존 report를 확장한다.
`--record`는 기존 skill-evals writer만 사용하고 평가 결과가 skill 설치·승격을 자동 수행하지 않는다.
기본 검증은 local fake provider와 임시 Git repo다. 실제 모델의 skill 효과를 비교할 때만
[TM-10](TEAM-ORCHESTRATION-VALIDATION.md)의 명시적 과제·provider/account·총량 조건을 적용한다.
새 hosted CI matrix·일정 실행·유료 호출을 이 패킷의 기본 gate로 추가하지 않는다.

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
| Native event와 Main 인계 | REL-01 + TM-06 + Room/hook + CC-04 | 짧은 evidence에서 같은 task/attempt 원문을 읽고 명시적으로 소비 확인 |
| Composite result | 기존 results/review/admission/landing | 개별 green이지만 합성 실패한 후보를 수락하지 않음 |
| Board와 catalog | 기존 Reader/Team/route projections | 어느 task가 무엇 때문에 막혔고 누가 조치할지 확인 |
| 변경 skill의 작은 시나리오 | skill-forge eval + TM-10/RF-05/06 | trigger 오탐·누락과 실제 task 결과를 구분; fixture와 live 효과 구분 |
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
