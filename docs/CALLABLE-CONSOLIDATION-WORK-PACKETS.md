# 호출 가능한 기능의 통폐합 구현 패킷

작성일: 2026-11-18. 기준 소스:
[`f9a695ea2de90a160754252c1ee9cbaa4dd7a488`](https://github.com/eightmm/oh-my-setting/commit/f9a695ea2de90a160754252c1ee9cbaa4dd7a488).
PR 기준 main `283ab6c1`까지의 추가 변경은 문서이며 이 소스 근거를 바꾸지 않는다.
[조사와 source 근거](CALLABLE-CONSOLIDATION-AUDIT.md)를 먼저 읽는다.
**CC-01~10은 후속 구현 명세이며 이 문서 PR의 runtime 변경이 아니다.**

## 우선순위와 의존 관계

| ID | 우선순위 | 결과 | 선행·충돌 조정 |
|---|---|---|---|
| CC-02 | P0 | routing이 전체 catalog를 소비 | 독립적인 작은 fix; CC-01/03을 기다릴 필요 없음 |
| CC-04 | P0 | 광고된 output reader를 실제로 호출하고 같은 reference 사용 | 기존 capture core; native/MCP 연결과 fixture를 한 계약으로 검토 |
| CC-01 | P1 | provider identity/discovery 한 구현 | peer/providers.rs의 REL-02/03 작업과 writer 조정 |
| CC-05 | P1 | hook ownership/convergence의 production owner 하나 | 현재 installed native/legacy corpus 먼저; lifecycle required gate |
| CC-06 | P1, 단계별 | Plan Git부터 공통 supervisor로 전환 | plan → graph → task; REL-04/AR-01과 공통 API writer 조정 |
| CC-03 | P1 | account read/validation 공유 | CC-01과 API 경계 조정; preview와 실행 재선택 유지 |
| CC-10 | P1 | callable 설명·owner·parser의 정합성 | CC-04/09의 실제 지원 여부를 반영; 새 routing engine 없음 |
| CC-07 | P2 | lock cancellation watcher 제거 | lock protocol·release 의미 보존; 별도 작은 PR |
| CC-08 | P2 | current-source 집계 한 구현 | CLI raw-path와 runtime state/work root adapter 유지 |
| CC-09 | P1, host 연결 별도 | native Main swap의 실제 지원 계약 | unsupported 표시 우선; host handshake와 기존 cleanup/lineage 연결 |

P0는 이 조사에서 바로 수정할 consumer/callable 계약을 뜻한다. 기존 #259의 LC-01~04와
native assignment를 낮추는 새 우선순위 체계는 아니다. CC-06의 단계들은 별도 코드 PR로
나누고, 같은 `providers.rs`/`services_collab.rs`/supervisor를 여러 worker가 동시에 편집하지 않는다.

## CC-01 — Provider identity와 discovery

**Owner:** `oms-command-peer` provider identity를 기준으로 `oms-command-providers-skills`,
`oms-mcp-server` consumer를 연결. 의존 방향을 먼저 확인하고 pure descriptor/validation만
추출한다. 세 command crate를 서로 호출하거나 새 registry 저장소를 만들지 않는다.

**현재 경로:** peer `providers::{Target::parse, normalize, binary_path, discovered}`,
models `selection/normalize/binary`, MCP `peer::{targets, normalize}`.
Builtin/alias 목록과 custom ID/discovery가 중복이며 models의 첫 글자 검증이 다르다.

**작업과 제거 조건:**

- Alias, builtin binary, custom ID, cwd를 명시한 bounded executable discovery를 공통 API로 제공한다.
- Relative adapter directory는 owner cwd에서 해석하고 worker worktree로 옮겨도 대상이 바뀌지 않게 한다.
- 각 consumer가 같은 identity fixture를 통과하면 별도 alias/builtin/ID/discovery 구현을 제거한다.
- MCP의 model-only target/160자와 peer의 account/optional/256자 문법, role·권한·model-family 정책은 남긴다.

**Canonical fixtures:** `oms-command-peer/tests/peer_contract.rs`의
`provider_adapter_discovery_requires_marker`, `provider_native_transports_match_shell_contract`;
`oms-mcp-server/src/peer.rs`의 `canonical_provider_aliases_and_cursor_bytes`,
`tests/provider-aliases.json`, `tests/native.rs::peer_argv_goldens`.

**완료:** 모든 기존 alias, malformed leading character, `..`, relative directory, missing/nonexecuting
adapter, `opencode2` fallback을 세 consumer에서 대조한다. Discovery는 adapter 실행·인증·지원
성공의 증거가 아니며 테스트는 local fake executable만 사용한다.

## CC-02 — Full catalog consumer 수정

**Owner:** `oms-command-panel/src/commands.rs::routing_inputs`.
Producer `oms-command-providers-skills/src/models.rs`의 `catalog_models`를 재사용한다.

**작업:** 전체 field가 존재하면 전체 목록을 사용하고, field 자체가 없는 이전 producer에서만
`models`로 fallback한다. 명시적인 빈 목록/잘못된 field shape/미지원 producer의 의미를
구분한다. 표시용 20개 limit은 display에 남겨도 되며 allocation/snapshot에는 적용하지 않는다.

**제거:** 표시 field가 nonempty라는 이유로 전체 catalog를 무시하는 분기.
별도의 catalog fetcher, live refresh, model 세대 규칙 변경은 필요하지 않다.

**Canonical fixtures:** `oms-command-panel/tests/panel_parity.rs::routing_accepts_present_provider_catalog`;
`oms-panel-core/src/routing.rs`의 `schema_two_remains_strict_and_snapshots_separate_model_identities`,
`snapshots_reject_rebinding_served_replay_and_malformed_evidence`.

**완료:** 21번째 이후에만 있는 model/family도 기존 policy에 따라 route 후보가 되고 snapshot
count/hash가 전체 입력을 반영한다. Legacy fallback·explicit empty·unprobed `usable:null`을
검증한다. Catalog membership을 entitlement나 실제 served identity로 표시하지 않는다.
#259의 account-scoped capability producer와는 독립적으로 착수할 수 있다.

## CC-03 — Account read/validation 계약 공유

**Owner:** connect의 `valid_account_id/validate_shared_registry/validate_claude_registry`, panel의
`Accounts::load/configured`, peer의 `load_registry/account_environment/bind_account`.
`oms-panel-core`는 이미 peer에 의존하므로 peer가 panel-core를 import하는 방향은 금지한다.

**작업:** provider home descriptor, account ID, bounded registry decode, schema 검증,
physical home uniqueness, 입력 evidence에 대한 순수 eligibility 검사를 공유한다. Writer는
connect의 기존 ownership receipt·link lifecycle에, credential environment·resume·retry는 peer에 둔다.

**제거 조건:** 같은 registry corpus에 대한 공통 validation 결과를 확인한 후 duplicate reader
규칙만 제거한다. Preview의 quota/load와 execution의 cooldown/error penalty는 별도다.
Panel의 unpinned 선택을 `--account`로 고정하지 않고 실행 시점에 다시 평가한다.

**Canonical fixtures:** providers-skills `tests/accounts.rs`의
`same_id_across_providers_but_never_a_shared_home`, `account_id_and_home_edges`,
`preserves_preexisting_links_and_preflights_receipts_and_claude_schema`;
peer `provider_accounts_select_pin_overflow_and_validate`,
`claude_binding_refuses_credential_overrides_and_resume_guesses`,
`caller_home_variable_wins_unless_an_account_is_requested`, `retries_reselect_unpinned_accounts`;
panel `dispatch.rs::pinned_account_is_forwarded_and_unpinned_is_not`.

**완료:** malformed registry·중복 physical home·explicit pin·caller home·ambiguous resume을 검증한다.
Preview와 execution이 의도적으로 다른 account를 고르는 경우도 보존한다. Dry-run 성공을
실제 binding 증거로 쓰지 않는다. 공통 decoder를 사용해도 registry unavailable/error의 caller별
처리, Claude implicit default의 예약·표시·실행 가능성 검사를 하나의 정책으로 통일하지 않는다.
Retry/cooldown 정책 변경은 #261 REL-02/03에 남긴다.

## CC-04 — Output capture reader 연결과 reference 통합

**Owner:** `oms-output-capture`를 canonical reference/read core로 사용하고,
`oms-output::OutputDigest::retained_capture`, peer `digest::run`, MCP catalog/handler를 연결한다.

**현재 결함:** 실제 digest는 `oms_output_read`와 arguments를 반환하지만 tool은 등록되지 않았다.
`oms-output-capture`의 page/search/batch reader는 구현되어 있고 peer Cargo dependency도 있으나
production caller는 없다. `retained_capture`가 같은 filename/hash/scope 계산을 다시 한다.

**작업:**

1. `capture_reference/capture_metadata`를 reference의 한 producer로 정한다. 기존 메서드는
   compatibility wrapper로 유지할 수 있으며 digest 표현·withhold는 `oms-output`에 남긴다.
2. `oms_output_read`의 schema와 실제 handler를 함께 연결하고 `read_capture`를 호출한다.
   Root 선택·child/operation 권한·outbound 검사·typed error mapping·응답 cap은 adapter가 소유한다.
3. `read_arguments`를 그대로 사용해 동일 저장본을 읽을 수 있게 한다. Reader가 없는 환경은
   callable로 광고하지 않는다. Core/full discovery 선택과 실제 호출 지원을 별도로 정한다.

**보존:** reference는 repo/name/SHA 식별자이지 권한이 아니다. `Root::read_capture`의 no-follow,
single-link, root/path/change 검증과 저장본 SHA 확인을 유지한다. Digest의 withheld·READ_CAP·
control-character repository 거절은 compatibility wrapper/adapter에 남긴다. Reference 형식은
같지만 validation은 같지 않으므로 공통 helper 채택으로 caller의 입력 허용 범위를 넓히지 않는다.
Arbitrary path reader나 command 재실행을 추가하지 않는다. `oms_peer_result.answer_ref`와 합치지 않는다.

**Canonical fixtures:** `oms-output-capture/tests/capture_contract.rs`의
`reference_and_metadata_contract`, `page_search_and_batch`, `pinned_reference_projection_corpus`,
`real_capture_reads_and_rejects_unsafe_stores`; output의 `json_table_retrieval_and_declines`;
MCP `tests/native.rs`의 기존 tool/owned-read scenarios와 discovery/fixed-argv corpus.

**완료:** 실제 digest → 광고된 registered tool → 동일 SHA의 bounded page/search를 round-trip으로
검증한다. 정상/control-character root·foreign repo·변경·삭제·symlink/hardlink·UTF-8 page·
withheld reference 제거·exact cap과 oversize typed error를 포함한다.
앞단 capture limit/completeness와 reader availability는 구별한다.
`retrieval_unavailable`는 현재 production caller가 없지만 일반 큰 command의 오류 재현 근거는
아니다. 기존 executor의 64 MiB cap과 `--file`의 별도 경로를 유지한다. 원격 수집 #263과 별개다.

## CC-05 — Hook ownership/convergence의 production owner 정리

**Owner:** `oms-command-lifecycle::integrations::{owned_hook,hooks}`와
`oms-command-hooks::surfaces`. 실제 writer는 lifecycle transaction에 남긴다.

**작업 순서:** installed absolute native, bare native, legacy wrapper, foreign user hook,
symlinked source, unknown matcher/metadata의 corpus부터 고정한다. 현재 surfaces는 bare `oms`,
installer는 pinned absolute native command를 인식하므로 단순 함수 치환은 하지 않는다.

그 다음 current ownership 계약을 받는 순수 convergence helper를 실제 installer에 연결하거나,
사용 가치/호환 필요가 없는 exported helper를 폐기하고 필수 사례를 installer fixture로 옮긴다.
필요한 ownership 동작이 공통화된 뒤 duplicate remove/readd/owner 판정을 제거한다.
Sibling 순서와 metadata 보존 방식이 바뀌면 의도적 semantic correction으로 명시한다.

**보존:** foreign hook, native executable identity, existing `onFailure`, provider capability probe,
rollback/ownership receipt, Codex/Claude/Antigravity 각각의 event·matcher·payload. Source plugin과
native manifest의 공통 descriptor는 재사용하되 서로 다른 provider event를 합치지 않는다.

**Canonical fixtures:** hooks `tests/host_adapter.rs::surface_rows_are_native_and_legacy_rows_migrate_to_them`
및 같은 파일의 ownership 사례; lifecycle `tests/native_lifecycle.rs::native_configuration_and_services`,
`src/plugins.rs`의 convergence/ownership tests와 기존 install/doctor gate.

**완료:** repeated install no-op, duplicate owned row 제거, user row 보존, current/legacy 경로,
fail-closed 정책 및 실패 rollback을 actual production writer로 검증한다. 삭제한 helper의 테스트는
옮긴 coverage를 명시한 후에만 정리한다. No caller 검색만으로 공개 API 호환성을 단정하지 않는다.

## CC-06 — 공통 supervisor로 실행기 정리

**Owner:** `oms-supervisor::process::{Request,Reactor,Completion}`. Domain별 argv·환경·권한·결과
adapter는 유지한다. 이는 하나의 개념 패킷이며 다음 세 단계는 작은 별도 PR로 진행한다.

| 단계 | 기존 caller와 재사용 | 전환 전 동등성 조건 |
|---|---|---|
| 1. Plan Git | `repository.rs::Git::output` default 분기 → 이미 같은 파일의 `output_before` Reactor 경로 | 기본 30초와 outer absolute deadline/code 75, stdin EOF, Git 환경 제거, output cap, nonzero/interrupt/error 구분 |
| 2. Graph | `util::process_detail`, command node·Git·benchmark consumer → 같은 supervisor | stdout/stderr 각각 256 MiB, 음수 signal, timeout `(-1,out,err,true)`, final drain·cleanup; agent/tool node의 host adapter 유지 |
| 3. Task | `git::query`의 capture형 호출 먼저; `verify`는 API 보완 후 | inherited stdin, merged live `Write`, HUP/INT/TERM 전달, marker 없는 16 MiB raw tail, receipt 시점 |

Plan default 분기는 child를 기다리는 30초 제한이 있지만 별도 reader/writer join이 있다.
Descendant가 pipe를 유지하는 사례까지 같은 Reactor completion으로 감독하는 것이 첫 목표다.
Graph의 signal 값을 `shell_code()`로 일괄 변경하지 않는다. Task verify의 inherited stdin과
merged stream은 현재 `Request`/duplex와 그대로 동등하지 않다. 이 기능을 갖추기 전에는
기존 verify lane을 남기며, rendered head/tail marker를 raw tail처럼 넘기지 않는다.

**Canonical fixtures:** plan `tests/cli.rs`의 `lifecycle_from_unrelated_cwd`,
`ambient_git_environment_cannot_hijack_a_plain_directory`,
`team_contracts_preserve_parent_lineage_and_generation`; supervisor
`process_group_natural_exit_and_stop_complete_cleanly`; graph `tests/host_adapter.rs`;
task `tests/task_contract.rs::lifecycle_init_update_append_verify_close`.

**완료/삭제:** blocked stdin/stdout·descendant-held pipe·output limit·nonzero·timeout·cancel·writer
failure·trailing chunk에서 결과와 cleanup이 보존된 단계만 기존 loop를 삭제한다. 검증 실패나
불완전 capture에 성공 receipt를 만들지 않는다. Graph command-node lifecycle의 미검증 경계는
기존 host suite에 local process fixture로 보완한다. REL-04/AR-01이 같은 Reactor API를 바꾸면
공통 선행 PR을 한 owner가 만든다. Broker·remote job·managed Team cleanup 완료를 뜻하지 않는다.

## CC-07 — Lock cancellation의 watcher 제거

**Owner:** `oms-lock::{Cancellation,Lock::acquire}`의 additive API와 task `storage::locked`,
room `Store::acquire` consumer. Borrowed flag 또는 bounded predicate를 받되 caller lifetime을
넘겨 저장하지 않는다. 기존 owned `Cancellation` API도 호환 wrapper로 남길 수 있다.

**제거:** flag를 복사하기 위한 task의 10ms sleep watcher, room의 25ms park/unpark watcher와
finished flag. Lock을 기다리는 core가 직접 취소를 확인하게 한다.

**보존:** Task signal별 exit, `guard.check`, 명시적 release 오류, Room outer deadline·lock 순서,
현재 path derivation·flock/mkdir 선택·generation. Rollover/operations/installation lock은 별개다.

**Canonical fixtures:** `oms-lock/tests/protocol.rs::timeout_and_cancellation_keep_live_authority`,
`guard_lifetime_nested_callbacks_and_nonreentrancy`; task lifecycle, room
`tests/room_contract.rs::room_lifecycle_and_cursor`.

**완료:** 시작 전·대기 중·획득 직후 취소, deadline/busy/generation 교체를 검증하고 contender가
다른 owner의 guard를 해제하지 않아야 한다. Watcher 제거 수는 보고할 수 있지만 지연 개선은
측정 전 주장하지 않는다. REL-03의 새 operation deadline/cooldown 정책은 추가하지 않는다.

## CC-08 — Current-source verification 집계 공유

**Owner:** 기존 `oms-context` 또는 context 구성요소의 순수 집계 API. CLI `main::source_status`와
library `oms_runtime::context::inspect`가 caller별 source reader를 주입한다.

**작업:** selected row validation, 정렬, hash/status 집계, bounded changed/missing 목록과 omission
계산을 한 번 구현한다. `inspect_manifest/compare_manifests`는 이미 공유되므로 다시 만들지 않는다.

**보존:** CLI의 단일 `Root`/raw path/오류 순서, runtime의 `source_root` 및 execution/state repo,
cancellation, source 2 MiB cap. 표시 limit을 검사할 source 개수의 limit으로 사용하지 않는다.
전체 parser·path 확장·child policy를 통째로 합치는 작업은 아니다.

**Canonical fixtures:** `oms/tests/context_manifest_contract.rs`의 `compare_current_text_and_denials`,
`sources_beyond_display_limit_and_path_safety`; runtime
`tests/runtime_contract.rs::context_keeps_cached_memory_summary_explicit` 내부 inspection 사례.

**완료/삭제:** 같은 manifest bytes·source 관측이면 두 adapter의 `sources_current`가 같고,
missing/unreadable·unsafe path·display limit 밖 변경·interrupt가 유지된 뒤 두 loop의 중복을 제거한다.
Library 검사 통과를 별도 MCP 연결의 증거로 쓰지 않는다. GR-03/04의 retrieval 전달과는 범위가 다르다.

## CC-09 — Native Main swap host 연결

**Owner:** `oms/src/services_collab.rs`의 실제 host adapter와 기존
`oms-panel-core::release::swap_main`, `Services::swap_main_host` 계약.

**현재 상태:** parser와 core는 있지만 production override가 없고 default는 unsupported다.
기존 terminal 문서의 swap/automatic quota 흐름은 현재 지원 완료를 입증하지 않는다.

**작업:** 먼저 실제 capability가 없는 UI/CLI에서 unsupported를 명확히 표시한다. Host 구현은
기존 pane generation 확인, bounded handoff, old-native exit 확인, canonical `Room::swap_main`,
같은 pane의 replacement 시작을 기존 core에 연결한다. 새 swap state machine을 만들지 않는다.

**보존:** core가 이미 잡은 start/session lock을 재획득하지 않음; identity가 바뀌면 input을
보내지 않음; old exit가 불명확하면 replacement 재발행 금지; interrupted handoff 자동 replay 금지;
old-exit/new-start/Room lineage 세 근거가 확인되기 전 성공 금지.

**Canonical fixtures:** `oms-panel-core/tests/parity.rs::main_swap_workflow`는 core+Fake 계약을 유지한다.
실제 `CollabServices`/native entry suite에 local fake host/child로 wiring·중단·generation 교체를
검증한다. 현재 이 연결 경계의 fixture가 없으므로 기존 suite 안에서 보완할 이유가 있다.
실 provider나 실제 사용자 pane을 테스트 대상으로 쓰지 않는다.

**완료:** unsupported와 지원 성공을 구별하고 새 owner를 확인한 후에만 교체를 보고한다.
LC-03/04의 active cleanup·epoch 경로를 재사용하되 unmanaged pane을 managed Team으로 바꾸지 않는다.
#258의 model/effort launch 선택을 다시 구현하지 않는다.

## CC-10 — Callable descriptor와 owner/parser 정합성

**Owner:** `oms::registry`, `main::owns_invocation/dispatch_owned`, MCP `tools.json/command.rs` 및
기존 command parser. Runtime mutation policy를 catalog에서 재정의하지 않는다.

**작업:** public/private/service/hook 구분, canonical owner, 필수 인자, source-staged/host/provider
요구 조건을 기존 descriptor에서 대조한다. 공통 이름·owner 정보의 수동 복사만 줄인다.
MCP의 legacy `bash scripts/*.sh` descriptor는 실제 native conversion과 동등한 native 표현으로
정리할 수 있지만 공개 tool schema·conditional required validation은 독립 fixture로 보존한다.

**보존:** CLI/MCP의 다른 visibility·timeout·stdin/environment·child 정책, core/full의 discovery 의미,
private peer owner, schema/argv golden의 독립성. 이름 목록을 합쳐 새로운 권한을 부여하지 않는다.
새 모든-명령 router나 새 machine state 파일을 추가하지 않는다.

**Canonical fixtures:** `registry.rs::embedded_registry_covers_public_and_frontdoor_without_scripts`;
`oms/tests/native_entry_contract.rs::frontdoor_routes_navigation_and_native_commands_without_python`;
MCP `tests/native.rs::{discovery_protocols,fixed_argv,executor_injection}`와
`tests/definitions.json`, `tests/fixed-argv.json`.

**완료:** 83 public 이름의 owner가 중복/누락 없이 결정되고 bounded parse/help 또는 existing fixture로
진입을 확인한다. 고정 argv 23개와 동적 handler 8개의 조건부 인자도 실제 parser에 맞는다.
CC-04의 새 reader가 추가되면 변경된 수·discovery·callability를 동시에 갱신한다. 코드에서 fixture를
자동 재생성해 자기 자신과 비교하는 검증은 하지 않는다. 실제 host unavailable은 별도 표시한다.

## 구현 에이전트 전달문과 검증

[통합 작업 큐](ORCHESTRATOR-WORK-QUEUE.md)에서 lifecycle·graph·성능 작업과 충돌을 먼저 확인한다.

> 최신 main과 관련 코드 PR을 확인하고 실제 구현이 완료된 작업은 건너뛰세요. CC-02의 작은
> consumer fix와 CC-04의 producer→registered reader 계약부터 착수하세요. CC-01/03은
> provider 파일의 writer를 한 명으로 정하고, CC-06은 Plan Git부터 단계별로 진행하세요.
> 기존 callable core·canonical fixture를 재사용하고 각 PR에 실제 caller와 삭제한 중복을 적으세요.
> public helper가 존재하는 것, production wiring, fixture 성공, 실 host/provider 성공을 구분하세요.
> 단순 count·line 감소나 fixture-only API를 사용자 기능 완료로 보고하지 마세요.
> 공통화 후에도 caller별 argv/identity/authority/exit/output/cleanup 계약을 보존하세요.
> 새 hosted CI job/matrix/schedule, 유료 모델 실행, 설치·배포·계정 변경을 기본으로 추가하지 마세요.

문서 변경은 reference/diff 검사를 사용한다. 실제 구현은 가장 가까운 fixture를 먼저 확장하고,
critical core·permission·lifecycle 변경에는 저장소의 required gate를 적용한다. 기존 coverage를
옮기거나 supersede한 이유를 적으며 기능 하나마다 새 suite를 만들지 않는다. 모든 명령을 실제
provider에 실행하는 inventory 검증도 하지 않는다. 이 전달문은 merge·사용자 상태 삭제 권한이 아니다.
