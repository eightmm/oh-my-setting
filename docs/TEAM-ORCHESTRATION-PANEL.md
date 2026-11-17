# Team-aware panel: layout and interaction contract

작성: 2026-11-17. **DESIGN ONLY / runtime UI 미구현**.
Source baseline: `de2f547c6a40ddf67c62068bce309050c6d4fa48`.
이 문서는 [#245](https://github.com/eightmm/oh-my-setting/pull/245)의 TM-06과
[#248](https://github.com/eightmm/oh-my-setting/pull/248)의 관측·검증 명세를 구체화한다.
새 Main, scheduler, 상태 DB, web frontend, 모델 호출 또는 기본값 변경을 추가하지 않는다.
Main 선택은 [별도 정책](TEAM-ORCHESTRATION-MAIN-POLICY.md)을 따른다.
목업의 팀·숫자·상태는 설명용이며 실제 실행 결과가 아니다.

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
Legacy worker는 'Direct workers' 그룹으로 구분하되 기존 task/alias를 유지한다.

## 2. 정보 구조

| 표면 | 항상 먼저 보여 줄 것 | 상세에서만 보여 줄 것 |
|---|---|---|
| Project/goal | repo, branch, 짧은 goal, snapshot freshness | 전체 goal, source digest, PR/CI의 관측 범위 |
| Main | 유일한 owner, harness, 선택 model, 상태 | served model, account alias, effort, session generation, swap policy |
| Needs you | 사람이 결정해야 하는 항목 수·이유 | 정확한 승인 대상·범위·만료·영향 |
| Main working | Main 권한 안에서 처리할 검증/복구/배정 | 다음 행동, 근거, block dependency |
| Team cards | task 이름, mode, 상태/막힌 이유, 다음 주체 | lead/member, attempts, scope, context, usage, dependencies |
| Results | candidate/verification/review/admission/publication의 구분 | exact base/patch/composed tree 및 receipts |
| Resource footer | 실행/검토 backlog, budget coverage, 관측 이상 | 계정별 window, source age, inclusive/partial usage |

Team 이름은 'Codex 팀' 같은 provider 중심 이름보다 'Parser 변경', '회귀 검증'처럼
**결과 책임**을 나타낸다. 한 팀이 여러 provider를 포함할 수 있다. Provider는
member 속성이며 전역 소유권이나 reviewer 독립성의 증거가 아니다.

## 3. Work 화면 목업

아래는 정보 배치 예시다. 고정 terminal-size snapshot이나 새 split 비율이 아니다.

```text
OMS / project     branch: feature/...    PR: draft    CI: observed @ SHA
Goal: Implement the approved change       snapshot: fresh
+----------------------------+-----------------------------------------+
| MAIN                       | NATIVE CONVERSATION                     |
| codex / GPT-6.1 Sol        |                                         |
| selected: high             | Existing Codex / Claude / AGY CLI       |
| state: waiting for result  | Input, permissions and history remain   |
|                            | native.                                 |
| NEEDS YOU  1               |                                         |
| ! T2 scope approval        | Selecting a team does not send input,   |
|                            | resume a session or wake a model.       |
| MAIN WORKING  1            |                                         |
| T1 independent review      |                                         |
|                            |                                         |
| TEAMS                      |                                         |
| > T1 Parser change         |                                         |
|   oms-team / review ready  |                                         |
|   next: independent review |                                         |
| v T2 Regression checks     |                                         |
|   oms-team / blocked       |                                         |
|   L2 lead      claude      |                                         |
|   W3 test      codex       |                                         |
|   W4 research  claude      |                                         |
| > T3 Native investigation  |                                         |
|   native-team / partial    |                                         |
|   children: unreported     |                                         |
|                            |                                         |
| HISTORY  4 [collapsed]     |                                         |
| writes 2/6; review wait 1  |                                         |
| usage: partial             |                                         |
+----------------------------+-----------------------------------------+
F5 control   F9 board/chat   F12 help     Enter inspect/expand
```

Team card는 기본적으로 2~3행의 task/state/reason 요약으로 제한한다. 긴 모델명,
모든 member 로그, 모든 quota window는 상세로 내린다. Main의 선택 모델과 실제 실행 모델이
다르면 명시적 표시를 붙이고, served model이 없으면 selected를 served로 복사하지 않는다.

'writes 2/6'은 설명용이다. 현재 panel의 effective cap, orchestrator skill의 WIP 지침,
그리고 TM-02의 canonical admission이 서로 다른 범위이면 각각 출처를 표시한다.
UI가 상수를 새로 정하거나 advisory count를 hard enforcement라고 표현하지 않는다.
Required member 결과가 없는데 team을 complete로 표시하지 않는다.

## 4. Team inspector와 review 흐름

Team을 선택하면 기존 reader/overlay에서 열린다. Native CLI를 자동으로 숨기거나
축소하지 않으며 사용자가 reader/Focus를 선택했을 때만 해당 host의 기존 배치를 쓴다.

```text
T1 Parser change       oms-team       root owner: M1 / generation 7
State: candidate ready             Next actor: independent reviewer

Summary | Members | Changes | Evidence | Messages | Usage

Required work: implementation ready; regression evidence missing
Members: L1 lead, W1 builder, W2 tester, R1 reviewer
Scope: declared paths only         Mode: OMS-managed
Candidate: base + patch digest     Combined tree: not verified

Evidence
  process exit       confirmed
  child cleanup      confirmed
  verify             missing on combined tree
  independent review pending
  admission          not attempted
  publication        not authorized

Actions: Inspect evidence | Open bound session | Request repair
Unavailable: Accept (combined-tree proof missing)
```

탭 이름은 정보 그룹이지 기존 Results/Events/Diff/Transcript 탭을 폐기하는 새 reader
구현 요구가 아니다. 기존 탭/reader에 매핑하고 필요할 때만 additive grouping을 한다.
Main/Team/Member 선택은 모두 같은 inspector identity를 사용한다. Diff의 finding에서
criterion/test로 이동한 뒤 원래 위치로 돌아올 수 있어야 한다.

Member는 `managed`, `observed`, `unreported` coverage를 분리한다. Native child가 보이지
않으면 '0명'이라고 하지 않는다. Observed node를 새 room participant로 등록하거나
제어 권한을 부여하지 않는다. 메시지 관계와 task dependency DAG는 별도 보기로 남긴다.

## 5. 입력·포커스·반응형 동작

[기존 panel keys](TERMINAL-PANEL.md)의 F5/F9/F12, Tab/Enter/Esc와 native CLI key ownership을
유지한다. Board focus에서만 기존 메뉴를 통해 team action을 선택한다. 새 전역 hotkey나
새 공개 CLI flag를 이 명세에서 확정하지 않는다.

- Refresh, worker 종료, attention 발생, team 정렬이 입력 focus나 선택 identity를 빼앗지 않는다.
- Enter의 기본 동작은 읽기/펼치기다. Session attach/resume/send/wakeup은 별도의 명시적 action이다.
- Cancel/hold/continue/repair/swap은 target identity/generation과 영향·비용을 보여 준 뒤
  기존 parent front door로 요청한다. 열람/ack는 실행·승인·결과 수락이 아니다.
- Ctrl-C는 현재 focus의 기존 의미를 유지한다. Main CLI interrupt가 모든 team cancel로
  확장되지 않으며, team cancel 요청도 confirmed termination으로 즉시 표시하지 않는다.
- 좁은 폭에서는 summary/tree와 기존 overlay/zoom으로 축소한다. 숨긴 member/row 수를
  표시하고 가로로 잘려 actionable reason이 사라지지 않게 한다.
- 사용자 split과 scroll을 보존한다. Reader를 닫으면 원래 pane/focus/draft로 돌아간다.
- CJK display width, combining/emoji, multiline, ANSI/control/bidi 문자와 긴 label을 안전하게
  처리한다. ASCII/no-color fallback과 기존 append-only log 접근을 유지한다.

동일 task/cause/generation의 경고는 하나로 묶고 affected members를 펼친다. 필터가
active이면 숨긴 수를 표시한다. 읽음 표시는 blocker 해결이 아니다.

## 6. 상태와 조작의 권한 경계

Team summary는 execution, evidence, cleanup, spending의 직교 상태를 요약한다.
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

## 7. Rust 구현 분할: TM-06 내부의 세 개 slice

새 orchestration engine을 만드는 작업이 아니다. 각 slice에서 가장 가까운 기존
projection/layout/navigation/results/routing fixture를 확장하며 새 테스트 트리를 복제하지 않는다.

| Slice | Scope | 선행 조건 | 완료 기준 |
|---|---|---|---|
| TM-06A | team-aware read projection, collapsed task cards, inspector 연결 | TM-01 schema/identity; absent field는 legacy fallback | view만 읽었을 때 state/model/network mutation 0; stable selection |
| TM-06B | capability-aware action preview, exact-target dispatch, swap 표시 | TM-02~05와 해당 adapter action 계약 | stale/observed target 차단, explicit request와 receipt 구분 |
| TM-06C | bounded refresh, responsive snapshots, accessibility, rollout | A/B 및 기존 RF-03 관측 producer | 아래 fixtures 통과, I/O baseline 비교, 기존 single 회귀 없음 |

조사한 재사용 anchor: `crates/oms-command-panel/src/layout.rs`,
`crates/oms-command-panel/src/navigation.rs`, `crates/oms-panel-core`,
`crates/oms-command-room`, 기존 panel/dashboard/results와 native host adapter.
Layout source는 직접 확인했다. 나머지 변경 함수·suite는 구현 시 현재 main에서 확인하고
path ownership을 분리한다. `.oms` 상태를 직접 편집하거나 새 authority writer를 만들지 않는다.

## 8. TM-06 acceptance: 기존 TV-19를 구체화하는 UX fixtures

| ID | Fixture | 필수 결과 |
|---|---|---|
| TP-01 | feature off / legacy direct workers | 기존 구조·키·JSON null 의미 보존 |
| TP-02 | 1 Main, 3 teams, mixed-provider members | task-first grouping; Main 하나; 명시 관계만 연결 |
| TP-03 | observed native nodes, missing child telemetry | control 불가; partial/unreported 표시; 0명으로 보정 금지 |
| TP-04 | live completion/reorder/filter while typing | native draft/focus/selection 유지; hidden count 표시 |
| TP-05 | 200x50, 120x30, 80x24, 40x12, 19-column board | overflow 없음; CJK/no-color; 기존 fallback 및 omitted count |
| TP-06 | request ack loss, cancel/complete race | requested/unknown/confirmed 분리; blind retry 금지 |
| TP-07 | individually passing patches, combined failure | verified/accepted/landed를 하나로 표시하지 않음 |
| TP-08 | inclusive usage, unknown usage, stale quota | double count 없음; source/age/coverage; unknown은 0 아님 |
| TP-09 | main swap, dead pane, changed generation | 새 owner 확인 전 input/control 거절; worker를 재생성하지 않음 |
| TP-10 | resumed native model/fallback differs from selected | served attribution 보존; reviewer independence 재평가 |
| TP-11 | rotation/truncate/partial tail; two panels | bounded incremental read; 기존 I/O 계측으로 중복 scan 확인 |
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
수행한 것은 아니다. 실제 TUI 렌더링, Rust compile, provider session, 모델 비교 평가와
성능 측정은 아직 수행하지 않았다. 게시 PR에서 실제 실행한 문서/CI 검증을 별도 기록한다.
