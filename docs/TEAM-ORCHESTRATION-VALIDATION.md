# Team orchestration: validation, observability and rollout

작성: 2026-11-17. **DESIGN ONLY / 아래 시험은 아직 구현·실행한 결과가 아님**.
OMS source 기준: `32923cd388e581a6d6d24525b200fca746cda4d0`.
관련 설계: [#245 architecture](https://github.com/eightmm/oh-my-setting/pull/245),
[#246 runtime](https://github.com/eightmm/oh-my-setting/pull/246),
[#247 adapters](https://github.com/eightmm/oh-my-setting/pull/247).
TM-06/10과 전체 구현 PR의 공통 합격 기준을 정의한다. Runtime 변경, native/유료 실행,
CI matrix 확장, 자동화 등록 또는 기본값 승격을 이 문서에서 수행하지 않는다.

## 1. 성공의 의미

Team 생성이나 여러 창의 표시는 성공 기준이 아니다. 같은 고정 과제에 대해 권한을
지키고, 검증 가능한 candidate를 만들고, 기존 admission/integration이 받아들인 결과를
기록해야 한다. 아래 사실은 각각 다른 column으로 유지한다.

`process exit`, `turn result`, `child cleanup`, `usage settled`, `candidate ready`,
`verification passed`, `review accepted`, `patch admitted`, `landed`, `published`.

Team-level projection이 이 중 일부를 생략할 때 coverage/omitted/unknown을 표시한다.
Native sign-off나 여러 agent의 합의는 실제 test·receipt를 대체하지 않는다.

## 2. TM-06 — 기존 panel/dashboard 확장

[Panel](TERMINAL-PANEL.md), [dashboard](DASHBOARD.md),
[panel core](../crates/oms-panel-core/README.md)의 기존 reader/projection을 재사용한다.
새 web server, global session crawler, 별도 상태 저장소, LLM 요약 heartbeat는 추가하지 않는다.

### 화면과 정보 경계

| 표면 | 필수 정보/조작 |
|---|---|
| Main -> Team -> Member tree | Logical parent와 실제 root owner, mode, role, provider/transport, requested/served model, current attempt와 generation |
| Team summary | Required/optional members, candidate/evidence 상태, running/session/write-WIP 각각의 cap·scope·예약, budget coverage |
| Attention | 권한 대기, 검증 실패, admission hold, quota 소진, transport gap, cleanup 불명, owner 교체를 서로 다른 원인으로 표시 |
| Results | Exact base/patch/tree, member provenance, test/review/landing receipt, unresolved finding과 missing evidence |
| Messages | addressed audience, submitted/delivered/consumed/wakeup 상태와 idempotency identity; 원문은 opt-in bounded reader |
| Controls | 실제 capability가 있는 exact team/member에만 hold/cancel/continue 요청. Mutation은 기존 parent front door로 전달 |

Managed member와 native observed member는 시각적으로 구분한다. 관측된 agent를
room participant나 approved task owner로 자동 등록하지 않는다. 통신 graph와 task DAG도
별개다. Deadline/approval 대기와 단순 quiet를 stalled/failed로 합치지 않는다.

Cancellation 버튼은 요청과 확인된 종료를 구분한다. Unsupported control은 숨긴 성공
처리 대신 이유를 보여 준다. Read-only overview를 열었다고 provider가 시작되거나
session을 resume하지 않는다. Unknown 비용은 0이 아니며 inclusive parent usage와
child usage를 이중 합산하지 않는다. 기존 schema의 null/type/exit semantics를 유지한다.

### I/O와 크기 제한

변화 없는 refresh에서 전체 log나 모든 provider-home을 다시 읽지 않는다. 기존 room의
incremental cursor/descriptor와 bounded native observation을 공유한다. 패널 수만큼 같은
stream을 중복 파싱하거나 작은 JSON update마다 전체 transcript를 재기록하지 않는다.

측정에는 refresh별 bytes read/written, filesystem metadata calls, fsync 횟수, subprocess
수, queue/record 수, peak RSS와 p50/p95 refresh 지연을 포함한다. CPU wall-time만으로
기존 서버 Disk I/O 개선을 주장하지 않는다. Truncate/rotate/partial-tail/gap과 cache
cold/warm을 구분하고, cache를 삭제해도 canonical 상태를 복원할 수 있어야 한다.

각 구현 PR은 parser/frame/event queue/message/retention의 실제 상한과 overflow 정책을
명시한다. 임의의 성능 수치를 합격으로 지어내지 않고 같은 머신·같은 dataset에서 현재
baseline과 비교한다. Safety-critical event를 조용히 버려 좋은 latency를 만들지 않는다.

## 3. 결정적 acceptance matrix

가장 가까운 canonical suite/fixture를 확장한다. 새 suite는 기존 시험이 다루지 않는
경계에만 추가하고 대체하거나 통합할 기존 fixture를 PR에 적는다. 모든 구현 PR에
관련 실패 시험을 함께 넣으며 마지막 test-only PR로 correctness를 미루지 않는다.

| ID | 주 소유 PR | 시나리오 | 필수 결과 |
|---|---|---|---|
| TV-01 | TM-01 | Legacy record, unknown schema, missing team field | Legacy 보존; missing은 null; unsupported mutation 거절 |
| TV-02 | TM-01 | 같은 이름/PID/native ID, 다른 account/host/generation | 잘못된 identity 결합·제어 없음 |
| TV-03 | TM-01/03 | Old owner, room rollover, stale policy/task/base | 새 admission 거절; 기존 artifact 보존 |
| TV-04 | TM-02 | 여러 team과 legacy dispatch가 같은 순간 요청 | 동일 admission domain에서 cap 초과 없음 |
| TV-05 | TM-02 | Lead/reviewer/repair/fallback/native child 집계 | 숨은 호출 누락 없음; unsupported bound는 거절 |
| TV-06 | TM-02 | Launch 전·후 crash와 send ack 손실 | Positive not-started만 반환; 불명은 유지, 자동 이중 실행 없음 |
| TV-07 | TM-02 | Missing/late/revised/cumulative usage | Unknown을 0으로 처리하지 않음; 중복 합산 없음 |
| TV-08 | TM-02/04 | Reviewer backlog·lead wait가 실행 슬롯 점유 | 검증 capacity 유지; deadlock/무한 busy-loop 없음 |
| TV-09 | TM-03 | Worker의 lead 사칭, recursive dispatch, scope 확대 | Parent guard와 enrollment 검증으로 거절 |
| TV-10 | TM-03 | Duplicate/out-of-order request, same ID changed body | Idempotent 결과 또는 충돌 거절, 재실행 없음 |
| TV-11 | TM-03 | Message loop, all fan-out, lost wakeup receipt | Bytes/round/call cap 적용; uncertainty 재전송 없음 |
| TV-12 | TM-04 | Independent member completion와 느린 sibling | Ready 작업 진행, 불필요한 전역 wave barrier 없음 |
| TV-13 | TM-04 | Main/lead crash, surviving workers, owner swap | Existing attempt 재결합, 새 Main 자동 증식 없음 |
| TV-14 | TM-04 | Cancel/completion race, escaped/unknown descendants | One terminal interpretation; cleanup 불명 유지 |
| TV-15 | TM-04 | 같은 port/build slot/test DB와 별도 file paths | 공유 자원 충돌 통제; 파일 분리만으로 안전 추정 금지 |
| TV-16 | TM-05 | 두 patch가 개별 test 통과하나 조합 후 실패 | Exact composed tree gate 실패, 자동 통합 금지 |
| TV-17 | TM-05 | Candidate/base/verifier 변경, self-review | Stale evidence 거절, 정책상 독립성 재확인 |
| TV-18 | TM-05 | Apply/commit/receipt 중단과 이미 적용된 patch | Actual state reconcile, blind reapply/삭제 없음 |
| TV-19 | TM-06 | 읽기만 수행, 관측 누락, 좁은 터미널/CJK/escape | 모델/권한 변화 없음; safe rendering, omission 표시 |
| TV-20 | RF-01/TM-07 | Fragment/backpressure/approval/EOF/oversize | Bounded memory와 cancel/reaping 유지 |
| TV-21 | TM-07 | Codex child filters 미지원, wrong thread/turn | Unknown/unsupported, unrelated thread 접근 없음 |
| TV-22 | TM-08 | Claude headless ordinary subagent vs teammate | Native team을 만들었다는 거짓 상태 없음 |
| TV-23 | TM-09 | AGY subagent_info/unsafe URI/cumulative usage | 제한된 관측, 경로 권한 없음, usage 차분·coverage |
| TV-24 | TM-07~09 | Native main done이지만 child 살아 있음 | Team cleanup 완료 처리 없음 |
| TV-25 | TM-10 | Failed/cancelled/unknown benchmark arm | 결과에서 누락하지 않음; 비교 coverage 표시 |
| TV-26 | 각 PR | Feature off, unsupported version, downgrade | 기존 single 동작 불변; 불명 새 상태에 fail-closed |
| TV-27 | 각 PR | Disk full, partial append, writer lock failure | 기존 writer의 불명/실패 보존, 두 번째 장부 금지 |
| TV-28 | 각 PR | 사용자 publication/GPU/원격 권한 없음 | 성공 결과가 새 부작용 권한이 되지 않음 |

Fake clock, controllable fake child, 임시 Git repo, 독립 fixture roots를 사용한다.
긴 실제 sleep, 네트워크 provider, GPU, 실제 Slurm 제출은 unit/CI 기본 경로에서 쓰지 않는다.
Linux/WSL/macOS의 cleanup/containment 차이는 capability로 보고한다. Unsupported 환경의
skip은 이름과 이유를 남기고 enforcement가 검증된 것처럼 표시하지 않는다.

## 4. TM-10 — 동일 과제 평가

[#244 RF-05/06](https://github.com/eightmm/oh-my-setting/pull/244)의 context attribution과
paired benchmark를 재사용한다. 새 evaluator/비용 DB를 만들지 않는다. Offline fixture는
protocol/correctness를 검증할 뿐 새 모델 응답의 품질·실제 속도·비용 절감을 증명하지 않는다.

### 비교 arm과 통제

Arm은 `single`, `oms-team`, capability를 통과한 `native-team`이다. 동일 task/base,
acceptance/verify digest, 허용 도구·네트워크·context·backend, 최대 총량·timeout,
명시된 모델/effort와 run manifest를 실행 전에 고정한다. Native 미지원은 미지원으로
기록하고 다른 모드의 결과로 채우지 않는다.

Topology 자체의 효과를 보려면 같은 model family/설정으로 비교한다. Codex+Claude 혼합
팀은 실용 성능 비교로 별도 cohort에 두며 모델 차이를 topology 효과라고 주장하지 않는다.
같은 상한을 부여해도 실제 spend가 같다고 가정하지 않고 실제 사용량·작업량을 함께 보고한다.

Task 유형은 작은 순차 버그, 독립 모듈 변경, 공유 API 변경, competing hypotheses 분석,
통합 때만 발생하는 실패, quota/interruption 복구를 포함한다. 각 case는 고정 oracle 또는
명시적 사람이 평가할 기준이 있어야 한다. 후보가 acceptance를 약화할 수 없도록
verifier version/digest와 보호 경계를 유지한다.

Task 수, 반복 수, arm 순서, cache 상태, 실패/재시도 정책, 중단 예산, 통계 방법,
품질 허용 한계를 먼저 선언한다. 소규모 pilot은 feasibility 확인이며 광범위한 개선의
증거가 아니다. 유료 run은 정확한 provider/account/총량 승인이 있어야 실행한다.

### 결과 지표

| 지표 | 정의와 주의 |
|---|---|
| Accepted-task rate | 고정 acceptance와 integration evidence를 만족한 과제 / 전체 배정 과제. 실패·취소·불명을 분모에서 빼지 않음 |
| Time to accepted artifact | 첫 승인 dispatch부터 검증·통합까지의 wall time. 실패/censored run은 성공 시간 평균에서 조용히 제거하지 않고 따로 보고 |
| Reported usage and cost | #240의 observed basis/coverage, #241의 active/unresolved 책임. Subscription 또는 missing cost를 달러로 환산하지 않음 |
| Cost per accepted task | 정의된 campaign의 실패 포함 비용 / accepted 수. 비용 coverage가 불완전하면 partial/unknown; 성공 0이면 정의 불가 |
| Human interventions | 새 권한/충돌/복구에 필요한 개입 수; 사용자 승인을 숨겨 자동화를 좋게 보이게 하지 않음 |
| Defects / unsupported claims | 검증된 실제 결함과 잘못된 지적을 구분. Reviewer 합의만으로 correctness를 판정하지 않음 |
| Integration pressure | 충돌, stale candidate, rebase/reverify, review backlog와 최대 WIP |
| Overhead | Lead/discussion/model wakeups, input duplication, orchestration CPU/RSS/I/O, process/session peak |

Same-task 차이와 task별 실패를 공개하며 arm 순서는 교차 배정한다. Warm cache와 cold
cache를 섞어서 절감으로 발표하지 않는다. Confidence interval 등은 사전 정한 방법과
표본수를 함께 제시하고, 실험이 적으면 불확실성을 유지한다. Google/타 프로젝트의
benchmark 수치를 OMS 개선 수치로 인용하지 않는다.

### Router 승격

초기 선택은 명시적 mode/role policy다. Team 수 확대나 phase별 model switching은
평가 후 선택형으로 도입한다. 판단 기준은 독립성·계약 안정성·검증 가능성·남은 예산과
현재 backlog다. 파일 수가 많다는 이유만으로 팀을 만들지 않는다.

Adaptive policy도 version/digest와 이유를 기록하고 허용된 model/account/scope 밖으로
나가지 않는다. 비교 결과가 없거나 불명 coverage가 크면 single/명시적 선택을 유지한다.
신규 provider 설치 또는 업데이트가 자동 fan-out이나 지출을 유발해서는 안 된다.

## 5. Rollout과 rollback

| Gate | 허용 | 승격 증거 |
|---|---|---|
| G0 | 설계/fixtures, feature off | 계약·선행 PR·test ownership 검토 |
| G1 | 소수 read-only OMS-managed teams, opt-in | TM-01~04와 relevant TV 시험, 기존 single 회귀 통과 |
| G2 | Scoped multi-writer, opt-in | TM-05의 exact integration tree·review·recovery 시험; resource admission 증거 |
| G3 | Provider별 native 관측 | Provenance/coverage와 bounded parser, ownership·privacy 검증 |
| G4 | Provider별 native control/write, opt-in | 해당 mode/version의 fan-out/isolation/approval/cancel/cleanup·usage 능력과 승인된 live smoke |
| G5 | Adaptive routing 또는 기본값 변경 | TM-10 paired evidence와 명시적 사용자/maintainer 결정 |

Gate는 provider마다 다르다. Codex G4 통과가 Claude/AGY의 G4를 의미하지 않는다.
Native read-only research라도 비용/프로세스 제한을 강제할 수 없으면 strict bounded
mode는 통과하지 못한다. UI에서 관측만 지원하는 상태를 execution-ready로 표시하지 않는다.

Rollback은 신규 team admission부터 끄고 기존 attempt의 관측·cleanup·receipt 정산을
계속한다. Native history, dirty worktree, partial patch, unknown liability를 삭제하지 않는다.
이전 바이너리가 새 schema를 안전하게 거절/읽을 수 없는 경우 먼저 drain/reconcile하고
호환성을 확인한다. 기능 flag를 껐다는 이유로 active team을 없었던 것으로 만들지 않는다.

## 6. PR별 제출과 사용자가 승인할 것

모든 구현 PR은 정확한 base/dependency SHA, 변경 scope, public wiring, 고정된 fixture,
실행한 명령/exit, 미실행 검사와 이유, unsupported capability, migration/rollback을 남긴다.
기존 `AGENTS.md`의 affected/critical CI 선택을 사용한다. Docs-only에는 reference 검사를,
supervisor·authority·permission·state 변경에는 해당 critical coverage를 적용한다.
기능마다 거대한 중복 matrix를 새로 추가하지 않는다.

지금의 문서 PR 검증과 미래 runtime 완료 시험을 섞지 않는다. Compile, local fixture,
원격 exact-head CI, 실제 provider smoke, 연구 task 평가, OS enforcement는 별도 증거다.
Unchanged evidence는 재사용하되 source/contract가 달라지면 다시 확인한다.

별도 승격 승인이 필요한 것은 새 dependency/설치 경로, interactive native control,
provider-home 접근 확대, 예산 수치·유료 평가, 원격/GPU 실행, default activation,
automatic publication이다. 모델 호출 없이 결정적 fixture를 구현하는 작업과 이러한
운영 승격을 같은 승인으로 묶지 않는다.
