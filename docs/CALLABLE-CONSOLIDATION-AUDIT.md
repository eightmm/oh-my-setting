# 호출 경로와 중복 기능 통폐합 감사

작성일: 2026-11-18. 소스 기준은
[`f9a695ea2de90a160754252c1ee9cbaa4dd7a488`](https://github.com/eightmm/oh-my-setting/commit/f9a695ea2de90a160754252c1ee9cbaa4dd7a488)이다.
PR 기준 main은 [`283ab6c1`](https://github.com/eightmm/oh-my-setting/commit/283ab6c1f6dd7dfbe0ab5f3bd41a5d3455be452e)이다.
두 revision 사이의 #259~#264 병합은 문서만 바꾸었으므로 아래 runtime source 근거는 동일하다.
이 문서는 **정적 조사와 후속 구현 명세**다. 아래 결함의 runtime 수정이나 native host 연결이
완료되었다는 뜻이 아니다. 현재 설치본의 동작은 그 설치 revision을 별도로 확인해야 한다.

**2026-11-19 갱신:** 이 tree는 CC-04의 canonical reference와 MCP output reader를 연결한다.
[최신 근거와 구현 범위](TOOL-CONTEXT-REVIEW.md)를 함께 읽는다. 아래 31/full·12/core 집계와
미등록 결함은 위 기준 revision의 감사 기록이며, 이 변경 이후 목록은 32/full·13/core다.

## 1. 결론과 조사 기준

재사용 가능한 Rust core와 실제 호출 경로는 이미 많다. 우선할 일은 새 범용 실행기나
통합 DB를 추가하는 것이 아니라 **중복된 판단을 한 곳으로 모으고, 기존 core와 public
consumer 사이의 빠진 연결을 완성하는 것**이다. 다음 네 종류를 구분한다.

| 판정 | 이번 조사에서 확인한 예 | 조치 |
|---|---|---|
| 같은 계약의 중복 구현 | provider identity/discovery, output capture reference, Git 실행 분기, lock 취소 전달 | 같은 owner의 core를 호출하고 adapter는 기존 외부 계약을 보존 |
| 이미 있는 값의 잘못된 소비 | 전체 model catalog가 있는데 표시용 앞 20개를 routing에 전달 | producer 재작성 없이 consumer 수정 |
| core는 있으나 호출 경로가 빠짐 | `oms_output_read` 미등록, native `swap_main_host` 미연결 | 실제 등록/host 연결과 unsupported 표시를 함께 처리 |
| 목적이 다른 여러 표면 | CLI/MCP, task/plan, Main/worker, preview/execute | 진입점·권한·저장소는 유지하고 필요한 순수 로직만 공유 |

함수 이름·코드 줄 수만으로 중복을 판정하지 않았다. 직접 caller, 입력 문법, producer,
반환값의 consumer, authority와 가장 가까운 fixture를 대조했다. Workspace 52개 member의
전체 함수에 대한 동등성 증명은 아니며, 아래 호출 경로를 중심으로 한 조사다.

## 2. 호출 표면의 실제 크기

| 표면 | 기준 revision의 정적 집계 | 해석 |
|---|---:|---|
| `oms list --all`의 public top-level verb | 83 | `registry::PUBLIC`의 고유 이름 수; 하위 명령·private helper 제외 |
| 기본 `oms list`의 frontdoor | 13 | `FRONTDOOR`의 분류 제목을 뺀 이름 수 |
| MCP tool 정의 | 31 | `tools.json`의 entry 수; CLI 83개와 일대일 관계가 아님 |
| MCP 기본/core discovery | 12 | `CORE`에 있는 이름만 `tools/list`에 표시 |
| MCP full discovery | 31 | 같은 handler 집합을 더 넓게 보여 줌 |
| MCP literal fixed-argv descriptor | 23 | `tools.json`에서 `argv` field가 있는 entry; 나머지 8개는 별도 handler |
| MCP resource / prompt | 1 / 0 | graph UI resource; prompt method는 구현되지 않음 |
| hook adapter 이름 | 21 | `oms-command-hooks::HOOKS`; 일반 public verb 목록과 다른 설치 표면 |

집계 원천은 [root registry][registry], [MCP 등록·dispatch][mcp],
[tool 정의][tools], [hook 소유 목록][hook-list]이다. Skill 문서는 호출을 안내하며
실행 entrypoint 자체가 아니다. `list`의 숫자가 실제 설치·인증·host 지원 성공률은 아니다.

**MCP core/full은 discovery 범위다.** `tools/call`은 두 profile 모두 전체 31개 정의에서
이름을 찾고, 이후 요청별 validation과 child/operation 경계를 적용한다. Core에서 목록에
안 보이는 것을 권한상 금지된 기능으로 해석하거나, 통폐합 과정에서 권한 검사를 목록
필터로 대체하면 안 된다. 반대로 등록되지 않은 `oms_output_read`는 두 profile 모두
unknown tool이 된다(`lib.rs:454–459`).

## 3. 먼저 재사용할 호출 경로

다음은 source/parser로 확인한 예시다. 이번 조사에서 명령을 실행하거나 provider를
호출하지 않았다. `--dry-run`도 준비 artifact를 만들 수 있으며 account binding·실제
모델 사용 가능성의 증거가 되지 않는다. `ROOM_ID`, `TASK_ID`, `MANIFEST`는 기존 값을 뜻한다.

| 필요 | 기존 호출 예 | 재사용 원천과 현재 한계 |
|---|---|---|
| 기능 탐색 | `oms list`, `oms list --all` | `registry`; helper/host 지원 여부와 별개 |
| 현재 주의 항목 | `oms inbox --repo . --json` / MCP `oms_inbox` | 기존 state projection을 재사용; 수리 권한과 분리 |
| 저장된 provider catalog | `oms models --providers codex --json` | `models::run`; 새 live probe 없이 기존 상태 조회 |
| route preview | `oms panel --repo . --routes --json` | `routing_inputs` → `NativeCommand::ProviderCatalog` → `models`; CC-02의 20개 잘림 있음 |
| provider 호출 준비 | `oms agent-call --to gemini-cli --prompt '호출 경로 확인' --dry-run` | peer `Target::parse`/provider core; 실제 provider 실행은 별도 |
| panel의 read worker 준비 | `oms panel --repo . --dispatch scholar --access read --task-id reuse-audit --prompt '호출 경로 조사' --dry-run` | 기존 dispatch → peer; task/Main/admission 검사는 실제 실행에서 유지 |
| 구조 검색 | `oms graph project find --json --limit 40 QUERY` / MCP `oms_project_graph_query` | 같은 graph owner; 검색 정확성과 worker 전달은 #264 |
| bounded context 생성 | `oms runtime context --target PATH` | `oms_runtime::context::plan`; manifest/bundle을 쓰는 생성 작업 |
| 저장된 manifest 검증 | `oms runtime context-inspect MANIFEST --verify-current` | main의 `source_status`; bundle 재실행 없이 source hash 검사 |
| 작업 evidence | `oms runtime evidence show`, `oms runtime next` | `oms_work_inspect`의 기존 projection; 완료/lease writer가 아님 |
| 저장 가능한 출력 요약 | `oms output-digest --json -- COMMAND ARG...` | peer digest → `oms_output`; command 실행과 capture 저장. 광고된 reader는 CC-04에서 연결 필요 |
| Main 교체 | `oms panel --repo . --room ROOM_ID --swap-main claude` | CLI와 core는 있으나 native host는 unsupported; 현재 사용할 수 있는 완료 경로로 소개하면 안 됨 |

Panel의 worker 경로는 이미
`dispatch_command → routing::allocate → dispatch::command → NativeCommand → CollabServices → peer::run_with_executor`다.
새 panel 전용 provider runner를 추가할 이유가 없다. [native 연결][collab]과
[dispatch adapter][dispatch]가 근거다. Native interactive Main/orchestrator는 prompt,
resume, permission, child marker, read isolation이 다르므로 peer 실행 모드에 강제 통합하지 않는다.

Context compiler도 이미 graph `project_context.rs:655`와 peer `compiled_context.rs:70`에서
같은 `oms_runtime::context::plan`을 호출한다. Work inspector는 task `projection.rs:42`,
plan `query.rs:895`, runtime `projection.rs`/`router.rs`, panel `state.rs:177`에 연결되어 있다.
이들을 미사용 core나 새 기능 누락으로 다시 계산하지 않는다. [기존 compiler][compiler],
[graph consumer][graph-context], [peer consumer][peer-context], [work inspector][work-inspect]를 재사용한다.

## 4. 확인된 통폐합·연결 후보

구체적인 owner, 삭제 조건, canonical fixture와 완료 조건은
[구현 패킷](CALLABLE-CONSOLIDATION-WORK-PACKETS.md)에 있다.

### CC-01: provider identity와 discovery

[Peer][providers] `providers.rs:85–220`, [model 조회][models] `models.rs:8–141`,
[MCP peer][mcp-peer] `peer.rs:16–89`가 alias·built-in binary·custom adapter discovery를
각자 구현한다. Models의 custom ID 검사는 첫 글자 alphanumeric 조건이 없고, peer와 MCP는
그 조건을 요구한다. 실행 가능한 `oms-agent-adapter--fixture`가 있으면 models가 `-fixture`를
받아들이고 나머지는 거절할 수 있다는 **소스상 반례**다. 설치·실행 재현은 하지 않았다.

공유 대상은 identity, ID 문법, builtin mapping, 명시적 cwd 기준 discovery다. MCP의
`PROVIDER[:model=...]`와 peer의 account/optional 문법, model 길이, 권한은 각 adapter에 남긴다.

### CC-02: 전체 catalog 대신 앞 20개만 사용하는 consumer

[Producer][models] `models.rs:437–453,527`은 `catalog_models` 전체와 `models` 표시용
`take(20)`를 함께 반환한다. [Panel][panel-commands] `commands.rs:988–996`은 `models`를
먼저 쓰고 비었을 때만 전체 목록을 읽는다. 따라서 21개 이상일 때 routing 입력이 잘린다.
그 뒤의 특정 모델 선택 실패 여부는 policy와 catalog 내용에 따라 달라진다.

전체 필드의 존재 여부를 먼저 확인하고, 구형 producer에만 표시 필드 fallback을 적용한다.
명시적으로 비어 있는 전체 목록을 과거 표시 목록으로 채우지 않는다. 이 변경은 account별
entitlement나 실제 served model을 입증하지 않으며, 새 probe를 필요로 하지 않는다.

### CC-03: account schema 검증과 preview/execute 분리

[Connect][connect]의 ID/registry validation, [panel routing][routing]의 `Accounts::load`,
[peer][providers]의 `load_registry`/`account_environment`가 동일 registry를 따로 해석한다.
순수 schema·ID·physical home 충돌·provider home descriptor는 공유할 수 있다.

그러나 panel preview의 quota/load와 peer 실행의 cooldown·retry·credential binding은
같은 결정이 아니다. [Dispatch][dispatch] `dispatch.rs:347–351`은 pinned account만
`--account`로 전달한다. Unpinned preview를 실행 pin으로 바꾸지 않는다. Registry 오류를
account 정보 부재로 처리하는 panel과 오류를 전파하는 peer, implicit default의 표시/실행 검사도
각 adapter의 정책으로 남긴다.

### CC-04: advertised output reader가 등록되지 않음

[Peer digest][digest] `digest.rs:54–106`은 command 결과를 redaction 후 저장하고
[OutputDigest][output] `retained_capture`를 호출한다. 이 메서드는 `output:name:sha:scope`
참조를 계산하여 `read_tool: oms_output_read`를 반환하지만 [MCP 정의][tools]와 root public
verb 목록에는 대응 reader가 없다. `oms_peer_result`의 operation-owned answer와는 다른 계약이다.

[이미 있는 capture core][capture]는 `capture_reference`, `capture_metadata`,
`validate_request`, `project_snapshot`, `read_capture`를 제공한다. 저장본 SHA 검증,
UTF-8 page/search/batch도 구현되어 있으나 production caller를 찾지 못했다. Peer의 Cargo
의존성만 존재한다. `retained_capture` 안의 reference 생성은 같은 wire identity의 중복이다.

기존 core에 얇은 reader adapter를 연결하고 한 reference producer를 사용한다. Reference는
읽기 권한이 아니며 root·권한·outbound·응답 상한은 adapter가 확인해야 한다. Reference
형식은 같아도 validation은 다르므로 digest의 control-character root·withheld·READ_CAP 거절을
compatibility wrapper에 보존한다. 일반 command의 64 MiB 초과가 곧바로 metadata 오류를 만든다는
재현 주장은 하지 않는다. 앞단 supervisor도
64 MiB를 제한하므로 capture completeness와 retrieval availability를 따로 검증해야 한다.

### CC-05: hook ownership/convergence의 두 구현

[Hooks core][hook-surfaces]의 `surfaces::converge/remove/owned_script`는 repository 안에서
테스트 caller만 확인된다. 실제 installer는 [lifecycle integrations][integrations]
`owned_hook`과 `hooks`로 기존 OMS row를 지우고 다시 추가한다.

두 구현은 바로 교환할 수 없다. Surfaces는 bare `oms VERB`를 인식·출력하지만 installer는
설치된 absolute native 경로를 사용한다. 한 구현은 sibling 순서·entry metadata를 보존하고,
다른 구현은 remove/readd한다. 먼저 현행 ownership·설치 경로·capability 계약을 고정한 후
공유 core를 채택하거나 미사용 compatibility helper를 폐기한다. 같은 이름의 사용자 hook을
OMS 소유로 확대 판정하면 안 된다.

### CC-06: subprocess lifecycle의 단계적 통합

[Plan Git][plan-git]의 `Git::output`은 outer deadline이 있으면 이미 Reactor를 사용하고,
없으면 별도 spawn/reader/writer/kill/wait를 사용한다. 기본 분기에도 30초 child 대기 제한은
있지만 I/O thread join과 descendant pipe를 같은 완료 계약으로 감독하지는 않는다.
이는 정적 위험 분석이며 hang을 실행 재현한 결과가 아니다.

[Graph process][graph-util]와 [task process][task-process]에도 별도 process-group/drain
구현이 있다. 기존 [supervisor][supervisor]를 재사용하되 plan → graph → task 순서로 전환한다.
Graph의 음수 signal/timeout tuple, task의 inherited stdin·merged live output·raw tail은
서로 다르다. 현재 Reactor API로 task verify를 그대로 치환할 수 없으므로 API 동등성 확보
전에는 해당 lane을 유지한다.

### CC-07: lock 취소 flag 전달의 watcher 중복

[Task storage][task-storage] `locked:395`와 [room storage][room-storage] `acquire:532`는
`AtomicBool`을 `oms_lock::Cancellation`에 복사하려고 각각 watcher thread를 만든다.
[Lock core][lock]는 이미 취소를 확인하며 대기한다. Borrowed cancellation 입력을 공유하면
이 전달 thread만 제거할 수 있다. Lock namespace, owner generation, release, deadline,
room rollover와 operations lock을 합치는 작업은 아니다.

### CC-08: saved context의 current-source 집계

[Main][main] `source_status:684`와 [runtime context][compiler] `inspect:646`은 같은
unchanged/changed/missing/unreadable 및 bounded path 집계를 별도로 구현한다.
공통 집계에 caller별 source reader를 전달하되 execution/state repo, raw path, 오류 순서와
취소 정책은 보존한다. 현재 공개 CLI는 main 경로를 사용한다. Library inspector의 확인된
직접 caller에는 runtime fixture가 있으며, 별도 MCP public inspector로 소개하지 않는다.

### CC-09: Main swap의 빠진 host adapter

[Panel][panel-commands] `--swap-main` → [release::swap_main][release] →
[Services::swap_main_host][panel-services] 경로는 있다. 그러나 실제 [CollabServices][collab]
`11–14`는 미구현 default를 유지하며, default는 unsupported 오류를 낸다. Parity Fake의
`main_swap_workflow`는 실제 host 연결의 증거가 아니다.

새 state machine 대신 기존 lock·old exit·replacement identity·Room lineage 검사에 host
handshake를 연결한다. 문서의 swap/automatic quota 설명도 현재 지원 상태와 구분한다.
#258의 새 orchestrator launch 선택과 LC의 managed Team cleanup을 완료했다고 간주하지 않는다.

### CC-10: callable catalog와 실제 owner의 대조

Root의 `PUBLIC/FRONTDOOR/DESCRIPTIONS`, `owns_invocation`, 실제 dispatch, 각 crate의 parser,
MCP schema/argv와 skill 안내가 같은 이름·owner 정보를 따로 유지한다. 현재 registry test는
설명과 frontdoor 포함 관계를 확인하지만 전체 public 이름의 parser 도달성을 증명하지 않는다.

MCP의 23개 fixed argv descriptor에는 legacy `bash scripts/*.sh` 표기가 남아 있다. 실제
[command adapter][mcp-command] `command.rs:311–326`은 이를 native verb로 바꾸므로 Bash/Python
실행의 증거로 쓰면 안 된다. Public/private/service/hook, owner, 필수 인자와 host 필요 조건을
기존 descriptor와 fixture로 대조하고, fixed argv를 native 표현으로 정리할 때 schema와
공개 응답의 golden은 독립적으로 보존한다. 새 범용 router나 이름으로 권한을 추론하는 표는 만들지 않는다.

### 기존 graph 작업에 연결할 adapter 재사용

CLI의 ranked search는 MCP `oms_project_graph_search`에 노출되지 않고, MCP trace는
`out`/depth 2로 고정되어 있다. CLI `runtime envelope/evidence matrix/next/brief`도 별도 MCP
도구는 없다. 이는 core 부재가 아니라 transport별 노출 범위다. 필요한 bounded 옵션만 기존
owner에 전달하도록 결정하며, 모든 CLI 명령을 MCP로 복제하지 않는다.

MCP [symbol slice][mcp-extra]는 별도 regex와 `block_end`를 사용하고 첫 동명 정의를 고른다.
#264 GR-01의 정확한 identity/span 이후 이를 소비하도록 연결한다. Explicit line range,
safe-fs, secret screen은 유지하고, line clipping과 각 줄의 200-character clipping도 구분한다.
별도의 세 번째 Rust parser 패킷으로 확장하지 않는다.

## 5. 통합하지 않을 경계

| 겉으로 비슷한 기능 | 유지할 구분 |
|---|---|
| `agent-task` / `agent-plan` / Room | resume note / 실행 task·claim / 협업 메시지와 lineage; 하나의 task DB로 합치지 않음 |
| `autopilot` / `goal-drive` / `plan-run` | 이미 실행 계층; 병렬 scheduler로 해석하지 않음 |
| `patch-admit` / `patch-land` / `land` | admission preview / worker patch 적용 / committed HEAD gate·publication |
| journal / memory / handoff / capsule | 관찰 기록 / curated 사실 / 로컬 session / sanitized 전송; retention·authority 별도 |
| native Main / provider orchestrator / peer worker | interactive host와 managed 실행의 permission·custody·resume 차이 |
| CLI / MCP / hook | transport·설치·child 제한과 오류 응답은 유지; shared core만 재사용 |
| `oms_context::Json` / `oms_json::Value` / serde | raw numeric spelling·WTF-8·순서·nonfinite·integer 한계가 다름; 전면 parser 통합 제외 |
| `Root::atomic_mutate` 주변 domain wrapper | task/plan/state/operations는 이미 safe-fs를 사용; CAS·writer lock·fsync·receipt 순서 유지 |
| 짧은 hash/time wrapper | 공통 crate 신설의 근거가 아님; 필요한 adapter를 삭제해 결합도를 늘리지 않음 |

[기존 command routing](../custom-skills/oms-agent-harness/references/command-routing.md)과
[JSON contract][json]가 위 경계를 설명한다. Scrub의 scan/refuse와 redact/retain,
사용자 memory의 tombstone 삭제도 같은 이름만 보고 하나로 합치지 않는다.

## 6. 기존 PR과 검증 범위

[#259](https://github.com/eightmm/oh-my-setting/pull/259)의 lifecycle·native assignment가
기존 핵심 큐다. [#260](https://github.com/eightmm/oh-my-setting/pull/260)의 Plan snapshot·I/O,
[#261](https://github.com/eightmm/oh-my-setting/pull/261)의 retry/deadline/Broker,
[#262](https://github.com/eightmm/oh-my-setting/pull/262)의 CI 효율,
[#263](https://github.com/eightmm/oh-my-setting/pull/263)의 remote artifact,
[#264](https://github.com/eightmm/oh-my-setting/pull/264)의 retrieval 정확성 명세는 반복하지 않는다.
이번 output capture는 local 저장본이며 remote collection의 대체가 아니다.
전체 우선순위와 같은 파일의 writer 조정은 [통합 작업 큐](ORCHESTRATOR-WORK-QUEUE.md)에 연결했다.

조사에서는 public parser와 직접 caller, 등록 목록, source-backed counterexample와 canonical
fixture를 확인했다. Rust toolchain/native OMS가 없는 환경이므로 Rust 테스트, MCP round-trip,
provider/account/installed host 동작, 처리량·지연 개선 수치는 실행 검증하지 않았다.
문서 변경의 검증과 remote PR 상태는 PR 본문에 별도로 기록한다.

[registry]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms/src/registry.rs
[mcp]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-mcp-server/src/lib.rs
[tools]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-mcp-server/src/tools.json
[hook-list]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-command-hooks/src/lib.rs
[providers]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-command-peer/src/providers.rs
[models]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-command-providers-skills/src/models.rs
[mcp-peer]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-mcp-server/src/peer.rs
[panel-commands]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-command-panel/src/commands.rs
[connect]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-command-providers-skills/src/connect.rs
[routing]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-panel-core/src/routing.rs
[collab]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms/src/services_collab.rs
[dispatch]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-panel-core/src/dispatch.rs
[compiler]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-runtime/src/context.rs
[graph-context]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-command-graph/src/project_context.rs
[peer-context]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-command-peer/src/compiled_context.rs
[work-inspect]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-work-inspect/src/lib.rs
[digest]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-command-peer/src/digest.rs
[output]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-output/src/lib.rs
[capture]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-output-capture/src/lib.rs
[hook-surfaces]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-command-hooks/src/surfaces.rs
[integrations]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-command-lifecycle/src/integrations.rs
[plan-git]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-command-plan/src/repository.rs
[graph-util]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-command-graph/src/util.rs
[task-process]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-command-task/src/process.rs
[supervisor]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-supervisor/src/process.rs
[task-storage]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-command-task/src/storage.rs
[room-storage]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-command-room/src/storage.rs
[lock]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-lock/src/lib.rs
[main]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms/src/main.rs
[release]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-panel-core/src/release.rs
[panel-services]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-panel-core/src/services.rs
[json]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-json/README.md
[mcp-command]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-mcp-server/src/command.rs
[mcp-extra]: https://github.com/eightmm/oh-my-setting/blob/f9a695ea2de90a160754252c1ee9cbaa4dd7a488/crates/oms-mcp-server/src/extra.rs
