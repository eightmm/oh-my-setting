# Main 제어와 세션 계약

Codex, Claude, Antigravity(AGY), Pi 중 어느 Main을 택하든 CLI·Board·MCP·UI는 같은 프로젝트의 Room, Plan, 실행 기록을 읽는다. 이 문서는 `27045b5f` 소스에서 확인한 경로와 기존 fixture의 검증 범위를 적는다. **공통 메시지 예시**는 [#432](https://github.com/eightmm/oh-my-setting/issues/432)의 adapter 설계안이다. 현행 wire 응답은 각 writer의 형식을 따른다. [관련 설계와 검증 범위](AGENT-HARNESS-WORK-PACKETS.md)

## Main별 기능과 근거

아래 표는 2026-11-25 KST의 소스 대조 결과다. **지원**은 명시한 범위의 구현, **거절**은 해당 owner의 명시적 차단, **미확인**은 그 범위에 대한 증거 부족이다. **S**는 소스, **F**는 저장소에 있는 fixture가 검사하는 범위다. 이 문서 변경에서 실공급자 세션은 검증하지 않았다. 실행 장소·시각·버전·행위·receipt가 있는 실측 증거는 같은 행위에만 적용한다. 기존 기능 표의 `partial`, `verified`, `limit`는 그대로 보존한다. [provider 기능 표](../config/provider-capabilities.json) · [Main admission과 네 경로의 fixture](../crates/oms-command-panel/src/native_panel.rs)

| 작업과 관측 범위 | Codex | Claude | AGY | Pi |
|---|---|---|---|---|
| Main 시작 | 지원 F, 새 세션·재개 stand-in | 지원 F, 새 세션과 명시 계정 stand-in | 지원 F, `agy` argv·Room | 지원 F, Pi → Codex gateway·Room |
| hook으로 busy/idle 관측 | 지원 F, 입력된 이벤트 | 지원 F, 입력된 이벤트 | 미확인 | 지원 F, Pi relay |
| Room 메시지 게시 | 지원 S | 지원 S | 지원 S | 지원 S |
| native Main에 제한된 wake 보내기 | 미확인 | 지원 F, 유휴·빈 프롬프트를 증명한 tick wake | 미확인 | 미확인 |
| 실행 중인 Main turn에 일반 steer / interrupt | 미확인 / 미확인 | 미확인 / 미확인 | 미확인 / 미확인 | 미확인 / 미확인 |
| native Main 일반 stop | 미확인 | 미확인 | 미확인 | 미확인 |
| outgoing Main 종료를 동반한 교체 | 지원 F, 검증한 idle pane에 `/exit` | 지원 F, Linux에서 운영자가 정상 종료한 뒤 검증 | 해당 Board 교체 경로 거절 S | 해당 Board 교체 경로 거절 S |
| native panel resume | 지원 F, Room·프로세스 검사 | 지원 S, `--resume` 경로 | 거절 S, native binding 미확립 | 거절 F, Pi binding 미확립 |
| 기존 tmux 세션에 attach | 지원 S, 공통 tmux 경로 | 지원 S, 동일 경로 | 지원 S, 동일 경로 | 지원 S, 동일 경로 |
| 위임 결과 envelope의 토큰·비용 | 부분 지원 F, 누적 토큰과 기준값 | 부분 지원 F, 호출 토큰과 추정 비용 | 해당 decoder 미지원 | 해당 decoder 미지원 |

- `MAIN_CAPABILITIES`와 `MainAdmission::plan`이 Main 기동·계정·resume 정책을 결정한다. `native_main_launch_preserves_physical_custody_and_unknown_evidence`는 stand-in CLI로 Codex·Claude 경로를 검사한다. `installed_pi_picker_runs_the_gateway_and_opens_the_owned_codex_lead`의 private tmux fixture에는 Pi뿐 아니라 AGY launch 확인도 있다. 새 Main의 명시 계정 선택은 Codex·Claude만 허용하며 `--account`와 `--resume`의 조합은 거절한다. [Main registry와 fixture](../crates/oms-command-panel/src/native_panel.rs)
- Room에 글을 게시한 receipt는 Room 게시의 증거다. Native 입력의 tick fixture는 Claude Main을 대상으로 한다. 작업 프로세스 그룹의 stop, 등록 job의 interrupt, Codex·Claude 교체 중 검증된 종료에는 각각 별도의 대상과 전제조건이 있다. 정상 교체는 실행 중인 turn에 대한 범용 제어권을 주지 않는다. [Room](../crates/oms-command-room/src/cli.rs) · [wake fixture](../crates/oms-panel-core/src/tick.rs) · [supervisor stop](../crates/oms-supervisor/src/process.rs) · [Main 교체 절차](TERMINAL-PANEL.md#swap-the-main)
- Board에서는 provider별로 로컬 사용량 관측을 별도로 읽는다. 예를 들어 `usage::provider_snapshot`은 AGY에 대해 기록된 quota window를 투영할 수 있다. 사용량 표의 마지막 행은 위임 결과 decoder에 한정한다. 측정된 토큰, 추정 비용, 현재 quota, 누락된 값을 구분하고 Pi 사용량을 Codex 계정에 임의 귀속하지 않는다. [사용량 decoder](../crates/oms-command-peer/src/usage.rs) · [Board 사용량](../crates/oms-command-panel/src/usage.rs)

## Connections에서 읽을 수 있는 정책

`oms panel --desktop-view connections --json`과 두 desktop MCP 프로필의
`oms_panel_tui_view`가 다음 필드를 반환한다. 행은 기동 전 로컬 호스트의 정책 관측이다.

| 행 필드 | 실제 의미 |
|---|---|
| `managedOutgoingSwap`, `swapEvidence` | Codex의 `supported` / `requires-verified-idle-pane-exit`는 교체 writer가 idle pane을 검증한 뒤 정상 종료를 요청할 수 있다는 계약이다. Claude는 기존 Linux proc 검사 결과에 따라 `supported` / `assisted-exit-linux-proc-available` 또는 `unsupported` / `assisted-exit-linux-proc-unavailable`이다. 이 경로는 운영자의 정상 종료와 세션 증거를 요구한다. AGY와 Pi는 `unsupported` / `no-managed-outgoing-adapter`다. |
| `resumeRefusalCode` | AGY와 Pi는 `binding_unverified`를 보고한다. Codex와 Claude의 `not_applicable`은 이 정적 거절 사유가 없다는 뜻이다. 실제 재개는 기존 Room과 세션의 검사를 거친다. |
| `configuredMainPermissions` | 현행 Main 설정 parser의 결과다. 미설정 기본값은 `full`, 명시 가능한 정책은 `full`과 `native`, 잘못된 값은 `unknown`이다. |
| `fullModeOption` | Codex·Claude·AGY의 native adapter에는 full 모드용 옵션이 있어 `supported`다. Pi gateway에는 해당 옵션이 없어 `unsupported`다. 실제 argv에 옵션을 포함할지는 기동 경로가 당시 정책으로 정한다. |

MCP는 이전 runtime에서 누락된 교체·권한 정책과 재개 거절 코드에 `unknown`을 제공한다. 로그인, 설치 완료, 지금 실행 중인 Main의 권한이나 호출자의 실행 승인은 별도의 근거를 요구한다. 아래 공통 메시지 예시는 계속 제안 단계다. [Native 정책](../crates/oms-command-panel/src/native_panel.rs) · [Board 열](../crates/oms-command-panel/src/desktop_board.rs) · [MCP 계약](../crates/oms-mcp-server/src/panel.rs)

`oms panel --launch <agy|pi> --resume <session> --dry-run`은 종료 코드 2와 기존 표준 오류 설명을 유지하며 표준 출력에 schema 1의 `oms-panel-action-result`를 반환한다. 여기에는 `action: "resume"`, `outcome: "refused"`, `code: "binding_unverified"`, `capabilityState: "unsupported"`, `harness`, `dispatchOwner`, `executes: false`가 있다. 이 결과에는 세션 ID나 실행 인자가 포함되지 않는다.

## 식별자, 제어권, 재접속

| 구분 | 기존 근거와 소비 방법 |
|---|---|
| Harness / dispatch owner / transport | `MainCapability`에서 각각 읽는다. Pi는 harness `pi`, owner `codex`, transport `CodexGateway`다. `desktop_connections()`가 보여주는 설치·의존성·계정 선택 정책을 로그인·응답 모델 증거로 쓰지 않는다. [registry](../crates/oms-command-panel/src/native_panel.rs) |
| 요청 모델 / 실제 응답 모델 / 계정 | 요청 모델과 해석한 launch 설정을 보존한다. 응답이 보고한 모델과 사용 계정은 그 실행의 근거가 있을 때만 채운다. 계정 alias·home의 binding은 `PersistedAccountBinding`과 `BindingAnchor`에서 확인한다. 공개 DTO에는 원본 home이나 session token을 넣지 않는다. [계정 binding](../crates/oms-command-panel/src/account_binding.rs) · [routing](../crates/oms-panel-core/src/routing.rs) |
| Host / boot / process generation | tmux `SessionProof`는 서버 pid·birth와 socket digest를 사용한다. Pane pid·birth, 필요 시 원격 host/boot 기록의 출처도 함께 확인한다. 재시작 후 pid 하나만으로 제어 대상을 복원하지 않는다. [tmux proof](../crates/oms-panel-core/src/terminals.rs) · [SSH host 관측](../crates/oms-runtime/src/ssh_adapter.rs) |
| Native session / Room participant | Native ID는 공급자가 소유한다. `native_resume_binding`은 현재 Room의 joined provider/participant와 consumer digest를 대조한다. 열거가 불완전하거나 중복·복수 Room·선택 Room 불일치이면 거절한다. 완전한 열거에서 기존 binding이 0개이면 Codex·Claude의 별도 admission에 따라 Room을 선택하거나 만들 수 있다. [resume 경로](../crates/oms-command-panel/src/native_panel.rs) |
| OMS Room / attempt / operation / task lease | Room 참여자·역할·범위, lifecycle attempt, 위임 operation, Plan의 task·lease 세대는 각각의 owner에서 읽는다. UI와 transport가 달라져도 현재 writer가 actor, scope, 이전 세대와 필요한 승인을 확인한다. [Room](../crates/oms-command-room/src/model.rs) · [Plan](../crates/oms-command-plan/src/engine.rs) · [MCP 위임 owner](../crates/oms-mcp-server/src/peer.rs) |

`attach_session`은 tmux 클라이언트를 붙인다. Room을 명시한 재연결에서 `check_reattach_room`이 그 Room과 열린 상태를 검사한다. 관측된 pane이나 외부 채팅에는 관리 권한을 부여하지 않는다. 이 경로는 repository에 live Main 한 개를 허용하며, launch/교체 시 기존 store transaction과 Main 근거를 다시 검사한다. 기존 Main을 교체하는 writer는 `native_panel::swap_main_host`다. 이미 시작된 배정의 custody를 인계할 때는 panel의 `native_succession::adopt`가 현재 Main·Room·lease·assignment를 재검사하고, panel-core의 `CheckedSuccession::parse_bound`가 해당 assignment에 묶인 증거를 검사한다. [transaction](../crates/oms-panel-core/src/storage.rs) · [교체 writer](../crates/oms-command-panel/src/native_panel.rs) · [배정 인계 writer](../crates/oms-command-panel/src/native_succession.rs) · [인계 receipt reader](../crates/oms-panel-core/src/native_succession.rs)

## 공통 메시지 예시 (제안)

다섯 객체는 독립된 **예시 DTO**다. `capability_state`는 작업의 지원 여부, `outcome`은 특정 요청의 결과다. `refused` 사유에는 `unsupported_operation`, `capability_unknown`, `generation_mismatch`, `binding_unverified` 등을 구분하는 것을 제안한다. `routing::CapabilitySnapshot`은 이미 있는 모델·effort 관측 타입이다. 아래의 Main 작업 행렬을 그 타입의 실제 wire 필드로 취급하지 않는다.

```json
[
  {"schema":1,"kind":"snapshot","project":"demo","observed_at":"2026-11-25T00:00:00Z","source_owner":"panel","generation":"g1","completeness":"partial","main":{"harness":"pi","dispatch_owner":"codex","transport":"CodexGateway","served_model":null}},
  {"schema":1,"kind":"action_request","request":"r1","project":"demo","room":"room1","action":"resume","expected_generation":"g1","idempotency_key":"r1"},
  {"schema":1,"kind":"action_result","request":"r1","outcome":"refused","code":"unsupported_operation","capability_state":"unsupported"},
  {"schema":1,"kind":"operation_receipt","request":"r2","operation":"op2","owner":"room","outcome":"accepted","receipt_ref":"msg2"},
  {"schema":1,"kind":"event","owner":"room","event_id":"msg2","operation":"op2","cursor":"c2","gap":false}
]
```

기존 writer의 오류 코드·메시지·`publication_uncertain`과 receipt를 보존한다. 응답이 유실되면 원래 project, 요청 키와 작업 범위로 상태를 먼저 조회한다. 현행 desktop Main 시작 경로의 예는 `oms_start_main` / `oms_start_status`와 원래 `requestId`다. 재시도는 그 writer가 동일 키의 중복 방지를 보장하고 상태 대조 결과 안전할 때만 허용한다. `accepted`, `delivered`, `completed`는 각기 별도의 증거가 필요하다. [기존 오류](../crates/oms-panel-core/src/lib.rs) · [현재 desktop 도구](../crates/oms-mcp-server/src/panel.rs)

버전과 `kind`를 보존한다. 기존 `SwapRecord`, `PersistedAccountBinding` 등 `deny_unknown_fields` reader는 선택 필드 추가도 거부할 수 있다. 해당 형식에는 호환되는 이전 버전 투영 또는 reader/writer 버전 전환·fixture를 먼저 준비한다. 제안 DTO가 모르는 상태를 읽으면 미확인으로 표시하고, writer가 판별할 수 없는 요청은 실행하지 않는다. UI는 이벤트 cursor의 누락 구간을 표시하고 원본을 다시 조회한다.

## 다른 PR과 남은 수락 조건

- [#440 §9](https://github.com/eightmm/oh-my-setting/blob/238c332ab734fb971e9d6fe1b3b7453977d94a01/docs/CONTROL-SURFACE-ROADMAP.md)와 같은 용어를 사용한다. 원격 gateway는 MCP 연결과 caller/project 권한을 담당할 후보이고, Pi의 Codex 추론 경로는 별개다. Supervisor/SSH는 host 관측, 기존 native adapter는 공급자 호출, 플러그인은 표시·요청을 담당한다. Plan·Agents·Logs·Review의 후속 화면은 같은 writer의 projection과 receipt를 사용하도록 제안한다.
- [#334 diff](https://github.com/eightmm/oh-my-setting/pull/334/files)의 `provider_identity::canonical`은 `agy`를 `antigravity`로 정규화하고 `Discovery`는 builtin과 custom adapter의 실행 파일을 찾는다. peer·models·MCP 호출부가 이를 공유한다. [#335 diff](https://github.com/eightmm/oh-my-setting/pull/335/files)는 #334에 쌓여 `provider_accounts`의 ID·home·중복 검사를 공유한다. routing/connect의 `Strict`와 peer의 `PeerLegacy` 해석 차이는 공통 fixture가 보존한다. 이 PR들이 반영되면 해당 parser와 fixture를 재사용하고 Main admission과 계정 파일의 물리적 검증은 기존 owner가 담당한다.
- 현재 Connections와 재개 dry-run에는 위 정책 및 거절 계약을 적용했다. 호환성 fixture는 이전 응답과 미검증 값을 다룬다. 공통 action·event 어댑터와 host/version별 실제 공급자 증거는 각 writer와 해당 통합 작업에서 다룬다. 이 문서로 선행 프로젝트의 기능 표나 다른 화면의 실행 검증을 대체하지 않는다. [#432 수락 조건](https://github.com/eightmm/oh-my-setting/issues/432)
