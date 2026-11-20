# Overall Main panel: work, sessions and interaction contract

작성: 2026-11-17. **설계와 TM-06A 선택형 읽기 화면 / 운영 승격 없음**.
Source baseline: `de2f547c6a40ddf67c62068bce309050c6d4fa48`.
이 문서는 [#245](https://github.com/eightmm/oh-my-setting/pull/245)의 TM-06과
[#248](https://github.com/eightmm/oh-my-setting/pull/248)의 관측·검증 명세를 구체화한다.
새 Main, scheduler, 상태 DB, web frontend, 모델 호출 또는 기본값 변경을 추가하지 않는다.
Main 선택은 [별도 정책](TEAM-ORCHESTRATION-MAIN-POLICY.md)을 따른다.
목업의 팀·숫자·상태는 설명용이며 실제 실행 결과가 아니다.

2026-11-18 구현 범위: `oms panel --teams [--json]`은 기존 Plan의 `team-list`,
resource snapshot, physical intent reader를 사용하는 선택형 읽기 화면이다.
Root owner/generation → Team ID/generation → logical lead/member를 보여 주며
같은 label도 ID로 구분한다. Held 이유와 source 오류·미검증·omitted 범위를 남긴다.
Spec은 실행 증거가 아니며 retained intent도 running count가 아니다. Served/cleanup/
usage/native children의 미보고 값을 0이나 성공으로 채우지 않는다. 기존 Reader에
연결하며 default panel·키·action은 유지한다. 이는 TM-06A의 로컬 fixture 범위다.
TM-06B 조작, TM-06C live refresh/I/O 비교, 실제 native terminal smoke는 별도다.

2026-11-19 UI 연결 명세는 기준 `7d9b291`, 이번 Auto 표시 정정, 후속 목표를 구분한다.

| 범위 | 동작과 한계 |
|---|---|
| 기준 `7d9b291` | `render_provider_rows`가 provider별 Orchestrator와 calls를 묶음; `--teams`는 별도 읽기 화면 |
| 현재 Auto overview | Room membership과 lifecycle parent attempt가 현재 Room의 live owner를 가리키는 활성 관리 작업을 `Overall Main · Direct work`에 표시. 나머지는 provider activity 영역에 표시하며 연결된 pane의 표시는 `Lead chat [1/2/3]`이다. |
| 후속 TM-06A/B/C·P0-B | task-first 공통 카드/inspector, native assignment/result binding과 지원 action 연결; 이번 render 정정만으로 완료되지 않음 |

현재 Auto overview는 기존 key/mouse target, 좁은 폭 fallback,
dispatch·소유권·task/Team 계약을 사용한다.
[#286](https://github.com/eightmm/oh-my-setting/pull/286)의 Main picker와
[#284](https://github.com/eightmm/oh-my-setting/pull/284)의 조건부 native 분업 프롬프트는 별도 변경이다.
Native assignment를 canonical task·attempt·결과에 연결하는
[#285의 P0-B](https://github.com/eightmm/oh-my-setting/pull/285)는 bridge 설계다.
현재 `--orchestrator --task`의 텍스트 전송을 이 bridge나 managed Team 실행으로 간주하지 않는다.

## 1. 무엇을 유지하고 바꾸는가

기본 흐름은 **왼쪽 OMS overview / 오른쪽 기존 native CLI**다. 팀이 생겼다고
Codex·Claude·AGY별 대화 pane을 항상 세 개 열거나 native 대화를 재구현하지 않는다.
질문은 네 가지로 좁힌다: 무엇을 끝내려 하는가, 어느 팀이 무엇을 하는가,
왜 막혔고 다음 주체는 누구인가, 어떤 증거로 결과를 받아들일 수 있는가.

[기존 UX](CONTROL-PLANE-UX-AND-FEATURES.md)의 Work/Overview/Focus,
Needs you, 선택형 inspector, context/evidence reader를 재사용한다.
[현재 layout source](../crates/oms-command-panel/src/layout.rs)의 sidebar 초기값 28%,
board 최소 20 cells, native 최소 40 cells와 저장된 split을 기준선으로 보존한다.
새 비율·breakpoint·키맵을 임의로 전역 적용하지 않는다. 실제 터미널 검증 후 필요한
수치 변경은 별도 근거를 갖춘 구현 diff로 제출한다.

Team record가 없거나 feature가 꺼져 있으면 기존 single/flat-worker 화면을 유지한다.
새 record를 읽지 못하면 unsupported/partial을 보여 주며 가짜 Team을 만들지 않는다.
목표 화면의 direct worker는 기록된 Main parent 아래에 두고 기존 task/alias를 유지한다.
Parent가 불명확한 legacy row는 unbound로 남기며, provider가 같다는 이유로 Lead 아래에 넣지 않는다.

사용자는 Overall Main을 Codex·Claude·AGY 중에서 선택하며 room/integration target의 Main은
하나다. Provider는 session 이동과 filter의 축이고 parent 관계가 아니다. 목표 관계는 다음과 같다.

| 대상 | 실행 책임과 실제 관계 | 작업 목록과 세션에 표시할 것 |
|---|---|---|
| Main의 직접 작업 | Overall Main이 수행 | Main 처리 중인 task; 가짜 Worker/Lead를 만들지 않음 |
| Direct Worker / reviewer | Overall Main이 기존 parent-only dispatch로 배정 | Main 아래 해당 task와 attempt; provider는 속성 |
| Native Provider Lead | Overall Main에게 받은 범위의 지속 native session | 배정된 task와 bound session; Lead가 직접 완료할 수도 있음 |
| Native child | 지원되는 native surface가 입증한 Lead/child 관계 | 해당 Lead의 task 상세; 관계·실행 관측이 없으면 unreported |
| Managed Team member | 기존 TeamSpec과 enrollment가 입증한 관계 | 해당 task의 Team 상세; native pane을 이 관계로 자동 승격하지 않음 |

일반 요청마다 Lead/direct 중 하나를 사용자에게 고르게 하지 않는다. Main은 기존 대화 맥락,
격리·독립 검증·결과 계약과 지원 capability에 맞춰 경로를 고른다. 작은 일은 Main이나
기존 Lead가 직접 끝낼 수 있다. '단순 작업이면 direct' 또는 '항상 Lead를 경유'를 강제하지 않는다.
OMS worker 배정은 Overall Main의 parent front door가 담당한다. Lead가 추가 OMS 작업을
필요로 하면 Main에 요청하고, 직접 OMS worker를 재귀 dispatch하지 않는다. 허용된 native
child 사용은 이 OMS 배정 권한과 별개이며 inherited scope와 승인 경계를 유지한다.

## 2. 정보 구조

| 표면 | 항상 먼저 보여 줄 것 | 상세에서만 보여 줄 것 |
|---|---|---|
| Project/goal | repo, branch, 짧은 goal, snapshot freshness | 전체 goal, source digest, PR/CI의 관측 범위 |
| Main | 유일한 owner, harness, 선택 model, 상태 | served model, account alias, effort, session generation, swap policy |
| Needs you | 사람이 결정해야 하는 항목 수·이유 | 정확한 승인 대상·범위·만료·영향 |
| Main working | Main 권한 안에서 처리할 검증/복구/배정 | 다음 행동, 근거, block dependency |
| Task cards | task 이름, 실제 담당, 상태/막힌 이유, 다음 주체 | mode, lead/member, attempts, scope, context, usage, dependencies |
| Session shortcuts | Overall Main과 연결된 Codex·Claude·AGY session | target identity/generation, 연결된 task; 미배정 대화는 unbound session |
| Results | candidate/verification/review/admission/publication의 구분 | exact base/patch/composed tree 및 receipts |
| Resource footer | 실행/검토 backlog, budget coverage, 관측 이상 | 계정별 window, source age, inclusive/partial usage |

Task/Team 이름은 'Codex 팀' 같은 provider 중심 이름보다 'Parser 변경', '회귀 검증'처럼
**결과 책임**을 나타낸다. 한 팀이 여러 provider를 포함할 수 있다. Provider는
member 속성이며 전역 소유권이나 reviewer 독립성의 증거가 아니다.

## 3. Work 화면 목업

아래는 목표 정보 배치다. 새 split 비율이나 구현된 terminal snapshot이 아니다.

| 왼쪽 OMS의 순서 | 표시 예시와 의미 | 오른쪽 기존 native CLI |
|---|---|---|
| Goal / Main | project·branch·freshness; 유일한 Main과 선택 model | 사용자가 선택한 Main/Provider session의 입력·권한·history 유지 |
| Needs you | T2 범위 승인 필요 | 목록 갱신이나 task 선택은 입력을 전송하지 않음 |
| Main working | T1 검증 실패 원인 확인 | Main의 직접 작업도 같은 task 흐름에서 표시 |
| Tasks | T2 Parser 변경 — Direct Worker, review 대기 | task 선택은 inspector; 세션 이동은 명시적인 별도 action |
| Tasks | T3 조사 — Claude Lead, native children unreported | 현재 대화의 draft/focus는 자동으로 바꾸지 않음 |
| Sessions | Overall Main / 연결된 Codex·Claude·AGY | 기존 세션으로 이동; 미연결 provider를 자동 시작하지 않음 |
| History / Resources | 접힌 과거 task 수·review backlog·usage coverage | 원본 로그와 기존 reader에 계속 접근 가능 |

Task card는 기본적으로 2~3행의 task/state/reason 요약으로 제한한다. 긴 모델명,
모든 member 로그, 모든 quota window는 상세로 내린다. Main의 선택 모델과 실제 실행 모델이
다르면 명시적 표시를 붙이고, served model이 없으면 selected를 served로 복사하지 않는다.

현재 panel의 effective cap, orchestrator skill의 WIP 지침,
그리고 TM-02의 canonical admission이 서로 다른 범위이면 각각 출처를 표시한다.
UI가 상수를 새로 정하거나 advisory count를 hard enforcement라고 표현하지 않는다.
Required member 결과가 없는데 team을 complete로 표시하지 않는다.

## 4. 공통 task inspector와 실행 연결

Task를 선택하면 기존 reader/overlay에서 열린다. Native CLI를 자동으로 숨기거나
축소하지 않으며 사용자가 reader/Focus를 선택했을 때만 해당 host의 기존 배치를 쓴다.

| 연결 대상 | 필요한 기존 identity / evidence | 읽기와 조작의 경계 |
|---|---|---|
| Task card | state repo, plan/task, 현재 owner·lease; criterion과 scope | task 이름·provider로 다른 작업을 합치지 않음 |
| Room / attempt | Room lineage·participant, parent/attempt ID와 generation | 메시지·배정·실행 이력을 같은 task에서 열되 ack를 승인으로 쓰지 않음 |
| Native session | 검증된 host/session·pane/process generation과 assignment binding | '담당 세션 열기'의 정확한 target; binding이 없으면 unbound session |
| Candidate / Changes | candidate attempt, base·patch digest, 실제 worktree | 같은 task의 새 candidate도 이전 검증을 자동 승계하지 않음 |
| Evidence / Results | verifier·review·admission·publication의 exact references | process exit, cleanup, verified, accepted, published를 각각 표시 |
| Supported actions | 현재 backend capability·authority와 위 target snapshot | 지원되는 inspect/open/continue/repair/cancel만 제공; 미지원 이유 표시 |

예를 들어 candidate가 있어도 combined-tree verifier와 독립 review가 없으면 결과를
열어 볼 수 있을 뿐 Accept는 사용할 수 없다. 지원되는 session open도 send/resume과 별개다.

일반 native 대화는 task를 만들지 않고 unbound session으로 유지한다. Durable assignment와
결과 수락이 필요한 작업만 #285 P0-B의 기존 Plan/operations/Room/result writer에 연결한다.
이 bridge가 구현되기 전에는 텍스트 전송, pane 존재, 자연어 완료 보고를 task claim·소비 확인·
검증된 결과로 채우지 않는다. UI는 기존 reference를 소비하며 새 assignment 원장을 쓰지 않는다.

탭 이름은 정보 그룹이지 기존 Results/Events/Diff/Transcript 탭을 폐기하는 새 reader
구현 요구가 아니다. 기존 탭/reader에 매핑하고 필요할 때만 additive grouping을 한다.
Main 직접 작업, direct worker, Lead/Team은 같은 task inspector를 사용하고 실제 parent와
attempt는 상세에서 구분한다. Provider filter와 session 전환은 task identity를 바꾸지 않는다.
Diff의 finding에서 criterion/test로 이동한 뒤 원래 위치로 돌아올 수 있어야 한다.

Member는 `managed`, `observed`, `unreported` coverage를 분리한다. Native child가 보이지
않으면 '0명'이라고 하지 않는다. Observed node를 새 room participant로 등록하거나
제어 권한을 부여하지 않는다. 메시지 관계와 task dependency DAG는 별도 보기로 남긴다.

## 5. 입력·포커스·반응형 동작

[기존 panel keys](TERMINAL-PANEL.md)의 F5/F9/F12, Tab/Enter/Esc와 native CLI key ownership을
유지한다. Board focus에서만 기존 메뉴를 통해 team action을 선택한다. 새 전역 hotkey나
새 공개 CLI flag를 이 명세에서 확정하지 않는다.
아래는 후속 task inspector의 목표이며 이번 Auto 표시 정정은 기존 Enter/docking 동작을 유지한다.

- Refresh, worker 종료, attention 발생, task 정렬이 입력 focus나 선택 identity를 빼앗지 않는다.
- Enter의 기본 동작은 읽기/펼치기다. Session attach/resume/send/wakeup은 별도의 명시적 action이다.
- Cancel/hold/continue/repair/swap은 target identity/generation과 영향·비용을 보여 준 뒤
  기존 parent front door로 요청한다. 열람/ack는 실행·승인·결과 수락이 아니다.
- Ctrl-C는 현재 focus의 기존 의미를 유지한다. Main CLI interrupt가 모든 team cancel로
  확장되지 않으며, team cancel 요청도 confirmed termination으로 즉시 표시하지 않는다.
- 키보드와 mouse hit target은 같은 task/session identity를 가리킨다. 재정렬·접기 뒤
  이전 행 좌표로 다른 대상에 action을 실행하지 않고 선택 target을 다시 확인한다.
- 좁은 폭에서는 summary/tree와 기존 overlay/zoom으로 축소한다. 숨긴 member/row 수를
  표시하고 가로로 잘려 actionable reason이 사라지지 않게 한다.
- 사용자 split과 scroll을 보존한다. Reader를 닫으면 원래 pane/focus/draft로 돌아간다.
- CJK display width, combining/emoji, multiline, ANSI/control/bidi 문자와 긴 label을 안전하게
  처리한다. ASCII/no-color fallback과 기존 append-only log 접근을 유지한다.

동일 task/cause/generation의 경고는 하나로 묶고 affected members를 펼친다. 필터가
active이면 숨긴 수를 표시한다. 읽음 표시는 blocker 해결이 아니다.

## 6. 상태와 조작의 권한 경계

Task/Team summary는 execution, evidence, cleanup, spending의 직교 상태를 요약한다.
`quiet`, `waiting dependency`, `waiting permission`, `rate limited`, `disconnected`,
`cleanup unknown`, `verify failed`를 하나의 stalled/failed로 뭉개지 않는다.
진행률은 분모가 확인된 acceptance 기준 수만 표시하며 task 수로 ETA를 계산하지 않는다.

Action availability는 backend의 현재 capability/authority 결과에서 가져온다. UI가
provider 이름만 보고 지원 여부를 추정하지 않는다. 선택 후 실행 직전에 root owner,
team/member generation, policy, scope, candidate/base와 capability를 다시 검증한다.
Snapshot은 approval이 아니며 stale snapshot의 mutation은 거절한다.

Pause는 '새 admission 중지'인지 '실행 중 agent 정지'인지 정확히 표기한다. 해당 backend가
지원하지 않는 pause/stop/resume은 이유와 함께 disabled다. 불명확한 send/launch/cancel
응답을 성공으로 꾸미거나 자동 재전송하지 않는다. 실패/중단 worktree는 보존한다.
Native assignment의 전송 결과가 불명확하면 direct Worker를 새로 띄워 같은 일을 대신
실행하지 않는다. 기존 intent/receipt와 target을 먼저 reconcile하며 경로 변경도 Main의
기존 권한과 scope·계정·비용 조건을 보존해야 한다.

## 7. Rust 구현 분할: TM-06 내부의 세 개 slice

새 orchestration engine을 만드는 작업이 아니다. 각 slice에서 가장 가까운 기존
projection/layout/navigation/results/routing fixture를 확장하며 새 테스트 트리를 복제하지 않는다.

| Slice | Scope | 선행 조건 | 완료 기준 |
|---|---|---|---|
| TM-06A | 기존 read projection에서 실제 parent별 task cards, 공통 inspector와 session 이동 연결 | TM-01 및 기존 task/attempt binding; native 미배정 대화는 unbound | direct Worker는 확인된 Main 아래; provider로 parent를 추정하지 않음; 읽기 mutation 0 |
| TM-06B | capability-aware action preview, exact-target dispatch, swap 표시 | TM-02~05; native assignment/result는 #285 P0-B와 해당 adapter 계약 | stale/observed target 차단; request·delivery·consumption·result 구분; 불명 전송의 fallback 중복 없음 |
| TM-06C | bounded refresh, responsive snapshots, accessibility, rollout | A/B 및 기존 RF-03 관측 producer | 아래 fixtures 통과, I/O baseline 비교, 기존 single 회귀 없음 |

TM-06A의 기존 `--teams` reader, 이번 Auto parent 표시 정정과 위 목표의 완료는 별개다.
새 queue/engine을 추가하지 않고 기존 TM-06A/B/C와 P0-B의 owner에서 caller를 연결한다.
재사용 anchor는
[render](../crates/oms-command-panel/src/render.rs)의 `calls`/`render_provider_rows`/`teams`,
[watch](../crates/oms-command-panel/src/watch.rs)의 `call_identity`/provider·worker hit target,
[orchestrator](../crates/oms-command-panel/src/orchestrator.rs)의 `Panel`/`run`,
[dispatch](../crates/oms-panel-core/src/dispatch.rs)의 `DispatchRequest`와 기존 Room/result reader다.
`.oms` 상태를 직접 편집하거나 새 authority writer를 만들지 않는다.

## 8. TM-06 acceptance: 기존 TV-19를 구체화하는 UX fixtures

| ID | Fixture | 필수 결과 |
|---|---|---|
| TP-01 | feature off / legacy direct workers / missing Team record | 기존 키·task/alias·JSON null 보존; provider만으로 Lead/Team을 합성하지 않음 |
| TP-02 | Main 직접 작업, direct Worker, 같은 provider의 Lead와 다른 provider의 native child, 이전 Main attempt 또는 다른 Room | 현재 Room의 live owner에 연결된 Room membership과 lifecycle parent attempt가 모두 확인될 때 direct work에 표시 |
| TP-03 | unbound native 대화 / bridge 없음 / missing child telemetry | 대화를 task로 만들지 않음; 관측만으로 control 허용 안 함; partial/unreported를 0이나 성공으로 보정 금지 |
| TP-04 | live completion/reorder/filter/collapse while typing; keyboard·mouse 선택 | task/session identity·draft/focus/scroll 유지; stale hit target 거절; hidden count 표시 |
| TP-05 | 200x50, 120x30, 80x24, 40x12, 19-column board; CJK/emoji/no-color | overflow·column drift 없음; 표시된 행과 mouse target 일치; fallback 및 omitted count 유지 |
| TP-06 | request ack loss, cancel/complete race | requested/unknown/confirmed 분리; blind retry·direct fallback 중복 실행 금지 |
| TP-07 | individually passing patches, combined failure; candidate 교체 | exact candidate/evidence 연결; stale proof 수락 거절; verified/accepted/landed 구분 |
| TP-08 | inclusive usage, unknown usage, stale quota | double count 없음; source/age/coverage; unknown은 0 아님 |
| TP-09 | main swap, dead pane, changed generation | 새 owner 확인 전 input/control 거절; worker를 재생성하지 않음 |
| TP-10 | resumed native model/fallback differs from selected | served attribution 보존; reviewer independence 재평가 |
| TP-11 | rotation/truncate/partial tail; two panels; 단순 조회·세션 이동 | bounded incremental read; 기존 I/O 계측으로 중복 scan 확인; 모델 호출·전송·재개를 암묵 실행하지 않음 |
| TP-12 | team label containing terminal escapes/private material | 안전한 렌더링·redaction; raw prompt를 footer에 노출하지 않음 |

이 표는 완료 조건이며 이 문서 PR에서 실행한 테스트가 아니다.
Render snapshots는 fake clock/fixtures로 재현하고 실제 native terminal smoke는 별도로 기록한다.
Cold/warm refresh의 bytes, metadata calls, subprocesses, RSS, p50/p95를 같은 환경에서
비교한다. 변경 없는 화면에서 모델 호출·전체 transcript 재파싱·새 state write는 금지한다.

## 9. 도입과 rollback

[#248](https://github.com/eightmm/oh-my-setting/pull/248)의 G0~G5를 그대로 따른다.
처음에는 feature-off + synthetic/read-only projection이며 native 실행을 UI 개발과 묶지 않는다.
UI rollout 중 문제가 생기면 legacy view로 되돌리되 team records, pending approvals,
cleanup responsibility, evidence와 dirty worktree는 보존한다. 숨겨진 team 때문에
resource ownership이 해제되거나 worker가 종료된 것으로 처리되어서는 안 된다.

## 10. 이번 문서의 검증 범위

Pinned source의 layout constants, 현재 panel 설명, orchestration skill, #245/#248의
관련 계약 및 기존 UX 명세의 해당 부분을 정적으로 대조했다. 전체 runtime 감사를
수행한 것은 아니다. 이후 TM-06A 선택형 reader/CLI는 로컬 Rust fixture로 별도 검증한다.
2026-11-19 추가 명세는 기준 `7d9b291`의 provider grouping, session 전환, direct dispatch와
native task 전송 caller를 대조했다. 현재 Auto overview의 parent 표시는 panel render
fixture로 검증한다. 공통 task inspector와 P0-B bridge는 후속 구현 범위다.
Provider session, 실제 native terminal smoke, 모델 비교 평가와 성능 측정은 이 구현의
검증 범위가 아니다. 게시 PR에서 실제 실행한 검사와 exact-head CI를 별도 기록한다.
