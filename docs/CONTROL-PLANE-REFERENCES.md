# OMS control-plane 참고 프로젝트 대장

검토 기준일: 2026-11-09. 목적은 [확장 작업계획](CONTROL-PLANE-EVOLUTION.md)의
설계 근거를 보관하는 것이다. 외부 프로젝트 설치·실행·부하·보안 벤치마크는 수행하지 않았다.
아래 기능은 이 대화에서 확인한 공개 README/설계 문서의 범위이며 운영 보증이 아니다.
링크는 이동하는 기본 브랜치다. 구현/코드 재사용 전 CP-18에서 정확한 commit,
LICENSE/NOTICE, 해당 기능의 소스·테스트를 다시 고정한다.

## 우선 비교 대상

| 프로젝트 / 1차 출처 | 참고할 내용 | 연결 작업 | 도입하지 않을 부분 |
|---|---|---|---|
| [AWS Labs CAO](https://github.com/awslabs/cli-agent-orchestrator), [control planes](https://github.com/awslabs/cli-agent-orchestrator/blob/main/docs/control-planes.md) | CLI/Web/MCP를 같은 관리 기능에 연결하는 adapter 경계 | CP-01/05/11/12 | OMS 위에 별도 supervisor/server 상태 권위 추가 |
| [AgentGlass](https://github.com/BjoernSchotte/agentglass) | Native session 관측, 사용량/비용 출처, read-only web와 privacy | CP-03/04/08/12 | 전체 HOME 자동 수집, 모든 로그의 복사/원격 전송 |
| [agent-dashboard](https://github.com/bjornjee/agent-dashboard) | tmux 상태별 분류, diff/plan/usage 탐색, TUI와 선택형 웹 | CP-06/07/10/12 | pane capture를 신뢰 가능한 lifecycle/승인 증거로 사용 |
| [Agent Deck](https://github.com/asheshgoplani/agent-deck) | 세션 검색·그룹·복구·Conductor UX | CP-06/07/09 | 새 Conductor를 OMS Main 위/아래 상주시키기 |
| [mu](https://github.com/mu-crew/mu) | Task DAG·명시적 wait·기록된 checks의 분리 | CP-02/07/10 | 두 번째 task DB, 관측 evidence를 검증 완료로 취급 |
| [murmur](https://github.com/mu-crew/murmur) | 호스트별 snapshot, status/picker/sidebar의 공통 read model | CP-05/07/11 | 중앙 mesh 권위·자동 원격 명령 |
| [OmO Herdr DAG](https://github.com/jc01rho/omo-herdr-dag) | 대화 focus 유지, DAG 갱신, 노드 접기·펼치기, pane 재사용 | CP-06/15 | 새 graph execution engine |
| [workmux](https://github.com/raine/workmux) | Worktree–terminal 연결, sidebar, 빠른 작업 전환 | CP-06/09/15 | 기존 worktree/landing의 자동 교체·정리 |
| [tmux-agent-sidebar](https://github.com/hiroppy/tmux-agent-sidebar) | 여러 tmux pane의 상태/활동과 바로 이동 UX | CP-02/03/06 | 발견된 세션을 자동으로 managed 등록 |
| [oh-my-symphony](https://github.com/cskwork/oh-my-symphony) | 단계별 verify, 재시작 이후 lease, Kanban 상태 설명 | CP-07/09/10 | 별도 scheduler/SQLite queue 또는 자동 merge |
| [commandcenter](https://github.com/cgeene/commandcenter) | Main→Worker→독립 Reviewer→사람의 다음 조치 | CP-07/10 | 새 daemon/DB/review gate로 기존 OMS를 중복 구현 |

## 기능별 보조 참고 대상

| 프로젝트 / 1차 출처 | 참고할 내용 | 판단 |
|---|---|---|
| [Agent of Empires](https://github.com/agent-of-empires/agent-of-empires) | 영속 tmux 세션, TUI/웹, 선택형 격리 | CP-09/12; 컨테이너 도입은 별도 범위 |
| [CCManager](https://github.com/kbwo/ccmanager) | 세션 상태 전략, 재시작 복구, worktree UX | CP-06/09; native transcript 복사 방식은 채택하지 않음 |
| [dmux](https://github.com/standardagents/dmux) | 빠른 pane/worktree 생성, 세션 복원 | CP-06/09; auto-commit/merge 기본값은 이식하지 않음 |
| [Herdr](https://github.com/herdrdev/herdr) | 터미널 host, agent-state, 재접속·layout 복원 | CP-15; 기존 선택형 adapter와 비교, 필수 의존성화하지 않음 |
| [T3 Code](https://github.com/pingdotgg/t3code) | 데스크톱/웹/모바일에서 native harness를 조작하는 UX | CP-12/13; GUI stack 도입은 별도 승인 |
| [AI Maestro](https://github.com/23blocks-OS/ai-maestro) | 여러 머신의 Attention·승인 카드·agent messaging | CP-07/11/13; 장기 상주 조직/mesh/memory store는 제외 |
| [Shipyard](https://github.com/cosscom/shipyard) | 개발 서버별 세션·worktree·연결 상태 관리 | CP-11; server provisioning/automation 플랫폼으로 확장하지 않음 |
| [oh-my-claudecode](https://github.com/Yeachan-Heo/oh-my-claudecode) | 단계별 team/verify/fix와 native/tmux 실행 구분 | CP-01/10; plugin/hook 전체 설치나 규칙 중복 금지 |
| [oh-my-codex](https://github.com/Yeachan-Heo/oh-my-codex) | Codex 기반 계획·team·review·QA UX | CP-01/10; Codex 전용 control plane으로 축소하지 않음 |
| [OmO](https://github.com/code-yeongyu/oh-my-openagent) | 멀티모델 workflow와 메모리/대기 분리 아이디어 | 개념 참고만; source license를 버전별 재검토하고 코드 복사 보류 |
| [multi-agent-shogun](https://github.com/yohey-w/multi-agent-shogun) | 명시적인 역할·파일 기반 메시징·보이는 세션 | 구조 비교; 고정 다수 worker/다층 manager와 비용 홍보 수치는 채택하지 않음 |
| [Claude Squad](https://github.com/smtg-ai/claude-squad) | 단순한 세션·diff·workspace UX | CP-06; auto-accept/yolo를 안전성 근거로 삼지 않음 |
| [agentmux](https://github.com/adelost/agentmux) | Durable delivery, recipient fanout 제한, session isolation | CP-07/13; Discord 연동은 기본 범위 아님 |
| [ccswarm](https://github.com/nwiizo/ccswarm) | 평가별 의견과 pipeline 단계 구분 | CP-10; approval 수를 OMS evidence/admission의 대체물로 쓰지 않음 |

## 메커니즘 단위 보강 (2026-11-09)

위 표는 프로젝트 단위의 방향만 적는다. 아래는 후속 구현이 실제로 참고할 수 있는
개별 메커니즘과 그 확인 수준이다. 외부 프로젝트를 설치하거나 실행하지 않았고
exact commit도 고정하지 않았다. 구현 근거로 쓰기 전에 채택 기록 양식으로 다시 확인한다.

확인 수준: **문서** = 해당 프로젝트의 공식 문서 페이지를 직접 열어 확인,
**조사** = 조사 도구가 README·문서·release note·issue를 읽고 요약한 내용(직접 재확인하지 않음),
**보조** = 제3자의 글·gist·fork에서 온 내용. 소스 코드와 테스트는 어느 항목도 확인하지 않았다.

| 프로젝트 / 출처 | 메커니즘 | 확인 | 연결 작업 | OMS 판단 |
|---|---|---|---|---|
| [tmux-agent-sidebar scripting](https://hiroppy.github.io/tmux-agent-sidebar/reference/scripting/) | Agent hook이 상태를 tmux pane 사용자 옵션(`@pane_status`, `@pane_attention`, `@pane_wait_reason` 등)에 쓰고, sidebar·status line·스크립트가 `tmux show -pv`로 읽는다 | 문서 | CP-06/07 | 데이터 계약만 독립 구현 후보로 둔다: board가 Attention 개수만 자기 window option으로 게시하는 형태. 같은 표의 `@pane_prompt`, `@pane_cwd`, `@pane_session_id`에 해당하는 값은 CP-07의 표시 제한 때문에 게시하지 않는다 |
| [oh-my-symphony v0.29.0](https://github.com/cskwork/oh-my-symphony/releases/tag/v0.29.0) | `.symphony/needs.json`이 막힌 ticket을 state/since/reason/action으로 나열한다. Verify gate는 lane을 떠나거나 건너뛸 때 실행되고, 실패하면 카드를 이전 lane으로 되감고 실패 note를 붙인다 | 조사 | CP-06/10 | “왜 멈췄는가 / 다음 조치”를 기존 Plan·Attention 표시에 더하는 것이 후보다. 별도 needs 파일, lane 엔진, agent가 직접 쓰는 상태 전이는 도입하지 않는다 |
| [Beads](https://github.com/gastownhall/beads) | `bd ready --explain`이 열린 blocker와 그 작업이 풀어 줄 작업 수를 보여 준다 | 조사 | CP-06 | Plan 행에 미완료 dependency를 표시하는 것이 후보다. 두 번째 task store는 만들지 않는다 |
| [Gas Town](https://github.com/gastownhall/gastown) | Problems 뷰가 agent를 진행 없음/정체/세션 사망/작업 중/유휴로 분류하고 nudge·handoff 키를 제공한다. Witness는 세션 종료 전에 git 상태를 확인한다. 감시 사슬의 바닥은 주기적 heartbeat를 내는 daemon이다 | 조사 | CP-02/07/09 | 분류 어휘와 “종료 전 dirty 확인”만 참고한다. Daemon, AI 감독 계층, 자동 nudge·kill은 [원칙](CONTROL-PLANE-EVOLUTION.md#2-보존할-설계-원칙) 1·5와 충돌하므로 도입하지 않는다 |
| Gas Town 정체 임계값 | 30분 무진척, nudge 3회 뒤 escalation 같은 숫자 | 보조 | — | 출처가 README가 아니므로 OMS threshold의 근거로 쓰지 않는다. CP-07의 threshold는 자체 fixture와 baseline으로 정한다 |
| [OpenAI Symphony SPEC](https://github.com/openai/symphony/blob/main/SPEC.md) | 실패 재시도 간격이 지수적으로 늘고 상한을 가진다. Stall timeout을 넘기면 실행을 종료하고 재시도한다 | 조사 | CP-07/09 | 반복 알림에 상한 있는 backoff를 쓰는 개념만 참고한다. 정체 시 종료·재시도는 원칙 5와 충돌하므로 도입하지 않는다 |
| [Agent Deck CLI reference](https://github.com/asheshgoplani/agent-deck/blob/main/skills/agent-deck/references/cli-reference.md) | Down 판정에 연속 probe의 일치를 요구한다(`--confirm-probes`). `events follow --jsonl --since CURSOR`로 이벤트를 이어 읽는다. 의도적 stop이 이후 sweep에서 error로 덮이는 문제를 조건부 쓰기로 고쳤다 | 조사 | CP-02/07 | “한 번의 실패한 probe로 종료를 확정하지 않는다”는 규칙은 CP-02 liveness 검토에, cursor 기반 tail은 CP-07의 bounded 대기 후보로 남긴다 |
| [CAO issue #896](https://github.com/awslabs/cli-agent-orchestrator/issues/896) | Inbox 메시지는 수신 terminal이 유휴일 때 전달된다. 선택형 eager delivery는 처리 중·사용자 응답 대기 중에도 전달하며, 열린 질문 대화상자에 메시지가 입력된 사례가 보고됐다 | 조사 + 보조 | CP-02 | OMS의 tick wake는 입력줄이 비었음을 증명하지 못하면 입력하지 않는다(`panel_native.input_line_empty`, 이 저장소 코드에서 확인). 승인·질문 대화상자 화면을 그 회귀 fixture에 추가해 같은 부류를 고정하는 것이 후보다 |
| [workmux](https://github.com/raine/workmux), [sidebar guide](https://workmux.dev/guide/sidebar/) | Agent hook이 window 상태를 설정하고, sidebar는 백그라운드 프로세스가 tmux 상태를 주기적으로 읽어 각 sidebar pane에 전달한다. `--all`로 현재 multiplexer의 모든 저장소를 조회한다 | 조사 | CP-06/15 | 상주 프로세스 없이 가능한 부분만 참고한다. 전역 조회는 새 project manager 대신 tmux의 `list-windows -a`와 게시된 window option으로 충분한지 먼저 본다 |

### 앞선 비교의 정정

대화에서 쓰인 요약 가운데 아래 내용은 조사 결과와 달랐다. 모두 **조사** 수준이다.

- CAO는 SQLite와 상주 API 서버를 전제로 한다. OMS와 가까운 것은 여러 CLI와 MCP라는 표면이며 상태 구조는 아니다.
  CAO의 provider 목록에 있는 것은 Pi가 아니라 Oh My Pi다.
- oh-my-symphony의 “실패 시 되돌림”은 코드 rollback이 아니라 카드를 이전 lane으로 되감는 동작이다.
- Gas Town 저장소는 `gastownhall` 조직으로 옮겨졌고 자체 웹 대시보드가 있다.
- workmux의 지속 표시 sidebar는 백그라운드 프로세스를 쓴다. 그대로 옮기면 원칙 1과 충돌한다.
- tmux-agent-sidebar에서 OMS가 가져올 것은 sidebar UI가 아니라 pane option이라는 데이터 계약이다.

### 이 조사로도 채택하지 않는 것

AI 감독 계층(Gas Town의 Mayor/Deacon/Witness), SQLite·Dolt 상태 원장, 상주 HTTP·PTY 서버,
정체 시 자동 kill·cold restart, agent가 자기 task 상태를 직접 바꾸는 모델은 위 조사 뒤에도
[작업계획 7절](CONTROL-PLANE-EVOLUTION.md#7-착수하지-않을-작업)의 비범위로 남긴다.

## 신규 의존성으로 채택하지 않을 후보

아래 상태는 검토 시 README에 명시된 내용이다. 미래의 fork/재개 여부는 별도로 확인한다.

| 프로젝트 / 출처 | 확인한 제한 | 결정 |
|---|---|---|
| [Vibe Kanban](https://github.com/BloopAI/vibe-kanban) | README의 sunsetting 안내 | 보드/diff UX 참고만, 새 기반 의존성으로 채택하지 않음 |
| [Overstory](https://github.com/jayminwest/overstory) | 유지보수 중단·archive 안내 | 역사적 구조 참고만 |
| [Warren](https://github.com/jayminwest/warren) | README의 2026년 11월 유지보수 중단 안내 | 후속 프로젝트라는 이유만으로 활성 대안으로 추천하지 않음 |

## 추가 확인 없이는 설계 근거로 쓰지 않는 후보

앞선 비교에 언급된 [OpenAI Symphony](https://github.com/openai/symphony),
[Gas Town](https://github.com/gastownhall/gastown),
[Beads](https://github.com/gastownhall/beads),
[Conductor](https://github.com/ShacklesLay/conductor)는 이 PR의 구현 근거로 채택하지 않는다.
해당 기능·현재 상태·코드/테스트를 확인한 뒤 필요한 항목만 CP-18에 추가한다.
이름이 비슷한 fork나 제품을 원본과 혼동하지 않는다.

2026-11-09 보강에서 OpenAI Symphony, Gas Town, Beads의 README·문서 수준 내용을
[메커니즘 단위 보강](#메커니즘-단위-보강-2026-11-09)에 기록했다. 소스·테스트·exact commit은
여전히 확인하지 않았으므로 위 판단은 바뀌지 않는다. Conductor는 같은 이름의 다른 제품이
있어 대상을 특정하지 못했고 이번에도 확인하지 않았다.

## 채택 기록 양식

후속 구현 PR에는 필요한 항목만 다음 형식으로 기록한다. 모든 프로젝트를 설치하거나
기능 수를 맞추기 위한 구현은 요구하지 않는다.

```text
OMS 작업 ID:
문제 / 최소 소비자:
기존 OMS 구현 / 재사용할 helper:
참고 repo / exact commit / file / 검토일:
확인 수준: README / source / fixture / native run
도입할 개념 / 도입하지 않을 동작:
코드 재사용 여부 / LICENSE / NOTICE / dependency 검토:
실패·권한·privacy 차이:
비교 fixture / baseline / 수락 조건:
판단: 재사용 / 독립 구현 / 현재 코드 유지 / 보류
```

원칙: 코드 근거가 없는 “유일함”, “무제한”, “무료”, “10배”, “zero overhead” 등의
홍보 표현을 OMS 요구사항이나 성능 합격치로 가져오지 않는다.
