# OMS bounded multi-team orchestration

작성: 2026-11-17. 상태: **DESIGN ONLY / 구현·기본값 변경 없음**.
코드 조사 기준: `32923cd388e581a6d6d24525b200fca746cda4d0`.
이 문서의 `TM-*`, 타입·정책·동작 이름은 후속 구현 계약이다. 현재 명령이나
구현 완료 목록이 아니다. 문서 병합은 모델 호출, 새 권한, 설치, 원격 실행,
유료 평가, 자동 merge/release를 승인하지 않는다.

현재 구현 상태는 [2026-11-18 source 감사](ORCHESTRATOR-COMPLETION-AUDIT.md)를 따른다.
아래 baseline/PR 상태 표는 최초 설계 당시의 기록이다. #254–#257의 후속 구현과
native unmanaged provider pane을 이 문서의 설계 완료 목록으로 혼동하지 않는다.
제품 범위는 [FEATURE-SCOPE](FEATURE-SCOPE.md), 현재 착수 순서와 남은 구현은
[통합 작업 큐](ORCHESTRATOR-WORK-QUEUE.md)를 따른다. 아래 TM ID는 원래 설계의
acceptance 참조이며, 이미 구현된 부분을 다시 만드는 별도 실행 backlog가 아니다.

## 1. 결정과 목표

**Single Main + bounded teams + one existing execution/verification plane**을 채택한다.
Main이 Codex, Claude 또는 Antigravity 어느 것이어도 같은 팀 계약을 사용한다.
Codex 전용 Main, Provider별 전역 scheduler, 새로운 agent framework를 만들지 않는다.

Single Main은 같은 목표/room/통합 대상의 소유권 경계다. 독립 프로젝트를 모두
하나의 전역 LLM에 직렬화하지 않는다. Team Lead는 하위 과제의 논리적 조정자이며
두 번째 Main이 아니다. 실제 launch/claim/approval/state mutation은 기존 부모
runtime이 수행한다. Lead 자신에게 자기 승인·재귀 spawn·범위 확대 권한을 주지 않는다.

Provider orchestrator pane은 기존 native session이고 managed Team Lead는 논리 역할이다.
Provider별 session과 task별 Team을 고정된 상하 계층으로 연결하지 않는다. 작은 작업은
Main의 direct dispatch로 처리하며 별도 provider lead와 Team Lead를 모두 만들 필요가 없다.

목표 지표는 agent 수가 아니라 **동일 품질 기준에서 검증되어 통합된 작업의 처리량**이다.
팀을 쓰지 않는 기존 단일 호출·peer-delegate 경로는 변경 없이 유지한다.

```text
User-authorized goal
    |
One Main: goal, exceptions, integration responsibility
    |
Existing parent runtime: plan, reservations, policy, processes, receipts
    |                         |
Team A / logical lead         Team B / logical lead
    |                         |
Codex + Claude workers        Optional provider-native team
    |                         |
Candidate artifacts + independently reproduced evidence
    |
Existing review -> patch-admit -> patch-land -> authorized publication
```

그림의 lead/worker 선은 논리적 관계다. OMS-managed worker는 lead가 raw CLI로
생성하는 손자 프로세스가 아니라 부모 runtime이 실행하고 감독한다. Native team의
내부 프로세스/스레드 관계는 별도 capability와 실제 lineage 증거가 있어야 표현한다.

## 2. 현재 기반과 조사 한계

아래는 선택한 소스·문서의 정적 확인이다. 전체 저장소 감사나 native provider
실행 시험을 수행했다는 뜻이 아니다. 오래된 문서의 Python 경로를 새 구현 대상으로
복구하지 않는다. 후속 PR은 당시의 Rust source/caller/test를 다시 읽는다.

| 확인한 기반 | 근거와 확장 경계 |
|---|---|
| Scoped dispatch, carried patch, WIP, main/attempt/room 관계 | [dispatch.rs](../crates/oms-panel-core/src/dispatch.rs). 현재 owner 매칭의 direct-parent 가정을 팀 후손 집계 시 재검토한다. 현행 WIP를 machine-wide hard cap이라고 주장하지 않는다. |
| Child의 peer 호출·owner mutation 차단 | [safety.rs](../crates/oms-command-peer/src/safety.rs). `parent()`를 약화하거나 child 환경 표식을 지워 team을 실행하지 않는다. |
| Shared room, cursor, ack, scoped append | [room crate](../crates/oms-command-room/README.md). 메시지·참가·ack는 승인 또는 실행 권한이 아니다. |
| Existing process owner와 canonical lifecycle | [supervisor crate](../crates/oms-supervisor/README.md). 별도 reaper, daemon, execution queue를 만들지 않는다. |
| Admission, review, recovery, result handling | [panel core](../crates/oms-panel-core/README.md). 팀은 기존 결과·패치 계약을 묶는 단위다. |
| Provider transport와 served model family의 구분 | [provider routing](../custom-skills/oms-agent-harness/references/provider-routing.md). 같은 CLI에서 다른 vendor 모델을 실행할 수 있으므로 reviewer 독립성을 CLI 이름으로 판정하지 않는다. |
| Multi-host와 verified integration 방향 | [기존 계획](MULTI-AGENT-MULTI-HOST-PLAN.md). 새 Slurm/GPU scheduler나 원격 control plane으로 확장하지 않는다. |

### 기존 PR과의 경계

상태는 조사 시점의 스냅샷이며 구현 시 다시 확인한다.

| 작업 | 확인 상태 | 재사용/비중복 결정 |
|---|---|---|
| [#237](https://github.com/eightmm/oh-my-setting/pull/237) collaboration delivery | merged | Room 전달·publication receipt 경로 재사용. Team 전용 notification 시스템 금지. |
| [#240](https://github.com/eightmm/oh-my-setting/pull/240) provider usage receipts | merged | 물리 call/operation/session identity, revision, unknown liability 재사용. Team 비용 장부를 새로 만들지 않는다. |
| [#241](https://github.com/eightmm/oh-my-setting/pull/241) campaign usage admission | open | Soft spend admission 공통 경계를 재사용·일반화. 팀만 별도 budget logic을 복제하지 않는다. |
| [#244](https://github.com/eightmm/oh-my-setting/pull/244) research follow-up contracts | open, design only | RF-01 duplex supervision, RF-03 live observation, RF-05/06 context attribution/paired evaluation를 공통 선행 작업으로 삼는다. 계획을 구현으로 간주하지 않는다. |
| [#238](https://github.com/eightmm/oh-my-setting/pull/238), [#239](https://github.com/eightmm/oh-my-setting/pull/239) feedback/repair | open | Team 메시지나 stall이 reviewed repair 권한을 만들지 않는다. MVP 필수 의존성은 아니다. |
| [#242](https://github.com/eightmm/oh-my-setting/pull/242), [#243](https://github.com/eightmm/oh-my-setting/pull/243) container/artifact | open | 선택한 실행 backend에만 연결한다. Team MVP 때문에 컨테이너/GPU/SSH를 필수화하지 않는다. |

## 3. 실행 모드와 선택

| 모드 | 소유권 | 기본 사용 |
|---|---|---|
| `single` | 기존 단일 agent/peer 호출 | 작거나 강한 순차 의존성이 있는 작업. 기존 기본값 유지 |
| `oms-team` | OMS가 모든 lead/worker 호출·작업·결과를 관리 | 첫 multi-agent 구현. 같은 팀에 Codex와 Claude를 함께 배치 가능 |
| `native-team` | Provider가 내부 협업 loop, OMS가 외부 계약·예산·검증 관리 | 명시적 opt-in과 capability 검증 후에만 허용 |

Native 기능이 있다는 사실과 OMS가 안전하게 제어할 수 있다는 사실을 구분한다.
`available`, `documented`, `fixture_verified`, `live_verified`, `unsupported`, `unknown`
증거를 분리한다. Capability 하나가 전체 team 지원을 뜻하지 않는다.

첫 릴리스는 **read-only OMS-managed team**이다. Scoped write는 TM-05의 후보
검증·통합·복구 시험까지 통과한 후에만 켠다. Native write와 adaptive routing은
MVP 필수가 아니다. 지원 불명인 native 실행을 자동으로 다른 모드로 바꾸지 않는다.
명시한 fallback 정책, 같은 예산·범위, 새 admission이 있어야 전환한다.

## 4. 권한 불변식

1. Goal, task, plan generation, scope, verifier, base identity의 소유자는 기존 부모다.
   Lead는 제한된 구조화 요청만 제출한다. Runtime은 요청한 문자열이 아니라
   부모가 등록한 membership·generation·정책으로 권한을 확인한다.
2. Worker/Lead는 `OMS_HARNESS_CHILD` 또는 delegate-depth guard를 우회할 수 없다.
   새 요청 경로는 좁은 producer를 통해 부모가 소비하며 일반 mutation을 열지 않는다.
3. 시작 깊이는 `Main -> Team Lead -> Worker`로 제한한다. Native 내부 깊이까지
   제한이 필요한 정책은 provider가 검증 가능한 cap/제어를 제공하지 않으면 거절한다.
   Prompt에 적은 제한을 enforced cap으로 표시하지 않는다.
4. Scope/도구/네트워크/모델/계정/비용 권한은 부모 계약보다 넓어질 수 없다.
   Worktree와 환경변수는 악성 동일-UID 프로세스에 대한 OS sandbox가 아니다.
5. Process 종료, model의 done, native verifier sign-off, room ack는 acceptance가 아니다.
   검증은 exact candidate와 criterion에 묶고 publication은 기존 별도 권한을 따른다.
6. 전역 자원 집계에는 leaf뿐 아니라 lead, reviewer, repair, 모든 물리 fallback 호출을
   포함한다. Native descendant의 미관측 사용량을 0으로 취급하지 않는다.
7. Unknown launch/cleanup/usage를 성공·미실행·환불로 바꾸지 않는다. 재시작은
   reconciliation 후 새 attempt로 진행하며 과거 evidence/dirty worktree를 보존한다.
8. 운영·account·host 범위를 명시한다. 같은 Main의 cap을 여러 repo/host/account를
   포괄하는 전역 hard limit이라고 표시하지 않는다. 공유 자원 예약 경계가 없는
   cross-host 팀은 strict bounded mode에서 거절한다.

## 5. 계약과 상태 모델

아래는 최소 논리 필드다. 새 authoritative DB나 이중 task graph의 설계가 아니다.
최종 Rust 타입·저장 위치·공개 schema는 TM-01에서 기존 writer와 호환성 검토 후 확정한다.

| 계약 | 필수 내용 |
|---|---|
| `TeamSpec` | schema, team ID/generation, owning Main generation, existing goal/task/plan refs, mode, objective/criterion digests, base, allowed/forbidden scope, resource claims, policy/budget refs, expiry, capability requirements |
| `MemberBinding` | member ID, logical parent, root owner, role, access, provider/transport/account/host namespace, requested and served model evidence, attempt/worktree refs, native identity and source when known |
| `TeamRequest` | request ID, authenticated source binding, team generation, exact action/target, contract digest, expected state version, bounded payload/artifact ref, expiry |
| `TeamResult` | candidate/attempt refs, base and patch/tree digests, per-member provenance, test/review evidence refs, unresolved findings, usage coverage, cleanup state, omissions |

`TeamSpec`는 부모가 작성하는 불변 artifact이며, 활성화/변경 결정은 기존 canonical
plan/operation writer에 기록한다. Team UI 상태는 그 기록과 attempt/evidence의 projection이다.
별도 mutable team 파일을 또 하나의 결정 권위로 삼지 않는다. Contract 변경은 새
version과 영향 분석을 요구하고 기존 진행 작업의 evidence를 조용히 재사용하지 않는다.

실행 projection은 `planned -> admitted -> running -> awaiting_evidence -> settled`를
기본으로 하되 `held`, `cancelling`, `failed`, `unknown`을 별도로 표현한다.
`settled`는 실행 정산이며 성공 또는 통합 완료가 아니다. 결과의 `verified`, `admitted`,
`landed`, `published`는 기존 evidence/receipt를 그대로 읽는다. 팀 전체 green을
구성원 상태의 다수결이나 문자열 요약으로 생성하지 않는다.

## 6. 원래 TM 구현 계약과 선행 관계

아래 10개는 **원래 설계의 구현 범위와 acceptance 묶음**이다. 현재 source의 부분 구현과
남은 연결은 위 감사와 통합 작업 큐에서 확인한다. 새 작업은 현재 큐의 좁은 범위로
착수하고 해당 TM의 acceptance를 재사용한다. 이름만 있는 code PR이나 placeholder/stub를 만들지 않는다.

| ID | 범위 / 산출물 | 선행 | 완료 조건 |
|---|---|---|---|
| TM-01 | TeamSpec/MemberBinding, root-owner lineage, lifecycle projection, policy digest | 이 설계 검토 | Legacy 읽기, version 거절, cycle/foreign-owner/expired binding 거절; launch 없음 |
| TM-02 | 기존 admission의 원자적 team/member 예약, WIP·usage 연결 | TM-01, #241 또는 검토된 공통 budget 경계 | 동시 요청 cap 초과 없음; lead/reviewer/unknown/fallback 포함; 실패 시 자원 반환과 불명 liability 보존 |
| TM-03 | Parent-consumed TeamRequest, room/context delivery, 제한된 lead 계획/후속 요청 | TM-01/02 | Child guard 불변, replay/forged identity/old generation 거절; wakeup은 비용·권한 검사 후 |
| TM-04 | OMS-managed mixed-provider team 실행, read-only 먼저, cancellation/recovery | TM-03 | 기존 supervisor만 launch/reap; native fan-out 통제; 이중 실행·취소 누락 없음; single 경로 회귀 없음 |
| TM-05 | TeamResult, 독립 검증, 후보 통합·admission·landing recovery | TM-04 | exact tree 검증, shared-file 충돌·중단 복구, 자기 승인 불가; 이 단계 전 multi-writer opt-in 불가 |
| TM-06 | 기존 panel/dashboard의 team projection과 bounded event UX | TM-01; live 화면은 TM-04 및 RF-03 | managed/observed 구분, null/coverage, 비용 중복 없음; 모델 호출 없는 idle refresh |
| TM-07 | Codex App Server adapter와 검증된 child 관측/제어 | TM-04/05, #244 RF-01, capability 시험 | stdio handshake/approval/interrupt/reconnect fixture; child lineage와 cap 증거 없으면 native write 거절 |
| TM-08 | Claude native capability/observation bridge | TM-04/05; duplex 필요 시 RF-01 | `-p` ordinary subagent와 interactive teammate 구분; 지원하지 않는 start/resume/control은 명시적 거절 |
| TM-09 | AGY native capability/observation bridge | TM-04/05; duplex 필요 시 RF-01 | 실제 공개된 interface와 exact version으로 증명; board/API 불명은 unknown; 안정 interface 없으면 control 활성화 보류 |
| TM-10 | 동일 과제 평가, 명시적 선택형 routing, staged rollout/rollback | TM-04/05/06, RF-06 재사용 | 실패 포함 paired 결과, 사전 기준·예산, unknown 비용 보존; 결과 없는 자동 확대 금지 |

### 병렬화와 병합 순서

TM-01 -> TM-02 -> TM-03 -> TM-04 -> TM-05를 먼저 직렬로 안정화한다.
TM-06의 fixture/read projection은 TM-01 뒤 별도 작업이 가능하다. #244 RF-01은
공통 transport 선행 작업으로 별도 진행하되 supervisor 소유권 파일을 다른 팀이
동시에 바꾸지 않는다. TM-07/08/09는 capability/fixture 작업을 분리해서 병행할 수
있지만 provider registry·root wiring 변경은 계약 확정 후 차례로 통합한다.
TM-10의 평가 설계·fixture는 일찍 작성하고 실제 유료 실행/기본값 승격은 마지막에 둔다.

Docs PR들은 서로 다른 문서를 소유하고 같은 pinned main에 독립적으로 추가한다.
읽는 순서는 architecture -> runtime -> adapters -> validation이다. 새로운 공통 runtime
API가 필요한 구현 PR은 정확한 dependency SHA를 base로 하는 stack으로 만들고,
provider마다 복사한 supervisor/budget/room 코드를 먼저 올리지 않는다.

## 7. 코딩 에이전트용 실행 지시

현재 큐에서 선택한 작업은 해당 TM의 acceptance와 다음 조건을 brief에 포함한다.

> 현재 큐에서 지정한 남은 범위만 구현한다. 최신 main과 선행 PR 상태·SHA를 확인하고 source,
> caller, canonical test를 대조해 이미 있는 기능은 재사용한다. Rust runtime을
> 유지하며 Python/TypeScript runtime 또는 새 orchestrator/DB/daemon을 도입하지
> 않는다. Parent mutation guard, single-owner, existing admission/verification,
> unknown/receipt 의미를 보존한다. Scope가 공통 파일과 겹치면 계약 소유자에게
> 요청하고 직접 다른 팀의 범위를 넓히지 않는다. Paid provider, GPU, SSH,
> global configuration 변경은 실행하지 않는다. Fake providers·임시 Git repo로
> 성공/실패/재시작을 시험하고 public entrypoint 연결을 검증한다. 구현한 항목,
> 미지원 capability, 실제 실행한 검사, 생략한 검사와 이유를 명시한 Draft PR을 낸다.
> 단순 compile, fixture, provider 문서, live run을 서로 다른 증거로 기록한다.

작업별 제출물은 scoped diff, 계약 변화와 migration/rollback, 기존 test 확장,
명확한 완료 기준에 대한 evidence matrix다. 통합 검증을 이후의 거대한 test-only
PR로 미루지 않는다. 불확실한 native API에는 추정 구현 대신 명시적 거절과 blocker를 남긴다.

## 8. 사용자 판단이 필요한 경계

지금 진행 가능한 것은 설계 검토와 위 범위의 fixture 기반 구현 준비다. 다음은 별도
명시 승격 대상으로 남긴다: multi-writer 기본 활성화, team/global quota 수치 확정,
새 dependency, provider-native interactive 제어, provider-home 범위 확대,
유료 비교 평가, 원격 실행/GPU 사용, 자동 merge/release.

제안 초기 실험값은 최대 2 teams, 각 team 최대 2 workers이며 lead/reviewer를
별도 집계한다. 이는 최적값이나 기존 설정 변경이 아니다. 사용 가능한 총량 안에서
부모가 결정하고 reviewer capacity를 남긴다. 자원이 부족하면 fan-out을 줄이거나
single을 선택하되 승인되지 않은 유료 계정/모델로 자동 우회하지 않는다.

## 9. 동반 명세와 완료 선언

계약 상세는 [runtime](TEAM-ORCHESTRATION-RUNTIME.md)(TM-01~05),
[adapters](TEAM-ORCHESTRATION-ADAPTERS.md)(TM-07~09),
[validation](TEAM-ORCHESTRATION-VALIDATION.md)(TM-06/10 및 전체 fault matrix)에 있다.
이 문서들은 acceptance의 근거를 보존하고 현재 실행 순서는 통합 작업 큐가 소유한다.

문서의 완료는 설계·선행 관계·수락 기준을 검토 가능한 형태로 제출했다는 뜻이다.
Runtime 구현, native 상호운용, 성능 향상, OS containment, published-head CI의
성공을 대신하지 않는다. 구현 완료는 해당 TM의 실제 코드·시험·exact-SHA 증거가
모두 충족될 때 별도 기록한다.
