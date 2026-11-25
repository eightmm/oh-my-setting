# Entry·UI·조회 통합 계약: 항목 21–27

**상태: DESIGN / NOT IMPLEMENTED.** 이 문서는 구현 순서와 합격 조건을 정하는
독립 제안이다. 새 descriptor·응답 field·옵션은 제안이며 현재 지원 API가 아니다.
소스 기준은 `4c7db603ecf33952598406532c9213cf3aa18293` (2026-11-25)이다.
소스 링크의 줄 번호도 이 revision을 기준으로 한다. 원래 감사의 1–27 번호를 유지한다.

UI의 snapshot과 지원 기능 목록은 실행 권한이 아니다. CLI/MCP/plugin의 transport와
각 domain의 mutation owner는 유지한다. 기존 [CC-10](CALLABLE-CONSOLIDATION-AUDIT.md#cc-10-callable-catalog와-실제-owner의-대조),
[command routing](../custom-skills/oms-agent-harness/references/command-routing.md),
[work queue](ORCHESTRATOR-WORK-QUEUE.md)를 구체화하며 새 범용 router·runner·store를 만들지 않는다.
아래 열린 PR의 제안은 현재 main 구현과 구분한다.

## 21. Callable catalog와 실제 owner의 대조

**현재와 중복.** Root의 [PUBLIC/FRONTDOOR/DESCRIPTIONS](../crates/oms/src/registry.rs#L22),
[owns_invocation](../crates/oms/src/main.rs#L1159), 실제 dispatch와 crate parser가
이름·노출 범위를 별도로 선언한다. [MCP fixed argv](../crates/oms-mcp-server/src/tools.json#L3)에는
legacy script 표기가 남지만 [command adapter](../crates/oms-mcp-server/src/command.rs#L311)가
native verb로 바꾼다. 이는 Bash 실행 경로가 두 개라는 뜻이 아니다.
GUI command catalog는 [기존 `list --all`](../crates/oms-mcp-server/src/panel.rs#L1965)을
이미 호출한다. 남은 중복은 [human text 재파싱](../crates/oms-mcp-server/src/panel.rs#L2022)과
등록·도달성 계약을 별도로 유지하는 부분이다.

**선정 owner/API.** 기존 root `registry`를 public 이름·설명·frontdoor의 owner로 둔다.
제안 `CommandDescriptor`는 이름, 노출 종류, owner key, 설명을 담는 순수 metadata다.
기존 `list`에 additive JSON 출력을 제공하고 MCP catalog는 그 출력을 소비한다.
MCP schema와 허용 argv는 별도 명시적 subset이며 descriptor 이름으로 실행 권한을 얻지 않는다.

**유지할 경계.** Private/service/hook verb는 public catalog에 승격하지 않는다.
CLI 기본 text와 daily frontdoor 순서는 유지한다. MCP profile, argument 검증,
고정 executable, secret screen, pagination과 output budget은 그대로 둔다.
모든 CLI 옵션을 MCP에 자동 노출하거나 descriptor로 dispatch 함수를 생성하지 않는다.

**구현 slice.**

1. 현재 registry fixture에 public owner 도달성을 대조하는 표를 추가한다. 실행 대신
   parser의 help/입력 거절 경로로 확인하고, host가 필요한 verb는 명시한다.
2. `registry::run`에 JSON descriptor 출력을 추가하고 기존 text를 같은 descriptor에서
   만든다. `main.rs::owns_invocation`/dispatch는 검증 대상이며 별도 router로 옮기지 않는다.
3. `panel_capabilities`를 고정 JSON argv로 연결하고 `command_catalog`의 공백 기반
   파싱을 제거한다. 그다음 `tools.json`의 legacy argv 표현을 native 표현으로 바꾸고
   `run_tool`의 호환 변환을 내부 fixture와 함께 정리한다.

**테스트·합격 조건.** [registry test](../crates/oms/src/registry.rs#L380)의
`embedded_registry_covers_public_and_frontdoor_without_scripts`,
[native entry contract](../crates/oms/tests/native_entry_contract.rs),
[MCP native tests](../crates/oms-mcp-server/tests/native.rs#L1845)의 `panel_command_catalog`,
[fixed argv golden](../crates/oms-mcp-server/tests/fixed-argv.json)을 확장한다.
모든 public 이름은 owner가 확인되고, MCP에 등록된 argv는 같은 native verb에 도달해야 한다.
Private 이름은 노출되지 않으며 text·JSON·GUI의 이름/설명이 일치해야 한다.
Pagination 경계와 malformed metadata 거절은 유지하고 검증에서 provider를 시작하지 않는다.

**의존성.** 기존 CC-10 범위의 구현이다. 항목 22–23의 metadata와 원칙은 같지만
명령의 공개 여부, Main 실행 capability, desktop view를 하나의 enum으로 합치지 않는다.

## 22. Main 지원 capability의 선언

**현재와 중복.** [MainCapability](../crates/oms-command-panel/src/native_panel.rs#L44)는
Codex·Claude·AGY·Pi의 owner/transport/dependency/resume 정책을 가진다.
[desktop connections](../crates/oms-command-panel/src/native_panel.rs#L149)는 이미 그 표를
읽는다. 반면 [MCP start schema](../crates/oms-mcp-server/src/panel.rs#L306),
[runtime 검증](../crates/oms-mcp-server/src/panel.rs#L1411),
[HTML 선택지](../crates/oms-mcp-server/src/conversation-panel.html#L547)는 지원 ID를 반복한다.
Pi의 inference owner는 Codex이고 AGY의 owner 표기는 `antigravity`다.

**선정 owner/API.** Main의 실행 계약 owner는 기존 `native_panel`이다.
이 owner가 관리하는 작은 정적 `MainSurfaceDescriptor`를 지원 ID·owner·transport 종류·
dependency·account/resume 지원 표시의 단일 입력으로 둔다. Rust launch code와 MCP
정의가 같은 정적 자료를 읽고, HTML은 host가 반환한 descriptor로 선택지를 만든다.
자료는 Panel 소유 asset으로 컴파일에 포함하므로 MCP가 Panel 실행 crate 전체를
의존하게 만들지 않는다. Native prompt/argv와 custody 검사는 계속 Rust owner가 수행한다.

**유지할 경계.** 설치된 binary는 login이나 검증된 session의 증거가 아니다.
Main의 네 가지 harness, Provider Lead의 세 provider, GUI read-only assignment의
제한된 provider 집합은 서로 다른 계약이다. `agy`/`antigravity` 정규화, Pi gateway, resume 거절,
account 지원, 명시적 launch 동의는 각각 보존한다. UI 선택지 생성으로 auto-start하지 않는다.

**구현 slice.**

1. `MAIN_CAPABILITIES`의 순수 metadata를 Panel 소유 descriptor로 추출한다.
   Native transport match와 descriptor의 ID 집합이 정확히 같은지 기존 launch fixture로 확인한다.
2. MCP start schema와 request validator를 descriptor의 허용 ID로 연결한다.
   Runtime 검증 자체는 삭제하지 않으며, legacy spelling 수용은 기존 경계에서만 유지한다.
3. `desktop_connections`와 HTML harness selector를 같은 metadata에 연결한다.
   UI label/번역은 UI에 남고 설치 상태·현재 Main identity는 별도 관측값으로 유지한다.

**테스트·합격 조건.** [Main launch fixture](../crates/oms-command-panel/src/native_panel.rs#L7540)의
`explicit_main_account_managed_launch_and_reuse`,
[custody fixture](../crates/oms-command-panel/src/native_panel.rs#L8841),
[AGY Main model tests](../crates/oms-command-panel/tests/agy_main_model.rs),
[MCP tests](../crates/oms-mcp-server/tests/native.rs),
[HTML tests](../crates/oms-mcp-server/tests/conversation-panel.test.mjs#L156)를 확장한다.
정의·검증·선택지의 ID 집합은 일치하고 unknown ID는 launch 전에 거절해야 한다.
Binary만 있는 환경에서 login/session이 있다고 표시하지 않으며 stale Main 정보도 지워야 한다.
Resume 및 account 거절과 Pi gateway argv가 바뀌지 않아야 한다.

**의존성.** [#525](https://github.com/eightmm/oh-my-setting/pull/525)의 Main resume/identity와
[#523](https://github.com/eightmm/oh-my-setting/pull/523)의 AGY selector 변경을 먼저 재대조한다.
Provider identity는 [#334](https://github.com/eightmm/oh-my-setting/pull/334)를 소비하되
provider discovery 목록을 Main 지원 목록으로 사용하지 않는다.

## 23. Desktop view ID와 schema

**현재와 중복.** [desktop board VIEWS](../crates/oms-command-panel/src/desktop_board.rs#L24)의
19개 ID, [DesktopView parse/as_str](../crates/oms-command-panel/src/desktop_board.rs#L57),
[MCP VIEWS](../crates/oms-mcp-server/src/panel.rs#L42)와 schema, HTML option에 ID가 반복된다.
CLI는 [이미 board VIEWS](../crates/oms-command-panel/src/cli.rs#L398)를 사용한다.
Board의 15개 typed view와 별도 history 계열 view 네 개가 다르므로 단순 enum 개수 일치를
합격 조건으로 삼으면 안 된다. Snapshot/health/attention producer를 새로 합치는 작업도 아니다.

**선정 owner/API.** `desktop_board`가 view ID·view 종류·pagination 계약의 owner다.
제안 `DesktopViewDescriptor`는 typed board/history 종류를 구분한 정적 metadata이며
Panel 소유 asset을 CLI/MCP 정의가 공유한다. HTML은 host의 ID와 기존 UI 분류를
대조한다. Row 내용과 field allowlist는 현재 domain projection owner에 남는다.

**유지할 경계.** Page 20행·응답 크기 제한, missing/zero/empty 구분, private path·prompt
비노출, stale 응답 무시, read-only 조회를 유지한다. View 이름이 mutation 권한을 주지 않는다.
번역·화면 순서·탭 그룹을 backend 실행 enum에 넣지 않는다.

**구현 slice.**

1. 현재 19개 ID를 typed/history 종류와 함께 descriptor로 고정한다.
   `DesktopView::parse/as_str`는 typed subset을 사용하고 history dispatch는 기존 owner를 유지한다.
2. MCP `VIEW` schema·validator와 CLI 검증을 같은 ID 집합으로 연결한다.
   `desktop_board::run`의 canonical producer 호출과 projection은 변경하지 않는다.
3. HTML option과 설정 view의 reachability를 descriptor와 대조하고 오래된 수동 ID만 제거한다.
   이미 공유된 snapshot·health·attention은 재구현하지 않는다.

**테스트·합격 조건.** [board fixture](../crates/oms-command-panel/src/desktop_board.rs#L1987)의
`desktop_views_use_read_only_sources_and_keep_page_identity`,
[MCP native tests](../crates/oms-mcp-server/tests/native.rs),
[on-demand paging test](../crates/oms-mcp-server/tests/conversation-panel.test.mjs#L402),
[tab reachability test](../crates/oms-mcp-server/tests/conversation-panel.test.mjs#L1505)를 확장한다.
모든 지원 view가 기존 read-only producer에 도달하며 unknown ID를 거절해야 한다.
두 페이지의 identity, 취소 후 stale 응답 무시, 번역 key, privacy allowlist를 유지한다.
Snapshot을 열어도 provider 호출·window focus·state 쓰기가 없어야 한다.

**의존성.** 항목 22와 같은 metadata 전달 기법을 쓸 수 있지만 지원 집합은 독립이다.
MCP/HTML 파일은 한 writer가 순서대로 반영한다.

## 24. Council frontdoor와 기본 좌석

**현재와 중복.** [Peer council](../crates/oms-command-peer/src/peer.rs#L1195)은
`consult`·`advise`·`peer-ask`·review의 공통 실행 engine이다.
현재 [복수 seat 기본값](../crates/oms-command-peer/src/peer.rs#L1282)은 세 provider이며,
단일 consult/advisor는 별도 환경변수와 독립 provider 선택 정책을 가진다.
[Panel council](../crates/oms-panel-core/src/review.rs#L526)은 명시 좌석·budget을 만들고
[planning council](../crates/oms-command-autonomy/src/planning.rs#L993)도 같은 Peer 경로를 호출한다.
Engine 중복이 아니라 입력/default/route를 여러 frontdoor에서 이해해야 하는 문제다.

**선정 owner/API.** 기존 [#318](https://github.com/eightmm/oh-my-setting/pull/318)의
council defaults 계약을 canonical 작업으로 유지한다. 명시 `--providers` → 비어 있지 않은
`OMS_PEER_PROVIDERS` → optional `council.peer_default` → legacy 기본값 순서의 resolver를
Peer의 기존 council 입력에 연결한다. 명시 좌석을 가진 Panel/planning은 그 값을 보존한다.

**유지할 경계.** Consult/advisor의 단일 provider 선택, reviewer의 writer-family 경고,
required/optional seat, Team request의 한 명시 좌석·한 round 제한, exact synthesis 증거,
call budget과 `council.extra` strict schema를 유지한다. 명시 좌석이 실패했다고 default
좌석을 추가하거나 독립성 없는 결과를 독립 review로 표시하지 않는다.

**구현 slice.**

1. #318의 실제 diff/fixture를 재대조하여 중복 구현 없이 defaults resolver를 완료한다.
2. `peer::council`, `review::run`, Panel `council_execute`, Autonomy planning의 명시/default
   여부를 기존 fixture에서 대조한다. Explicit assignment는 기본값 resolver로 덮어쓰지 않는다.
3. Registry·CLI help·UI council 안내에 선택된 좌석과 근거를 같은 말로 표시한다.
   새 council 실행기나 범용 routing layer를 추가하지 않는다.

**테스트·합격 조건.** [Peer seat publication](../crates/oms-command-peer/tests/peer_contract.rs#L1138),
[planning evidence](../crates/oms-command-autonomy/tests/autonomy_contract.rs#L1297),
[Panel council catalog](../crates/oms-panel-core/tests/parity.rs#L584),
[role configuration](../crates/oms-command-agent-role/tests/native.rs#L812)을 확장한다.
우선순위 matrix에서 explicit seats가 항상 이기고, 빈 env와 malformed config를 구분해야 한다.
선택된 좌석·round·native output binding이 기존 검증을 통과해야 하며 failure 뒤에
자동 seat 추가나 추가 provider 호출이 없어야 한다.

**의존성.** #318이 구현 owner다. #334의 identity와 #335의 registry 검증은 재사용하지만
계정 선택·pinning·cooldown 및 caller별 authority는 해당 실행 owner에 남긴다.

## 25. Workflow 선택 안내

**현재와 중복.** [Graph invocation](../crates/oms-command-graph/src/execution_runner.rs#L365)은
이미 `plan-run`을 호출하고 [Graph commit](../crates/oms-command-graph/src/execution_commit.rs#L102)은
`goal-drive --commit-task`를 호출한다. [Recipe step](../crates/oms-command-state/src/recipe.rs#L1325)은
이미 Runs의 `execute_with_hooks`를 사용한다. 이 경로를 별도 실행 engine 세 개로 세어
합치면 기존 Plan claim과 process custody가 훼손된다.
[현재 command routing](../custom-skills/oms-agent-harness/references/command-routing.md#plan-and-autonomy-depth)은
이미 실행 깊이를 설명한다. 남은 작업은 catalog/help/UI의 선택 언어를 맞추는 것이다.

**선정 owner/API.** 기존 command-routing의 intent 표를 설명 계약으로 삼고 registry
설명·Panel/MCP의 workflow 안내가 이를 따른다. 새로운 runtime selector는 만들지 않는다.

| 이미 승인된 작업 | 표시할 기존 진입점 | 보존할 owner |
|---|---|---|
| 하나의 frozen ready Plan task | `plan-run` | Plan task·claim·review |
| 검토된 Plan의 반복 실행 | `goal-drive` | bounded drive·stop·commit |
| 명시적 분기 또는 기존 Graph run | Graph | Graph topology; 실제 task는 Plan |
| local command/companion recipe 재현 | Recipe/Runs | recipe 순서; Runs process custody |
| 승인된 goal을 proposal부터 진행 | `autopilot` | proposal/admission·budget·authority |

**유지할 경계.** 저장된 patch의 `--land`, delivery의 `land`, Graph commit은 같은 동작이
아니다. 기존 CLI spelling과 실행 계층을 보존한다. 안내/preview를 보았다는 이유로 claim,
provider 실행, commit, publication을 시작하지 않는다.

**구현 slice.**

1. `command-routing.md`, `COMPONENTS.md`, registry 설명에서 같은 목적에 충돌하는
   표현만 정리한다. 현재 코드 경로를 기준으로 링크하고 구현되지 않은 shortcut은 표시하지 않는다.
2. GUI workflow 설명과 help를 같은 intent 표에 맞춘다. 버튼이 있다면 기존 명시 command로만
   이동하고 임의 engine 자동 선택은 추가하지 않는다.
3. 기존 Graph/Recipe host fixture가 보여 주는 연결을 문서에 연결한다. Runtime 동작 변경이
   필요하다고 드러난 경우 별도 source 변경으로 검토하며 이 설명 통합에 숨기지 않는다.

**테스트·합격 조건.** [Graph host adapter](../crates/oms-command-graph/tests/host_adapter.rs#L211)의
dry-run과 [commit-bound fixture](../crates/oms-command-graph/tests/host_adapter.rs#L508),
[recipe fixture](../crates/oms-command-state/tests/recipe_and_gc.rs#L118)가 가장 가까운 실행 증거다.
문서만 바꾸면 reference check로 검증하고 이 fixture를 다시 통과했다고 주장하지 않는다.
UI navigation을 바꾸면 기존 HTML test만 확장한다. 각 안내는 실제 지원 argv와 일치하고
Graph→PlanRun, Recipe→Runs의 현재 연결을 새로 구현한 것처럼 서술하지 않아야 한다.

**의존성.** [#522](https://github.com/eightmm/oh-my-setting/pull/522)의 managed Plan bridge는
병합 전 현재 기능으로 안내하지 않는다. 후속 안내에서도 ready task를 미리 claim하지 않고,
native Lead와 managed Plan transport를 구분한다. Goal recovery 안내는
[#518](https://github.com/eightmm/oh-my-setting/pull/518)와 재대조한다.

## 26. 기록 검색의 읽기 facade

**현재와 중복.** [Memory handoff recall](../crates/oms-command-memory/src/db.rs#L526)은
최근 handoff의 제한된 prefix를 읽어 별도 순위·snippet을 만든다.
[Journal recorded search](../crates/oms-command-journal/src/search.rs#L212)는 handoff를 포함한
다섯 record kind를 bounded scan한다. [Archive search](../crates/oms-command-state/src/archive.rs#L567)는
cold archive의 SQLite FTS를 읽는다. 일부 대상과 검색 UI가 겹치지만 curated memory,
현행 journal/handoff, cold archive는 다른 저장소·provenance·retention 계약이다.

**선정 owner/API.** 기록 검색 의미는 기존 Journal recorded-search 계약에 둔다.
제안 `RecordedHit`는 source kind, source ref, excerpt, source별 생략/불완전 여부를 담고
검증 성공이나 현재 사실을 뜻하지 않는다. Root host의 읽기 adapter가 각 owner의 결과를
조합한다. Journal→State 의존을 추가하지 않는다; State가 이미 Journal을 의존한다.

**유지할 경계.** 각 store의 write/GC/restore owner와 기존 query/ranking은 보존한다.
기존 검색의 default source 집합과 비용은 바꾸지 않고 combined query는 명시적으로만 켠다.
Native transcript·sealed journal·network/GitHub 검색은 암묵적으로 추가하지 않는다.
Archive hit 때문에 restore하거나 검색 결과를 memory 사실로 승격하지 않는다.

**구현 slice.**

1. Journal의 hit·budget·withheld 계약을 typed read 결과로 분리하고 text/JSON 출력을 유지한다.
   Memory와 Archive는 source별 adapter로 자신의 query 의미와 provenance를 보존한다.
2. Root host에서 명시적 source 선택을 받는 combined read projection을 연결한다.
   기존 `journal show --grep`의 추가 source 옵션은 이 slice에서 additive 계약으로 검토한다.
   CLI/MCP는 같은 host projection을 소비하고 source 하나의 실패를 전체 성공으로 감추지 않는다.
3. Memory의 handoff 후보 읽기를 기존 recorded-source reader와 공유할 수 있는 최소 부분만
   연결한다. Prefix/ranking과 Journal tail/line 검색의 차이는 유지하고 중복 acquisition만 제거한다.

**테스트·합격 조건.** [Journal read-only fixture](../crates/oms-command-journal/tests/lossless_json.rs#L517)의
`read_only_status_queries_and_ascii_search_preserve_missing_and_stale_storage`,
[Archive fixture](../crates/oms-command-state/src/archive.rs#L713)의
`dedups_blobs_searches_safely_and_restores_exact_bytes`,
[Memory recall tests](../crates/oms-command-memory/tests/recall_github.rs#L219)를 확장한다.
누락된 store는 조회 뒤에도 생성되지 않고 원본 bytes는 같아야 한다. Unicode·snippet·scan
상한과 scrub을 지키며, 부분 실패/생략을 표시하고 다른 출처의 동명 hit를 합쳐 지우지 않는다.
Default 조회에 network 호출이나 provider transcript 읽기가 추가되지 않아야 한다.

**의존성.** 기존 source별 reader를 재사용한다. 항목 15의 provider history와는 별도 범위이며
그 내용을 이 facade에 자동 편입하지 않는다. 항목 19의 reference inventory도 삭제 안전성
계약이므로 검색 hit로 대체하지 않는다.

## 27. Reason·evidence·recovery의 읽기 projection

**현재와 중복.** [Adaptive decision](../crates/oms-command-autonomy/src/adaptive.rs#L664)은
receipt·Plan·budget·review·external observation으로 checked decision을 만든다.
[Panel wake](../crates/oms-panel-core/src/tick.rs#L1375)는 이미 external wait와 연결되어
owner/binding/result identity를 확인한다.
[Landing wait](../crates/oms-command-landing/src/wait.rs#L128)와
[PR observation](../crates/oms-command-landing/src/delivery/pr_observation.rs#L26)도 기존 receipt와
holder 증거를 사용한다. Wait/wake가 미연결인 것이 아니라 reason·evidence·다음 조치 설명이
여러 UI에 전달되는 형태가 다르다.

**선정 owner/API.** 기존 domain decision을 그대로 읽는 제안 `RecoveryObservation`을
root projection adapter에 둔다. Domain, reason code, observation status, evidence ref,
identity/freshness/completeness와 checked recovery 진입점 설명을 담는다.
`CheckedDecision::allows` 및 실행 시 재검증은 Autonomy, wake 판정은 Panel,
receipt/holder 변경은 Landing이 계속 소유한다.

**유지할 경계.** 이 projection은 mutation token이나 새로운 resume engine이 아니다.
Unknown/partial/expired evidence를 done으로 바꾸지 않는다. Pending external wait에서
model을 polling하거나, UI의 recovery 문구만으로 retry/claim/receipt 소비를 수행하지 않는다.
PlanRun의 delegate 복구와 role-dispatch worker의 `panel --continue`도 구분한다.

**구현 slice.**

1. Adaptive decision, Panel `wake_reason`, Landing wait 결과의 reason/evidence mapping을
   existing fixtures에서 고정한다. Raw receipt나 private path를 UI로 복사하지 않는다.
2. Root의 read projection에서 domain 결과를 공통 envelope로 감싼다. 기존 Inbox/dashboard/
   desktop detail은 표시용 envelope를 소비하고 실행 owner의 allowed action은 재계산하지 않는다.
3. 각 UI의 중복 reason 해석을 제거한다. 실제 wake 조건·cursor 소비·retry policy는 기존
   owner에 그대로 두며 표시 필드가 없거나 낡으면 unknown과 확인 경로를 보여 준다.

**테스트·합격 조건.** [Panel external-wait fixture](../crates/oms-panel-core/src/tick.rs#L5647)의
`external_pending_work_never_polls_a_model_and_terminal_wakes_once_across_days`,
[Landing exact receipt wait](../crates/oms-command-landing/tests/landing_contract.rs#L3500),
[independent PR observation](../crates/oms-command-landing/tests/landing_contract.rs#L2486),
[Adaptive feedback tests](../crates/oms-command-autonomy/tests/autonomy_contract.rs#L611)를 확장한다.
Pending 상태에 model 호출이 없고 terminal 결과는 같은 binding에서 한 번만 소비되어야 한다.
Stale holder/identity·unknown cleanup·부분 증거는 성공으로 표시하지 않으며 projection 조회
전후 receipt·lease·Plan bytes가 같아야 한다. UI 표시와 owner의 실행 허가를 분리해 검증한다.

**의존성.** #518의 checked recovery와 #522의 managed Plan 결과 의미를 재대조한다.
항목 7의 failure 분류 결과를 표시할 수 있지만 분류 통합만으로 재시도 권한을 얻지 않는다.

## 검증과 완료 판정

이 제안 PR의 검증은 번호·상대 링크·소스와 열린 PR의 경계 대조다. 위 API와 runtime 합격
조건은 후속 구현에 적용하며 이 문서 추가로 구현되거나 통과했다고 주장하지 않는다.
각 slice는 가장 가까운 기존 fixture를 확장하고 실제 consumer 연결 뒤에만 중복 선언/해석을
제거한다. 최종 source 변경의 root `./verify`와 CI 증거는 정확한 구현 head에 기록한다.
