# OMS 공통 제어 화면과 플러그인 로드맵

작성일: 2026-11-24 (KST). 코드 대조 기준:
[`63142c26`](https://github.com/eightmm/oh-my-setting/commit/63142c26e805749a6c3c0b78c30caecdac1ef0b3).
상태: **설계와 작업 등록. 공통 UI·원격 제어의 구현 완료를 뜻하지 않는다.**
진행 현황과 Project 연결 증거는 [epic #431](https://github.com/eightmm/oh-my-setting/issues/431)에서 관리한다.

## 1. 결정과 첫 결과물

OMS를 같은 `eightmm/oh-my-setting` 저장소에서 발전시킨다. GitHub Project 하나로
관련 Issue와 PR을 모으고, 기존 Rust runtime 위에 선택형 공통 화면과 host adapter를
추가한다. 별도 제품 저장소, 새 실행 엔진, 필수 상주 daemon을 먼저 만들지 않는다.
패키지와 UI 의존성은 설치 대상별로 분리할 수 있지만 실행 상태의 소유자는 유지한다.

첫 결과물은 **현재 repository의 작업, Main, 진행 상태, 검증 근거를 읽는 화면**이다.
같은 projection을 터미널과 MCP client에서 확인하고, 개인용 ChatGPT 연결에서
inline UI를 실증한다. 네 provider의 모든 기능을 동시에 구현하는 것이 첫 출시 조건은 아니다.
각 기능은 확인된 adapter만 사용하고, 미지원 기능과 증거가 없는 기능을 구분한다.

이 문서는 [통합 작업 큐](ORCHESTRATOR-WORK-QUEUE.md)의 기존 owner를 사용자 흐름으로
연결하는 delivery view다. [Feature scope](FEATURE-SCOPE.md), AH/CP/CC/RF의 세부 계약,
기존 lifecycle·검증·landing owner를 대체하지 않는다. CP-12의 구체적 수요를 기록하며,
CP-13은 typed 권한 계약을 검토하기 전까지 보류한다. 로드맵 등록은 public endpoint
개방, tunnel 계정 설정, 유료 추론, 원격 mutation 또는 공개 배포의 실행 기록이 아니다.

## 2. 상태와 권한의 소유자

| 대상 | 소유자 | 다른 화면에서 하는 일 |
|---|---|---|
| 개발 요구사항·Issue 완료 조건 | GitHub Issue | 기존 Issue를 참조하고 결과 근거를 연결 |
| 우선순위·개발 단계·개발 진행 | GitHub Project | 같은 Issue/PR을 table·board에서 분류 |
| 변경 코드·검토·병합 증거 | Git와 GitHub PR | 정확한 commit/tree와 검증 결과 표시 |
| 실행 task·attempt·Room·lease·approval | 기존 OMS writer와 `.oms` 상태 | bounded projection을 읽고 허용된 typed action만 요청 |
| 임시 화면 선택·필터 | 각 UI instance | 실행 상태와 독립적으로 보존 |

실행·검증 계약은 [프로젝트 맥락](PROJECT-CONTEXT.md)의 `PROJECT.md` Verification과
기존 절차로 채택한 Plan을 따른다. `intent draft`는 후보이며 승인된 내용만 기존
`intent adopt`로 채택한다. GitHub Issue·Project 편집만으로 활성 run이나 이미 채택된
완료 조건이 바뀌지 않는다.

Project의 `Ready`, `In progress`, `Done`은 개발 관리 값이다. 카드 이동은 agent 실행,
비용 발생, 승인 소비, task 수락, PR 병합을 지시하지 않는다. Project가 `Done`이어도
OMS run의 완료나 검증을 추론하지 않는다. 반대로 실행 종료만으로 개발 Issue를 닫지 않는다.

ChatGPT, Codex, Web UI, TUI는 같은 OMS를 이용하는 client다. ChatGPT 연결 자체가
두 번째 Main을 만들지 않는다. Main 교체는 기존 Room과 process generation, lease,
결과 인계 조건을 따르고 하나의 작업 소유권을 유지한다. 대화 기록은 provider별로
분리하며 다른 provider의 native session을 같은 ID로 재개할 수 있다고 가정하지 않는다.

## 3. 이미 있는 기반과 현재 한계

### Main 선택

현재 [native Main capability](../crates/oms-command-panel/src/native_panel.rs)와
[터미널 패널 안내](TERMINAL-PANEL.md)를 기준으로 한다. 아래는 source 연결 사실이며
이번 문서 작업에서 실제 provider CLI를 실행해 확인한 결과는 아니다.

| Harness | 현재 owner / transport | 보존할 차이 |
|---|---|---|
| Codex | `codex` / native positional prompt | 기존 session·계정·승인 및 process binding 유지 |
| Claude Code | `claude` / native append-system prompt | outgoing Main 전환의 남은 범위는 [#392](https://github.com/eightmm/oh-my-setting/issues/392) |
| AGY | `antigravity` / native interactive | 검증된 native session binding 없는 resume 거절 |
| Pi | `codex` / `CodexGateway` | `pi`, `codex`, `oms` 필요; 검증된 Pi binding 없는 resume 거절 |

따라서 harness, inference/dispatch owner, account, model, session identity를 한
`provider` 문자열로 합치지 않는다. Pi의 현재 Main 경로를 임의 Pi provider 지원으로
확대하지 않는다. Launch가 있다는 사실은 steer·resume·approval·usage의 지원 증거가 아니다.

### 재사용할 구성요소

| 기존 구성요소 | 이번 확장에서 재사용할 책임 |
|---|---|
| [OMS runtime](OMS-RUNTIME.md) | 기존 상태의 typed projection, evidence와 다음 동작의 근거 |
| [panel core](../crates/oms-panel-core/README.md) | terminal과 독립적인 읽기 모델, 기존 action 검증 |
| [MCP server](../crates/oms-mcp-server/README.md) | native stdio, bounded tool 결과, fixed-argv 경계와 UI resource |
| [supervisor](../crates/oms-supervisor/README.md) | process·stdio·deadline·취소·cleanup, 선택형 bounded duplex |
| [GitHub workflow](wiki/GitHub-Workflow.md) | Issue intake, Plan binding, PR와 병합 evidence |
| [기존 Codex plugin](../plugins/oh-my-setting/.codex-plugin/plugin.json) | 기존 설치 layout·hooks 소유권과 호환성 |

논의한 모듈 이름은 분리 후보이며 새 crate나 서비스 생성 결정이 아니다. 먼저 현재
owner에 연결하고, 독립 배포나 의존성 분리가 필요한 경우에만 경계를 나눈다.

| 모듈 후보 | 먼저 재사용할 owner / 연결 Issue |
|---|---|
| `oms-control-api` | runtime·panel-core의 typed projection과 기존 action writer; #432/#433/#437 |
| `oms-gateway` | 기존 MCP의 선택형 remote transport·caller scope 경계; #434/#435/#445 |
| `oms-host` | 기존 host registry·supervisor·remote projection; #444와 기존 multi-host owner |
| `oms-provider-codex` | 현재 native Codex adapter와 session lifecycle; #432/#436. 다른 Main도 같은 capability 계약 소비 |
| shared React UI | 기존 graph resource·공통 projection 위의 공유 UI 후보; #433/#438. React 채택은 미확정 |
| provider adapters | 기존 Codex/Claude/AGY/Pi 및 custom adapter·RF-01; #436. 같은 process나 queue의 두 번째 owner 금지 |
| host plugins | 기존 OMS plugin layout·MCP resource·설치 lifecycle; #438/#445 |

Remote 연결 후보 `oms-gateway`와 Pi의 현재 **Codex inference gateway**는 목적이 다르다.
이름이 비슷하다는 이유로 인증·transport·모델 추론의 책임을 같은 서비스로 합치지 않는다.

MCP는 full 32개, core discovery 13개의 도구를 제공한다. 두 discovery profile 모두
같은 32개 이름의 호출을 기존 제한 아래 처리하므로 **discovery 축소가 권한 제한은 아니다**.
기존 graph UI resource와 `window.openai` 연동은 출발점이다. 이를 완성된 공통 대시보드나
모든 host에서 검증된 UI라고 표시하지 않는다. Remote pilot은 별도의 server-side read 권한
경계를 확인하고, UI에서 버튼을 숨기는 것으로 mutation을 막았다고 간주하지 않는다.

### 2026-11-24 PR 상태 재대조

| 근거 | 확인 상태와 로드맵에 미치는 영향 |
|---|---|
| [#313](https://github.com/eightmm/oh-my-setting/pull/313) → [#342](https://github.com/eightmm/oh-my-setting/pull/342) | #313 자체는 미병합 종료; AH 문서가 병합된 #342에 통합됨. AH 전체 구현 완료는 아님 |
| [#333](https://github.com/eightmm/oh-my-setting/pull/333) → [#348](https://github.com/eightmm/oh-my-setting/pull/348) | #333 자체는 미병합 종료; conformance 구현이 병합된 #348에 통합됨. 도움말·버전 probe는 native session 인증이 아님 |
| [#334](https://github.com/eightmm/oh-my-setting/pull/334), [#335](https://github.com/eightmm/oh-my-setting/pull/335), [#318](https://github.com/eightmm/oh-my-setting/pull/318) | Open draft. Identity·account·council 작업을 중복 구현하지 않고 의존 관계 확인 |
| [#347](https://github.com/eightmm/oh-my-setting/pull/347), [#353](https://github.com/eightmm/oh-my-setting/pull/353) | 병합됨. Issue/Plan/PR 연결을 재사용; Projects v2 field 동기화와 구분 |
| [#373](https://github.com/eightmm/oh-my-setting/pull/373), [#381](https://github.com/eightmm/oh-my-setting/pull/381), [#390](https://github.com/eightmm/oh-my-setting/pull/390) | 병합됨. Native 결과·review·landing evidence 연결을 새 UI가 소비 |
| [#394](https://github.com/eightmm/oh-my-setting/pull/394), [#397](https://github.com/eightmm/oh-my-setting/pull/397), [#430](https://github.com/eightmm/oh-my-setting/pull/430) | 병합됨. Main succession·switch·reattach/reboot 처리를 별도 session manager에 복제하지 않음 |
| [#396](https://github.com/eightmm/oh-my-setting/pull/396) | Open 통합 적용 대상. #395/#380의 component 이력을 main 완료 상태로 계산하지 않음 |

상태 표는 날짜가 있는 조사 기록이다. 착수 시 current source와 PR state를 다시 확인한다.
[권한 #277](https://github.com/eightmm/oh-my-setting/issues/277),
[사용량 #279](https://github.com/eightmm/oh-my-setting/issues/279),
[결과 연결 #366](https://github.com/eightmm/oh-my-setting/issues/366),
[readiness #367](https://github.com/eightmm/oh-my-setting/issues/367)은 기존 Issue를 이어 쓴다.
오래된 본문의 “미병합” 문구보다 현재 PR metadata와 실제 caller를 우선한다.
위 #318은 council 좌석의 기존 이력이며 이번 UI 확장의 신규 구현 범위나 필수 선행은 아니다.

## 4. 전체 논의와 단계별 canonical Issue

단일 저장소·단일 Project 결정은 epic #431, 구현 논의는 아래 **13개 Issue**에 연결한다.
A–M은 행 식별자이며 새로운 runtime ID나 독립 작업 큐가 아니다. Issue의 acceptance가
구현 범위이고, 우선순위는 이 확장 안의 순서다. 기존 core owner와 UI 상세 동선을 나눠 추적한다.

| 행 / Issue | 기존 owner | Phase / 우선순위 | 추가할 결과와 선행 조건 |
|---|---|---|---|
| A / [#432](https://github.com/eightmm/oh-my-setting/issues/432) | AH-01/03/08, RF-01, CC-01 | Foundation / P0 | provider·session·capability 계약, 타입·오류·event 예시와 증거 수준 확정; #334/#335 경계 재사용 |
| B / [#433](https://github.com/eightmm/oh-my-setting/issues/433) | CP-12, AH-11, 기존 projection owner | Read-only / P1 | A를 소비하는 공통 작업·Main·evidence 읽기 화면 |
| C / [#434](https://github.com/eightmm/oh-my-setting/issues/434) | AH-09, CC-10 | Foundation / P1 | A 이후 MCP caller와 repo/Room identity binding; 기존 Room send/updates/ack 재사용 |
| D / [#435](https://github.com/eightmm/oh-my-setting/issues/435) | CP-12, AH-11, 기존 MCP/plugin | Read-only / P1 | B/C의 읽기·identity 경계 위에서 개인용 ChatGPT inline UI·private transport 실증 |
| E / [#436](https://github.com/eightmm/oh-my-setting/issues/436) | AH-03, RF-01, lifecycle owner | Control / P1 | A 이후 UI와 독립적인 native session/message/event adapter 연결 |
| F / [#437](https://github.com/eightmm/oh-my-setting/issues/437) | CP-13, AH-12, 기존 action writer | Control / P1 | B/C/E와 검토된 action 계약 이후 scoped remote command; 기본 보류 |
| G / [#438](https://github.com/eightmm/oh-my-setting/issues/438) | 기존 plugin/MCP, CP-12 | Extensions / P2 | D 이후 ChatGPT sidebar/thread와 Codex 패키지의 실제 host별 검증 |
| H / [#439](https://github.com/eightmm/oh-my-setting/issues/439) | #347/#353, `oms-github` | Extensions / P2 | 필요가 입증될 때 Projects v2 metadata 동기화; 초기 수동 보드 운영의 선행 아님 |
| I / [#441](https://github.com/eightmm/oh-my-setting/issues/441) | 기존 identity/account/catalog, A/B | Read-only / P1 | provider 연결·설정 UX; 설치·인증·연결·관측·제어 가능 상태를 구분 |
| J / [#442](https://github.com/eightmm/oh-my-setting/issues/442) | 기존 artifact/review/approval/landing owner | Control / P1 | 결과→artifact→diff→검증→결정 동선; 읽기는 B, 실제 조작은 F 의존 |
| K / [#443](https://github.com/eightmm/oh-my-setting/issues/443) | C/G와 기존 context/identity owner | Extensions / P2 | scoped context reference, Composer Mentions, Deep Links와 재탐색 |
| L / [#444](https://github.com/eightmm/oh-my-setting/issues/444) | CP-11, 기존 multi-host owner | Extensions / P2 | A/B/C 이후 등록된 여러 host의 읽기 projection; 실제 수요가 있을 때 착수 |
| M / [#445](https://github.com/eightmm/oh-my-setting/issues/445) | 기존 install/lifecycle와 D/G | Extensions / P2 | private/public 배포·운영·복구 기준; 제어 노출 release는 F도 의존 |

B는 native 제어 기능의 완성을 기다리지 않고 현재 읽기 projection으로 시작할 수 있다.
D의 private 연결보다 **C의 caller/repository 검증과 read-only E2E가 먼저**다. E는 A 이후
UI와 병행할 수 있다. F는 B/C/E와 D의 pilot 증거를 검토한 뒤, 예를 들어 기존 task/attempt
취소 요청 transaction 하나의 provider·scope·writer를 고정해 연결한다. 새 universal cancel을
만들지 않는다. I/J/K는 해당 core 계약을 소비하고 identity·approval·session engine을 복제하지
않는다. Parent·선행 관계는 Issue 본문 링크이며 native sub-issue/dependency 등록과 구분한다.

L은 CP-11 및 [기존 multi-host 단계](MULTI-AGENT-MULTI-HOST-PLAN.md)를 따르고 첫 private
dashboard의 필수 조건은 아니다. Cross-provider handoff도 기존 succession·capsule·#392의
범위에서 실제 요구가 있을 때만 넓힌다. M은 배포 대상·운영 주체·비용 범위가 정해진 뒤 착수한다.

## 5. 구현 계약의 핵심

### Projection과 session

UI는 repository, Room, task/attempt, session, generation과 freshness 근거를 표시한다.
여러 원장을 읽는 snapshot이 원자적이지 않으면 그 한계를 노출한다. `unknown`, `partial`,
`stale`, `unsupported`를 성공이나 0으로 바꾸지 않는다. 사용량은 단위, 누적 범위,
account 귀속, estimate 여부가 확인된 기존 값만 보여 준다.

Session adapter는 launch/status/observe와 아래 동작의 지원 상태를 각각 광고한다.
실제 native ID와 OMS attempt를 연결하고 reconnect 뒤 같은 generation인지
검사한다. UI 종료가 backend run 종료를 의미하지 않으며, widget state를 지워도 기존
run을 다시 찾을 수 있어야 한다. 새 durable 기록이 필요하면 기존 owner의 versioned
계약을 확장하고 별도의 session 권한 DB를 만들지 않는다.

Event 전달은 bounded cursor, 중복 식별, gap, 보존 상한을 정의한다. 통신 단절 뒤
결과가 불명확한 send를 자동 재전송하지 않는다. Worker 완료 보고, verifier 통과,
OMS acceptance, PR publication을 서로 다른 evidence로 유지한다.

### 탐색과 메시지 lifecycle

| 동작 | 의미와 완료 증거 / owner |
|---|---|
| 세션 선택·열기·draft | 화면 탐색과 미전송 초안; 실행·Main 변경·메시지 전송을 유발하지 않음. #433/#441/#443 |
| Send | 지정한 세션·attempt에 요청을 제출; accepted/delivered 결과와 실제 처리 결과를 구분. #436 |
| Queue | 실행 중 turn 이후 처리하는 기능이 실제 지원될 때만 사용; 별도 UI queue로 성공을 흉내 내지 않음. #436 |
| Steer | 현재 turn에 영향을 주는 지원 경로; Queue나 다음 turn의 Send와 구분. #436 |
| Interrupt | 현재 처리 중단의 provider별 의미와 결과 확인; 세션 종료로 추정하지 않음. #436/#437 |
| Stop | 대상 task/attempt/process의 정확한 취소·종료 계약과 receipt 사용; 버튼 응답은 실제 종료 증거가 아님. #436/#437 |
| Attach | 검증된 기존 세션에 다시 연결; 새 Main/실행을 생성하지 않음. #436 |
| Resume | 검증된 native history/binding으로 지원 경로를 재개; Attach와 구분하고 AGY/Pi의 현재 거절 유지. #436 |

전송 결과가 불명확하면 `delivery_unknown`으로 남기고 receipt/cursor를 확인한다.
ChatGPT 대화에 context를 붙이거나 메시지를 보내는 동작과 OMS native Main에 전달하는
동작은 다르다. 전자는 후자의 delivery receipt를 만들지 않는다. UI는 기존 TUI와 native
pane 접근을 보존하고, 같은 backend 세션을 여러 화면에서 찾을 수 있게 한다.

### MCP와 원격 action

Client가 전달한 path, participant label, agent ID를 신뢰해 repository나 권한을
선택하지 않는다. 인증된 caller를 허용된 repo/Room에 server-side로 바인딩한다.
Room join/ack는 Main, lease, 승인 권한을 만들지 않는다. 기존 MCP에 이미 있는
Room peer 경로를 이어 쓰고 CLI 전체를 새 도구 목록으로 복제하지 않는다.

Mutation은 actor/action/target, scope, expected revision/generation, expiry,
idempotency와 replay 판정을 갖춘 typed 요청으로만 기존 writer에 전달한다.
Action은 시작 전에 현재 권한과 소유권을 다시 검사하고 정확한 receipt를 반환한다.
취소 요청 수신과 실제 process 종료도 분리한다. UI optimistic state가 성공 receipt를
대신할 수 없다. 기존 approval·admission·review·landing 절차는 유지한다.

## 6. ChatGPT와 Codex 연결 순서

1. 기존 stdio MCP를 local client로 검증하고 structured/text 결과를 보존한다.
2. 표준 MCP Apps bridge로 초기화, tool 결과 수신, refresh, 빈 상태, 오류를 연결한다.
   기존 `window.openai` 경로는 필요한 호환 범위로 유지한다.
3. 계정의 사용 가능성과 권한이 확인되면 stdio + Secure MCP Tunnel로 private pilot을
   실증한다. Tunnel 미지원이면 그 제한과 적합한 HTTPS 연결 경로를 기록한다.
4. Host/앱/version/account별로 inline, sidebar, thread, tools/skills를 따로 확인한다.
5. 개인용 설치와 공개 배포를 분리한다. 공개 배포 조건은 해당 시점의 공식 문서와
   설치·업그레이드·제거 시험으로 검토한다.

### 논의한 9개 UI surface

#438은 host bridge·등록·지원 matrix를, #433/#441/#442/#443은 아래 UX를 소유한다.
이 표는 검증할 목표이며 현재 OMS나 모든 ChatGPT/Codex host의 지원 완료 표가 아니다.

| Surface | 사용자 동선 / canonical Issue | 미지원 시 fallback |
|---|---|---|
| Global Sidebar | 프로젝트·host 선택과 작업 개요; #433/#438, 다중 host는 #444 | Inline Widget와 명시적 repository 선택 |
| Conversation Panel | Agents/Plan/Logs/Review 탐색; #433/#442/#438 | bounded 요약과 허용된 상세 참조 |
| Inline Widget | 작업·Main·상태·evidence 카드; #433/#435 | structured/text tool 결과 |
| Composer Mentions | 허용된 repo/Room/task/run/artifact context 선택; #443/#438 | 명시적 tool argument와 scoped reference |
| File Viewer | artifact·diff·검증 근거 확인; #442/#438 | bounded text/diff와 권한이 확인된 artifact 참조 |
| PiP | 실행 상태를 작게 유지하는 관측 화면; #433/#438 | inline 또는 지원되는 side panel |
| Forms | 설정·결정 입력과 검토할 대상 표시; #441/#442/#438, 제출 권한은 #437 | 텍스트로 확인 가능한 명시적 typed 요청 |
| Deep Links | 같은 작업·세션·산출물을 새 화면/대화에서 열기; #443/#438 | stable ID를 받는 조회 도구 |
| Settings | 연결·harness·account·model/effort·지원 범위 확인; #441/#438 | 기존 CLI/config 경로와 상태 설명 |

각 host/app version/account × surface에 `supported`, `partial`, `unsupported`, `unknown`과
fallback을 기록하고 문서·fixture·live 증거를 구분한다. UI처럼 보이는 mockup이나 manifest
발견만으로 지원을 승격하지 않는다. Settings는 설치·인증·연결·launch·observe·control을,
Review는 보고·검증·acceptance·publication을 각각 구분한다. 기존 offline artifact viewer를
remote artifact fetch 지원으로 확대하지 않으며, Forms 초안과 실제 feedback receipt도 구분한다.

[OpenAI plugin 문서](https://developers.openai.com/plugins/build/plugins)는 portable
`plugin.json`/`mcp.json`과 기존 `.codex-plugin` 호환 경로를 설명한다. 공통 directory는
모든 host의 UI 기능이 동일하다는 증거가 아니다. 기존 OMS hooks의 설치 소유권을 보존하고
배포 대상별 bundle을 같은 저장소에서 분리한다.

[MCP UI 문서](https://developers.openai.com/plugins/build/chatgpt-ui)는 표준 bridge와
server-owned business state를 설명한다. [Extensions 문서](https://developers.openai.com/plugins/build/extensions)의
sidebar와 conversation panel은 지원 surface 안의 확장이다. ChatGPT/Codex 앱 전체의
DOM/CSS를 바꾸거나 모든 대화 기록을 추출하는 API로 해석하지 않는다.

[Secure MCP Tunnel](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels)은
outbound 연결로 stdio/HTTP MCP를 이용하는 private 경로다. 실제 workspace association과
권한, runtime credential 관리, client 가용성을 확인해야 한다. 이 경로는 첫 demo 전에
새 public HTTP server를 만드는 의존성을 줄이지만 공개 directory 배포 경로는 아니다.
[공개 제출](https://developers.openai.com/plugins/deploy/submission)의 HTTPS·review 조건과
local stdio/hook 패키지의 제한은 별도 배포 작업에서 재확인한다.

## 7. GitHub Project 초기 구성

실제 Project는 [OMS Development](https://github.com/users/eightmm/projects/3)이며
`eightmm/oh-my-setting`의 동일 Issue/PR을 모은다. 현재 항목 수, 필드 반영과 설정 확인 결과는
[epic #431의 receipt](https://github.com/eightmm/oh-my-setting/issues/431)를 따른다.

| 필드 | 초기 값 / 의미 |
|---|---|
| Status | Backlog, Ready, In progress, Review, Blocked, Done |
| Priority | P0, P1, P2; 이 로드맵 내부 상대 순서 |
| Area | Runtime, MCP, UI, Plugins, GitHub |
| Phase | Foundation, Read-only, Control, Extensions |

Views는 [Roadmap](https://github.com/users/eightmm/projects/3/views/1)(Phase별 table),
[Board](https://github.com/users/eightmm/projects/3/views/2)(Status별 board),
[Review](https://github.com/users/eightmm/projects/3/views/3)(Review 항목)다. 일정 근거가 생기기
전에는 임의 날짜나 마감을 채우지 않는다. Epic, A–M과 관련 기존 Issue/PR을 연결하며,
Draft PR은 자동으로 review-ready로 판단하지 않는다.

Issue의 Goal/Scope/Acceptance criteria/Verification을 작성하고 구현 PR은 같은 Issue를
참조한다. 초기 운영은 수동 갱신과 필요한 GitHub 내장 기능으로 충분하다. Projects v2
자동 동기화 H는 별도 선택 작업이며 새 scheduled Actions, 반복 polling, 유료 job을
추가하지 않는다. H는 manual/dry-run으로 mapping과 예상 변경을 먼저 보여 주고 필드별
소유권, 사용자 편집 충돌, 권한 오류, 재시도 중복을 처리한다. Project 상태로 runtime 실행을
시작하거나 실행 원장을 역으로 덮어쓰지 않는다.

## 8. 첫 MVP 완료 기준과 검증

- 같은 repository/run의 CLI, MCP, UI가 같은 source revision과 evidence를 가리킨다.
- UI가 없어도 structured/text 결과로 작업·오류·지원하지 않는 기능을 이해할 수 있다.
- Main은 Codex 고정이 아니며 네 harness의 실제 차이와 미검증 칸이 표시된다.
- Private read client가 임의 repo/path를 읽거나 숨겨진 mutation tool을 호출할 수 없다.
- UI 종료·재접속·새 대화에서 backend-owned run을 조회하고 stale/gap을 식별한다.
- 기존 non-Codex Main fixture에서도 같은 run을 다시 열며 duplicate session/Main을 만들지 않는다.
- 기존 panel, CLI, one-shot adapter와 설치·제거 소유권을 보존한다.
- 실제 확인한 host와 버전, fixture 검증, live 검증을 구분해 Issue에 기록한다.

각 구현은 가장 가까운 기존 fixture를 확장한다. Projection 일치, 다른 repo/session/세대,
중복·누락 event, 취소 경합, read-only 호출 거절, host fallback을 해당 owner에서 검증한다.
초기 계약은 fake provider와 로컬 fixture로 검증하며 실제 provider 호출·remote host·배포
시험은 필요한 대상과 범위가 정해진 작업에서 수행한다.

이 문서 PR의 검증 범위는 참조 무결성과 문서 변경이다. Runtime 기능 시험의 통과,
네 provider의 live parity, Project 생성, plugin 설치를 문서 검증 결과로 대신하지 않는다.
해당 작업의 receipt와 Issue/PR 상태가 각 완료 주장을 뒷받침해야 한다.
