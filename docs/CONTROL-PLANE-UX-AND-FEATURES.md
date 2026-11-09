# OMS UI/UX 비교와 신기능 설계

검토일: 2026-11-09 (KST). 상태: **제안 / 문서 전용 / 미구현**.
[확장 계획](CONTROL-PLANE-EVOLUTION.md)의 CP-01–18 및 72개 수락 조건을 보존하고,
UX-01–06과 NF-01–06의 세부 설계·48개 수락 조건을 추가한다.
이 12개 명세는 12개 독립 엔진이나 자동 구현 승인이 아니다.

원 계획 기준은 `20ce1be`; 이번 확인의 main은
[`3979588`](https://github.com/eightmm/oh-my-setting/commit/39795883459e902d1b5f1f0514e229b834fd3219)다.
PR #14의 기존 head `b566497` 위에 문서만 추가하며 main 코드나 다른 작업을 재작성하지 않는다.
외부 제품의 공개 화면·공식 문서와 OMS의 관련 문서/인터페이스를 비교했다.
외부 앱 설치, 실제 OMS 조작, 사용자 실험, 성능 benchmark는 수행하지 않았다.
아래 불편은 확인된 사용자 결함 신고가 아니라 **검증할 UX 가설**이다.

## 1. 제품 방향

목표는 더 많은 에이전트를 전시하는 화면이 아니라 **작업의 다음 판단을 돕는 관제 화면**이다.
질문은 다섯 가지다: 무엇이 진행됐나, 지금 누가 행동해야 하나, 왜 막혔나,
무슨 근거로 검토할 수 있나, 돌아왔을 때 어디서 이어야 하나.

단일 Main, 왼쪽 OMS/오른쪽 native CLI, 구독 기반 실행을 유지한다.
CLI 대화를 재렌더링하지 않고 raw transcript를 매번 모델에 넣지 않는다.
새 기능의 첫 버전은 기존 상태를 읽는 파생 뷰이며 기본 추가 추론은 0회로 설계한다.
이는 비용 절감을 측정했다는 뜻이 아니다. 변경 행동은 기존 front door와 권한을 따른다.

## 2. 추가 비교: 기능 수보다 사용 흐름

아래는 검토 시 공개 문서에 적힌 동작이다. 결과의 품질·안정성을 직접 검증한 것은 아니다.
개념을 참고하며 코드·UI asset을 복사하지 않는다. 재사용은 CP-18의 버전/라이선스 검토를 거친다.

| 참고 / 1차 출처 | 확인한 패턴 | OMS에 적용할 판단 | 그대로 가져오지 않을 것 |
|---|---|---|---|
| [cmux](https://github.com/manaflow-ai/cmux) | Attention ring, unread 목록, workspace 메타데이터 | 사건→해당 세션으로 이동; 입력 focus는 보존 | macOS terminal 재구현, 알림을 승인으로 간주 |
| [Paradev, 이전 Superset](https://github.com/ParadevAI/Paradev) | Worktree, Changes/PR/terminal을 연결; command palette | task→변경→검증→후속 요청을 한 흐름으로 | IDE 전체·브라우저·유료 원격 제품을 OMS 필수 기능화 |
| [Maestro](https://github.com/RunMaestro/Maestro), [release notes](https://github.com/RunMaestro/Maestro/releases) | Queue/Force Send/Queue Held/Steer 구분, draft 보존, quick actions | 전송 방식과 미지원 이유를 선택 전에 표시 | 자동 prompt 재전송·endpoint fallback·별도 chat renderer |
| [Jean](https://github.com/coollabsio/jean) | Session recap, finding 추적, line-comment diff, 최근 session | 복귀 요약과 검토 맥락 연결 | 모든 recap에 추론 호출, Full access 기본값 |
| [OpenChamber](https://github.com/openchamber/openchamber), [Multi-run](https://github.com/openchamber/openchamber/blob/main/packages/docs/content/docs/multi-run.mdx), [Walkthrough](https://github.com/openchamber/openchamber/blob/main/packages/docs/content/docs/walkthrough.mdx) | 여러 결과 비교·Fusion, 변경 walkthrough | 같은 기준의 결과 비교와 evidence-linked 검토 순서 | 기본 fan-out, 자동 결과 합성, OpenCode 의존성 |
| [workmux sidebar](https://workmux.dev/guide/sidebar/), [patch mode](https://workmux.dev/guide/dashboard/patch-mode/) | 지속 sidebar, named view/접기, diff 중심 작업 | 안정된 task 목록과 상세의 단계적 공개 | 자동 merge/cleanup, worktree를 sandbox로 오인 |
| [Agent Deck TUI](https://github.com/asheshgoplani/agent-deck/blob/main/skills/agent-deck/references/tui-reference.md) | 검색·필터, context inspector, session reference, queue | 짧은 reference 전달과 컨텍스트 출처 검사 | raw key 주입형 quick approval·YOLO·임의 transcript 복사 |
| [Lazygit keybindings](https://github.com/jesseduffield/lazygit/blob/master/docs/keybindings/Keybindings_en.md) | 현재 panel 기준 도움말, 목록→diff, 일관된 탐색 | 다음 행동을 보이는 위치에 배치; 기억할 키 최소화 | git undo를 agent 실행 전체의 rollback으로 표현 |
| [Zellij](https://zellij.dev/tutorials/basic-functionality/) | mode별 status bar, floating/stacked pane | focus/모드 표시, 필요한 세부정보만 열기 | tmux 교체, 모든 native CLI 단축키 선점 |
| [Worktrunk hooks](https://github.com/max-sixty/worktrunk/blob/main/skills/worktrunk/reference/hook.md) | hook preview, blocking pre-hook과 background post-hook 구분 | 실행 전 영향·조건을 읽기 전용으로 미리 설명 | 새 hook/workflow DSL과 별도 scheduler |

Superset URL은 이번 조회에서 Paradev로 redirect됐고 README도 이전 이름을 명시한다.
이름 변경을 기능 안정성이나 모든 배포판의 동일성 근거로 사용하지 않는다.
공개 workmux sidebar 화면에서는 왼쪽 목록/오른쪽 native 대화 패턴을 확인했다.
스크린샷은 정보 배치 근거일 뿐 실제 latency·키보드 접근성의 증거가 아니다.

## 3. 이미 있는 OMS 기능과 진짜 추가분

[현재 패널 문서](https://github.com/eightmm/oh-my-setting/blob/39795883459e902d1b5f1f0514e229b834fd3219/docs/TERMINAL-PANEL.md)의
Layout/Keys 구간에는 Work/Overview/Focus, resize hysteresis, 선택형 reader,
Needs you, Graph/Tree, 결과 검색, Shift-drag와 contextual help가 이미 있다.
[현재 runtime 문서](https://github.com/eightmm/oh-my-setting/blob/39795883459e902d1b5f1f0514e229b834fd3219/docs/OMS-RUNTIME.md)의
ContextManifest에는 source digest, 포함 이유, byte budget과 context debt가 이미 있다.
따라서 새 layout engine, context compiler, inbox, review engine을 만들지 않는다.

| 제안 | 기존 재사용 지점 | 추가 가치 |
|---|---|---|
| 판단 중심 목록 | inbox, task/plan, existing Attention | 사람/Main의 다음 행동 구분, task 단위 집계 |
| 검토 cockpit | Results/Events/Diff, evidence coverage | 변경과 criterion의 왕복·finding 위치 보존 |
| 복귀 요약 | session log, handoff, event/artifact index | 마지막 열람 이후 delta, 불완전한 이력 표시 |
| 막힌 이유 | runtime next, lease/review/acceptance 상태 | 원인→필요 조건→실행 주체의 설명 |
| Context inspector | ContextManifest | 포함/누락/반복/출처의 화면화; provider 전체 context로 과장하지 않음 |
| 결과 비교 | attempt/result, experiment/criterion records | 동일 조건 확인 후 옆으로 비교; 서로 다른 실험은 비교불가 표시 |
| 실행 전 preview | dispatch dry-run, scope/WIP/route/ownership 검사 | 실행 영향과 known overlap을 한 카드로; 정책 재구현 없음 |
| 관측 replay | bounded event/projection, 기존 synthetic fixture | 과거 화면 재구성과 redacted 재현 자료; 실행 재연 없음 |

## 4. 정보 구조와 조작 원칙

### Work 화면의 우선순위

좌측은 목표 한 줄, 사람이 결정할 항목, Main이 처리 중인 항목, 진행 task 순이다.
W1/W2/R1은 task 아래의 attempt로 펼친다. 하나의 실패가 다섯 행으로 반복되지 않게 한다.
Agent Graph는 기존 Overview에서 관계 분석에 사용하고, Work에서는 task의 현재 결과와
다음 행동을 우선하는 후보를 비교한다. 기본 화면 변경은 측정 후 별도 검토한다.

아래는 동작 예시이며 실제 실행 상태/새 구현 스크린샷이 아니다.

```text
OMS / demo-project       snapshot: fresh        Main: native CLI

사람의 결정 1               | native Claude / Codex / Pi conversation
  T7 범위 확대 필요          | 기존 입력·권한 요청·history 그대로
Main 처리 중 2              |
  T3 검증 실패 원인 확인     | 선택할 때만 여는 inspector:
  T4 독립 리뷰 대기          | 요약 / 변경 / 검증 근거 / 이력
진행 작업                   |
  T5 구현 중                | T3: code changed -> verify failed
  T6 선행 작업 대기          | 필요한 조건: current-tree verifier pass
완료/과거 8 [접힘]           | 다음 주체: Main (사용자 승인 아님)
```

현재 28% 초기 sidebar, 최소 pane 크기, Work/Overview/Focus와 F5/F8/F9/F12를
기준선으로 유지한다. 임의의 새 화면 비율을 전역 기본값으로 덮어쓰지 않는다.
좁은 화면에서는 두 개의 작은 reader를 더 만들기보다 기존 overlay/zoom을 사용한다.
Activity 갱신은 focus를 빼앗지 않으며 선택 행을 자동 정렬로 밀어내지 않는다.

시각 표시는 색+기호+텍스트를 함께 쓴다. `exited`, `verified`, `accepted`, `landed`를
하나의 초록색 Done으로 합치지 않는다. 진행률은 기준이 명시된 criterion 수만 사용하고,
작업 수를 소요시간 백분율로 바꾸지 않는다. Raw Log는 상세에서 접근 가능하게 남긴다.

## 5. UX 세부 명세

### UX-01 — 결정 중심 Action Inbox (CP-06/07/10)

`Needs you`를 새로 복제하지 않고 기존 항목에 주체·차단 이유·영향 범위·근거를 연결한다.
사람에게 권한이 필요한 경우만 사람 inbox로, Main 권한 내 복구는 Main 처리 항목으로 보인다.
우선순위는 알려진 심각도와 실제 차단 관계로 정하고 추정 완료시간은 만들지 않는다.

- [ ] 동일 task/원인/generation은 하나의 사건으로 모이고 관련 attempt는 펼쳐 볼 수 있다.
- [ ] 사람이 해야 할 결정, Main의 작업, 단순 관측 alert가 구별된다. 담당이 불명확하면 unknown이다.
- [ ] 열람/읽음/숨김은 resolution이나 approval이 아니며 blocker가 유지된다.
- [ ] 항목에서 이유와 근거를 여는 데 선택 후 최대 2회의 UI 행동을 목표로 검사한다. 승인 절차는 줄이지 않는다.

### UX-02 — 하나의 Task Inspector와 Review 흐름 (CP-06/10)

요약→변경→검증 근거→이력을 같은 task identity에서 연다. Diff의 finding에서 관련
criterion/test로 이동하고 다시 원래 줄로 돌아온다. 검토 순서는 기계적으로 연결된
경로·finding 기준이며 AI walkthrough는 별도 명시 요청일 때만 고려한다.

- [ ] 동일 task의 탭 이동·refresh·continuation 후 선택을 안정적으로 유지하고 교체된 결과는 표시한다.
- [ ] diff/base/candidate digest와 검증 당시 snapshot을 표시하고 stale proof로 수락을 제안하지 않는다.
- [ ] 본문/preview/필터가 잘렸으면 표시하며 원본 접근 경로를 제공한다.
- [ ] summary에서 검증 근거까지 최대 3회의 UI 행동을 목표로 하되 raw 결과와 기존 reader를 제거하지 않는다.

### UX-03 — 검색 가능한 명령·대상 선택 (CP-06/09)

Board에만 적용되는 palette에서 task, session, action을 검색한다.
사용 불가 action은 숨기기만 하지 말고 이유를 보여준다. 임의 shell 입력창이 아니다.
키는 충돌 감사 뒤 정하고 native CLI의 Ctrl+K/Tab/Esc를 전역으로 가로채지 않는다.

- [ ] 실행 전 repo/room/task/대상 세션과 읽기/변경/파괴 범위를 표시한다.
- [ ] action enablement는 기존 capability/authority 판단을 재사용하고 실행 직전 재확인한다.
- [ ] 키보드만으로 검색·선택·취소가 가능하며 Esc로 native 입력/미전송 문자가 손실되지 않는다.
- [ ] 기존 키는 유지하고 도움말·화면 footer·실제 binding을 같은 정의에서 검증한다.

### UX-04 — 안정된 Layout·Focus·접근성 (CP-06/15/16)

기존 세 모드의 전환을 명확히 표시하고, mode마다 목적에 맞는 정보 밀도를 사용한다.
Wide에서는 inspector, narrow에서는 overlay/zoom; 새 activity 때문에 window를 이동하지 않는다.

- [ ] 80x24, 120x40, 180x50 및 반복 resize에서 입력 focus·선택·scroll과 사용자의 pane 크기 의도가 보존된다.
- [ ] 한글/CJK·결합 문자·emoji·긴 경로에서 잘린 글자와 column drift를 검사한다. 색 없이도 상태가 구별된다.
- [ ] Shift-drag/복사/search/modal 종료를 검사하고 native CLI의 paste·selection과 충돌하지 않는다.
- [ ] 백그라운드 갱신이 선택 행을 이동시키거나 critical action의 대상 identity를 바꾸지 못한다.

### UX-05 — 전송 의도와 Draft 보존 (CP-02/07/09)

OMS가 이미 제공하는 전달 표면에서 Send/Queue/Steer/Stop을 구분한다.
Provider가 지원하지 않는 동작은 disabled+reason이며 send-keys로 흉내 내지 않는다.
Native composer를 복제하지 않고 OMS에서 작성한 draft만 보존한다.

- [ ] Queue는 다음 turn, Steer는 지원되는 진행 turn, Stop은 별도 권한 행동임을 명시한다.
- [ ] target/room 전환 때 draft를 자동 전송하지 않고 정확한 대상 identity로 다시 확인한다.
- [ ] acknowledgement가 없으면 delivery_unknown으로 남기고 무조건 재전송하지 않는다.
- [ ] browser/terminal 닫기와 draft 취소를 worker 취소로 해석하지 않는다. 영속 draft는 별도 privacy/retention 계약 없이는 만들지 않는다.

### UX-06 — 실행 전 지원 상태·출처 표시 (CP-01/08/17)

설치/auth/session observation/resume 등 capability를 간결한 상태로 먼저 보여준다.
모든 launch마다 긴 doctor를 강제하거나 자동 repair를 실행하지 않는다.
선택한 action의 disabled 이유와 필요한 최소 조치를 Main이 처리할 수 있게 연결한다.

- [ ] installed/authenticated/launchable/observable/resumable을 하나의 ready 배지로 합치지 않는다.
- [ ] cached/probed/reported/estimated/unknown과 마지막 확인 시각을 구별한다.
- [ ] 시작 안내와 오류 화면에 사용할 수 있는 경로가 있으며 사용자에게 명령 복사를 기본 요구하지 않는다.
- [ ] 단순 UI 열람이 모델 호출·로그인·설치·전체 HOME 탐색·네트워크 refresh를 유발하지 않는다.

## 6. 신기능 후보: 작은 첫 버전과 확장 경계

### NF-01 — Return Brief: 마지막 확인 이후 변경 요약

문제: 자리를 비웠다가 돌아오면 수십 개 로그를 다시 읽어야 한다.
첫 버전은 기존 event/receipt의 delta로 완료된 변경, 새 blocker, 필요한 결정, 남은 작업을
묶는다. `session-handoff`를 대체하지 않고 사람이 복귀할 때 읽는 view다.
기본 추가 모델 호출 없음. UI bookmark는 authority state가 아니며 처음에는 memory-only로 둔다.

- [ ] 새 사용자/새 process/retention gap이면 비교 기준이 없거나 불완전함을 명시한다.
- [ ] 내용마다 event/attempt/criterion 근거를 열 수 있고 worker exit를 accepted로 요약하지 않는다.
- [ ] room/plan 교체와 out-of-order event를 구분하며 같은 사건을 중복 집계하지 않는다.
- [ ] 읽음 처리·bookmark 변경이 task 상태나 다른 사용자의 판단을 바꾸지 않는다.

### NF-02 — Why Blocked: 원인과 다음 조치 설명

문제: waiting이라는 상태는 보이지만 왜 기다리는지와 누가 풀어야 하는지 불명확하다.
첫 버전은 runtime next와 기존 gate reason을 연결해
`review missing -> independent reviewer unavailable -> Main의 route 판단`처럼 설명한다.
보이는 DAG edge가 있어도 인과 근거가 없으면 원인으로 단정하지 않는다.

- [ ] 이유마다 source와 freshness를 표시하고 보이지 않는 원인은 unknown으로 둔다.
- [ ] cycle·missing dependency·상충하는 snapshot을 bounded하게 처리한다.
- [ ] 제안하는 next action은 기존 front door의 권한/전제조건과 같고 실행을 자동 시작하지 않는다.
- [ ] blocking path와 영향받는 task 수는 보여도 소요시간·성공확률을 추정해서 사실처럼 표시하지 않는다.

### NF-03 — Context Budget Inspector

문제: 어떤 규칙/파일/요약이 worker에 전달됐으며 중요한 문맥이 빠졌는지 확인하기 어렵다.
첫 버전은 기존 ContextManifest의 포함 이유, digest, byte budget, omission/context debt를 보여준다.
이것은 OMS가 만든 context pack이지 provider의 숨겨진 system prompt·전체 대화 복원기가 아니다.

- [ ] source별 포함 이유·bytes·필수 여부·truncation·digest와 누락 이유를 확인할 수 있다.
- [ ] token 값은 실제 보고/추정을 구분하며 tokenizer가 없으면 bytes만 제공한다.
- [ ] 필수 PROJECT/acceptance/security 맥락은 UI에서 조용히 제거할 수 없고 같은 digest의 중복을 알려준다.
- [ ] preview만으로 delegate/파일 접근 확대/규칙 편집을 하지 않으며 required-source debt를 0으로 숨기지 않는다.

### NF-04 — Run Compare와 Research Evidence Matrix

문제: 두 구현 또는 실험 중 무엇이 더 나은지 판단하려면 여러 로그와 결과를 옮겨 봐야 한다.
첫 버전은 이미 있는 두 결과를 선택해 patch·acceptance·review·reported usage를 비교한다.
연구 모드에는 dataset/split/config/seed/metric unit·direction과 verifier 근거를 함께 둔다.
동일 과제에서 새 A/B 실행은 명시적 요청·budget/동시성 승인 후 별도 기능으로 남긴다.

- [ ] code/base·과제·dataset/split·config·metric 계약 차이를 먼저 표시하고 비교불가는 승자를 고르지 않는다.
- [ ] skipped/missing/stale/failed와 측정값 0을 구분하고 단일 seed 결과를 통계적 우월성으로 표현하지 않는다.
- [ ] provenance·artifact digest와 허용된 상대 경로를 검증하고 결과를 renderer에서 실행하지 않는다.
- [ ] 비교 선택이 rerun/fan-out/합성/merge 또는 evidence 승격을 실행하지 않는다.

### NF-05 — Dispatch Preview와 Scope Conflict Radar

문제: 실행 버튼을 누르기 전 어떤 worker·model·worktree·범위·검증을 쓰는지 흩어져 있다.
첫 버전은 기존 dry-run/route/scope/WIP 결과를 하나의 제안 카드로 묶는다.
Declared scope overlap과 실제 바뀐 파일 overlap을 구분한다. Worktree가 분리됐다고
의미적 충돌이 없다고 단정하지 않는다. 새 scheduler나 자동 모델 선택 엔진이 아니다.

- [ ] requested/selected route, 접근 권한, scope, reviewer 조건, verifier와 known quota freshness를 설명한다.
- [ ] unknown quota/범위를 추정치로 채우거나 구독 소진 시 paid API로 전환하지 않는다.
- [ ] preview 뒤 plan/head/lease/route가 바뀌면 재검증하고 오래된 카드를 실행 허가로 쓰지 않는다.
- [ ] preview가 claim·dispatch·토큰 사용을 발생시키지 않으며 conflict 경고는 advisory와 실제 guard 거부를 구분한다.

### NF-06 — Read-only Timeline Replay와 재현 패키지

문제: 패널에서 잘못 표시된 순간을 다시 보려면 실제 에이전트를 돌려야 한다.
첫 버전은 제한된 structured projection/event로 과거 화면을 재구성하고,
사용자가 선택한 redacted snapshot과 terminal 크기/기능 버전을 로컬에 내보낸다.
Replay는 읽기 전용 재생이며 shell/model/approval 이벤트를 다시 실행하지 않는다.

- [ ] historical watermark와 현재 snapshot을 명확히 구분하고 과거 성공을 현재 성공으로 표시하지 않는다.
- [ ] raw transcript·credential·명령 인자·개인 절대 경로를 기본 export에서 제외한다.
- [ ] import된 문자열·path·ANSI/OSC·크기·schema를 검증하며 자동 외부 업로드를 하지 않는다.
- [ ] 같은 fixture로 표시 회귀를 재현할 수 있으나 이를 provider 실행·보안·실사용 검증으로 주장하지 않는다.

## 7. 우선순위와 기존 PR 묶음에 연결

가치/비용 평가는 설계 판단이며 실제 사용시간·개발시간 측정이 아니다.
기존 CP-02의 최소 identity/authority 계약만 만족하면, UX 후보가 모든 provider의 전역
discovery 완료를 기다릴 필요는 없다. 기존 managed 데이터로 먼저 비교할 수 있다.

| 단계 | 명세 | 기존 묶음 연결 | 판단 |
|---|---|---|---|
| A | UX-01/02/03/04 | I1의 최소 경계 + I4/I5 | 최우선: 판단·검토 동선 개선 |
| B | NF-01/02/03 | I3/I5/I6 | 높은 가치 후보: 기존 기록을 재사용, 기본 추가 추론 없음 |
| C | UX-05/06, NF-05 | I2/I4/I6 | provider별 전달 계약을 확인한 뒤 단계적 적용 |
| D | NF-04/06 | I5/I8 및 CP-16 | 연구 비교·재현성이 필요한 경로부터 |
| E | 웹/모바일 원격 변경, 자동 multi-run/Fusion | CP-11–13 또는 별도 ADR | 이번 제안에서 실행 보류 |

별도 실험 후보는 **versioned playbook 미리보기**와 **가벼운 artifact viewer**다.
Playbook은 기존 PROJECT/template/task envelope로 compile되는 범위부터 검토하고 새 DSL을 만들지 않는다.
Artifact viewer는 Markdown/JSON/CSV의 bounded preview부터 고려한다. Parquet/분자 3D/browser 등
큰 dependency는 실제 소비자·메모리/보안 요구를 확인하기 전 도입하지 않는다.
이 두 후보에는 이번 단계의 구현 수락 조건이나 일정 약속을 부여하지 않는다.

## 8. 검증할 사용자 여정과 UX 지표

동일한 synthetic 사건으로 현행 화면과 후보 화면을 비교한다.
아래 수치는 **합격 목표 제안**이지 현재 성능이나 측정된 개선율이 아니다.
경로 길이는 선택 후 UI 행동이며 텍스트 입력 길이·권한 확인은 별도로 센다.

| 여정 | 확인할 결과 | 검증 방식 |
|---|---|---|
| 돌아와서 새 결정 찾기 | 사건 근거까지 최대 3행동, retention gap 표시 | NF-01 fixture + 사람이 baseline/candidate에서 수행 |
| 검증 실패 이유 확인 | 이유·담당·다음 조건까지 최대 2행동 | UX-01/NF-02; 확인 버튼이 approval을 만들지 않음 |
| finding 검토 후 CLI 복귀 | diff 줄/선택/미전송 입력 손실 없음 | private PTY, focus/resize 회귀 |
| busy agent에 후속 요청 | Queue/Steer 차이·미지원·전달 불명을 구분 | fake provider ACK/timeout/identity replacement |
| context 누락 확인 | 왜 제외됐는지와 필수 맥락 debt를 식별 | 기존 manifest fixture |
| 두 연구 결과 비교 | split/seed/unit 차이를 알아보고 비교 보류 | synthetic metrics와 missing/stale proof |

추가 지표는 task 찾기 시간, 원인 이해 시간, 불필요한 화면 전환, 중복 알림,
focus loss, 잘못된 대상 action 시도, 실제 추가 모델 호출 수다.
자동 테스트 통과만으로 사용성이 좋아졌다고 선언하지 않는다.
먼저 동일 사용자의 전후 수행 기록을 모으고 화면 순서를 바꿔 학습 효과를 구분한다.

로컬 renderer 입력 응답은 warm state에서 p95 100 ms 이하를 후보 목표로 삼고,
관측 갱신 지연·파일 bytes·subprocess·CPU/RSS는 별도 측정한다.
장비/terminal/버전/샘플 수를 기록한다. I/O나 원격 응답을 100 ms에 포함한 약속이 아니다.
10개 active/50개 retained task로 시작하며 cold scan과 100개 규모는 별도 결과로 남긴다.
기존 workload의 baseline 이후 목표를 고정하고 통과시키려고 뒤에서 완화하지 않는다.

## 9. 구현자 인계와 범위

수정 후보는 기존 [panel_view.py](../scripts/lib/panel_view.py),
[room_view.py](../scripts/lib/room_view.py), [panel_state.py](../scripts/lib/panel_state.py),
[panel_chats.py](../scripts/lib/panel_chats.py), [panel_metrics.py](../scripts/lib/panel_metrics.py),
[dashboard_projection.py](../scripts/lib/dashboard_projection.py)와 runtime read API다.
모든 파일을 고치는 것이 목표가 아니다. 선택한 UX/NF의 최소 소비자 경로만 변경한다.

회귀는 [visual_and_controls.py](../tests/operator/visual_and_controls.py),
[room_and_board_rendering.py](../tests/operator/room_and_board_rendering.py),
[native_integration.py](../tests/operator/native_integration.py)와 관련 existing case를 확장한다.
새 테스트 프레임워크·대규모 E2E 인프라·real provider/GPU 작업을 기본 요구하지 않는다.

각 후속 PR은 UX/NF와 CP ID, baseline/candidate snapshot, 실제 action 경로,
불가능/실패/권한 case, 측정 환경과 unknown, compatibility/rollback을 함께 기록한다.
새 public CLI/schema/default, 영속 UI 기록, 모델 호출, 원격 노출은 별도 검토 대상이다.
이 문서가 병합돼도 해당 변경의 권한이나 제품 구현 완료를 뜻하지 않는다.
