# 후속 연구: 구현 경계와 검증 계약

**상태: DESIGN ONLY — 아래 기능은 이 문서로 구현되거나 활성화되지 않는다.**
조사일은 2026-11-17, OMS 코드 기준은
`3cdd3e74f22d286d4a779013f62c11ee537e9ecf`다. `RF-*`는 제안 PR 단위이며
필드와 API 이름도 설계상의 이름이다. 현재 사용할 수 있는 명령이나 설정으로
해석하지 않는다. 이 변경은 runtime 코드, dependency, provider 기본 경로를 바꾸지 않는다.

1–9절은 위 조사 시점의 설계 기록이다. [10절](#10-2026-11-21-하네스-강화의-현재-경계)은
2026-11-21에 현재 source와 협업 PR을 다시 대조한 보강이다. 기존 RF/REL/DR/VW owner에
실행 품질·연속성·검증 과제를 연결하며, 별도 구현 PR과 남은 제안을 구분한다.

남은 과제는 세 가지다. 기존 supervisor 위의 선택적 ACP 연결, 실제 실행 중
증거에 근거한 tool-loop/stall 관측, 호출별 context 출처와 동일 과제의 짝 비교다.
별도 PR에서 다루는 feedback 수집·repair와 사용량 영수증·예산,
기존 context inspection을 재구현하지 않는다.

이 문서는 **관측**을 읽은 코드·공식 문서의 사실, **해석**을 그 사실에서 도출한
설계 판단, **제안**을 앞으로 구현하고 검증해야 할 계약으로 구분한다.
외부 저장소 전체 감사나 독립 모델 검토를 수행했다는 뜻이 아니다. 아래에 명시한
모듈과 문서만 조사했으며 실제 유료 provider, GPU 또는 원격 작업은 실행하지 않았다.

## 1. 확인한 OMS 경계

| 관측 | 근거 | 아직 채워지지 않은 경계 |
|---|---|---|
| `Request.stdin`은 `Option<Vec<u8>>`이며 `write_input`은 그 버퍼를 다 쓰면 stdin을 닫는다. reactor가 자식 회수와 종료 증거를 소유한다. | [supervisor process](../crates/oms-supervisor/src/process.rs), `Request`, `Job::write_input`, `Completion` | 실행 중 받은 요청에 따라 새 응답을 보내는 bounded duplex 채널 |
| Peer의 `Executor::execute`는 `Completed`를 반환한다. `Native`는 `Progress::Pending`을 외부에 전달하지 않고 완료 시 출력과 상태를 반환한다. | [peer process](../crates/oms-command-peer/src/process.rs), `Invocation`, `Native::execute` | provider 실행 중 event 소비·응답·취소의 typed 경계 |
| Root 서비스의 `ChildSpec.stdin`도 고정 `Vec<u8>`이며 `run_child`는 기존 reactor를 구동한다. | [root services](../crates/oms/src/services_runtime.rs), `ChildSpec`, `run_child` | 새 adapter가 기존 감독을 우회하지 않고 같은 lifecycle을 쓰는 연결부 |
| Provider는 실행이 끝난 뒤 `decode_output`을 호출한다. Custom route는 `oms-agent-adapter-*` 이름과 별도 write opt-in 경계를 사용한다. | [providers](../crates/oms-command-peer/src/providers.rs), `binary_path`, `transport`, `execute_env` | custom discovery 경계 안에서 명시적으로 고른 ACP transport 한 개 |
| Goal의 `stalled_cycles`는 tree 변화·닫힌 task로 계산하며 campaign은 wave 단위로 멈춘다. | [goal](../crates/oms-command-autonomy/src/goal.rs), `progress`; [campaign](../crates/oms-command-autonomy/src/campaign.rs), `wave_completed` | 한 provider 호출 내부의 현재 tool 상태·반복·대기 원인 |
| Context compiler는 source별 `bytes_before`, `bytes_selected`, `omitted_bytes`, selection과 digest를 저장한다. 기존 inspector는 주입량, 도구 결과 크기와 사후 타임라인도 읽는다. | [context compiler](../crates/oms-runtime/src/context.rs), `plan`; [inspection 계약](CONTEXT-INSPECTION.md) | 실제 호출에 연결된 계층별 출처, provider가 추가한 context의 미측정 범위 |
| Benchmark는 model/context mode별 cohort와 snapshot 비교를 제공하고, cohort가 인과적 절감 증거가 아니라고 표시한다. | [benchmark](../crates/oms-runtime/src/benchmark.rs), `context_modes`, `snapshot`, `compare` | 사전 고정한 동일 과제의 paired 실행·품질 평가·실패 포함 비교 |

**해석:** 완료된 artifact나 저장된 transcript를 다시 읽는 것으로 실행 중 제어를
구현했다고 말할 수 없다. 또 context bundle의 byte 차이만으로 provider의 token,
청구액 또는 작업 성공률 변화를 알 수 없다. 다음 단계는 이 관측 경계를 확장하는 작업이다.

## 2. 선행 PR과 결합 경계

조사 시점에 아래 번호는 공개된 PR이며, 이 문서의 기준 commit에는 포함되지 않는다.
구현 PR은 병합된 실제 dependency HEAD를 다시 고정한다.

| 선행 작업 | 후속 설계에서 재사용할 계약 | 중복하거나 확대하지 않을 부분 |
|---|---|---|
| [#238 PR/CI feedback observations](https://github.com/eightmm/oh-my-setting/pull/238) | 관측과 권한을 분리하는 방식, bounded 수집과 source identity | provider live event를 PR 댓글로 보내거나 feedback 수집기가 자식을 제어하지 않는다. |
| [#239 reviewed feedback repair](https://github.com/eightmm/oh-my-setting/pull/239) | 관측을 실제 repair로 연결할 때의 reviewed binding과 제한된 continuation | stall 경고는 repair 승인이나 새 outbound 권한이 아니다. |
| [#240 canonical provider usage](https://github.com/eightmm/oh-my-setting/pull/240) | 물리 호출별 `call_id`, 공통 `operation_id`, revision·session·cost provenance, unknown liability | ACP나 benchmark 전용 비용 장부를 만들지 않는다. |
| [#241 campaign soft admission](https://github.com/eightmm/oh-my-setting/pull/241) | 이전 시도의 settled usage와 active/unknown reservation을 포함한 admission | 관측 timeout이나 hold로 예산을 환불하지 않는다. Provider hard cap으로 표현하지 않는다. |
| [#242 Apptainer runtime](https://github.com/eightmm/oh-my-setting/pull/242) | 해당 backend로 benchmark할 때 image/backend identity와 실제 enforced capability | ACP 자체를 sandbox라고 주장하거나 local fixture에 Apptainer를 필수화하지 않는다. |
| [#243 resumable artifact collect](https://github.com/eightmm/oh-my-setting/pull/243) | 원격 benchmark 결과를 받을 때 기존 run identity, 크기·digest 검증, 재개 계약 | 새 전송기나 암묵적 원격 재실행을 추가하지 않는다. |

## 3. 외부에서 확인한 유용한 패턴

외부 수치나 구현 전체를 OMS의 성능·안전성 증거로 가져오지 않는다.
공식 소스의 다음 경계만 참고한다.

| 소스와 고정점 | 관측 | OMS에 대한 해석 |
|---|---|---|
| ACP 공식 [v1 initialization](https://agentclientprotocol.com/protocol/v1/initialization), [transport](https://agentclientprotocol.com/protocol/v1/transports), [prompt turn](https://agentclientprotocol.com/protocol/v1/prompt-turn), 조사일 위와 같음 | version 합의, 생략된 capability의 미지원 처리, newline JSON-RPC stdio, turn 중 notification과 역방향 요청을 정의한다. | CLI의 고정 입력·완료 출력 경계만으로는 구현할 수 없다. 협상 결과를 실행 가능성의 근거로 보존한다. |
| ACP Rust SDK `6a4a95cabed1b5307651a19771c20b91bb6566ea`: [v5 migration](https://github.com/agentclientprotocol/rust-sdk/blob/6a4a95cabed1b5307651a19771c20b91bb6566ea/md/migration-v5.md), [transport architecture](https://github.com/agentclientprotocol/rust-sdk/blob/6a4a95cabed1b5307651a19771c20b91bb6566ea/md/transport-architecture.md) | `Client.v1()`과 draft v2를 구분한다. Protocol/channel driver와 transport를 분리하지만 문서의 채널은 unbounded이며 `AcpAgent`는 자체 spawn/kill lifecycle을 갖는다. | protocol 타입·correlation의 재사용 후보다. SDK process transport나 conductor를 바로 넣으면 OMS의 수명·메모리 경계를 보장하지 못한다. |
| Mistral Vibe `b20313e01d6926d4191da89c32d5da11e3a0a060`: [ADR 0009](https://github.com/mistralai/mistral-vibe/blob/b20313e01d6926d4191da89c32d5da11e3a0a060/docs/adr/0009-app-server-boundary.md), [streaming helper](https://github.com/mistralai/mistral-vibe/blob/b20313e01d6926d4191da89c32d5da11e3a0a060/vibe/app_server/_streaming.py) | private 실행 event·public event·저장 record를 구분한다. Snapshot/watermark와 gap 복구를 계약화하고 bounded event queue와 completion을 함께 구동한다. 기존 loop ADR 0003은 0009로 대체되었다. | 관측 누락과 terminal outcome을 명시하는 패턴을 사용한다. Vibe에 OMS용 stall 판정기가 있다고 추정하거나 그 orchestrator를 이식하지 않는다. |
| Headroom `18067b1d19f6606bae3b4d6b50efeeaa48dc477e`: [savings ledger](https://github.com/headroomlabs-ai/headroom/blob/18067b1d19f6606bae3b4d6b50efeeaa48dc477e/headroom/savings_ledger.py), `SCHEMA_VERSION`, `estimate_cost_usd` | compression과 tool-schema 절감을 분리하고 cache mix·계산 basis를 기록한다. 동시에 unknown model용 가격 fallback과 retention도 둔다. | 출처·단위·basis를 보존하는 발상만 사용한다. 가격 fallback, 결측치의 0 변환, retention을 canonical usage에 복제하지 않는다. |
| 같은 Headroom commit: [session mode benchmark](https://github.com/headroomlabs-ai/headroom/blob/18067b1d19f6606bae3b4d6b50efeeaa48dc477e/benchmarks/claude_session_mode_benchmark.py), `ReplayTurn`, `TurnMetrics`, `ModeSummary`; [evaluation loaders](https://github.com/headroomlabs-ai/headroom/blob/18067b1d19f6606bae3b4d6b50efeeaa48dc477e/benchmarks/comprehensive_eval.py) | replay는 baseline/token/cache **simulation**이며 observed mix와 mode 결과를 구분한다. Evaluation 코드는 token 절감과 정답 보존을 함께 측정하도록 구성한다. | replay 결과와 실제 과제 재실행을 분리하고 품질을 함께 평가한다. 이 소스를 실행하거나 성능 향상을 재현했다는 주장은 하지 않는다. |

## 4. RF-01 — bounded duplex supervision

### 제안 계약

기존 `Reactor`가 자식과 file descriptor, signal, reaping의 유일한 소유자로 남는다.
고정 stdin 모드의 기존 동작을 유지하면서, 내부 typed 요청에만 선택적 duplex 모드를
추가한다. 이 PR에는 ACP method, 새로운 provider, 외부 명령 flag를 넣지 않는다.

| 경계 | 필수 계약 |
|---|---|
| 입력 | 실행 중 완전한 frame을 enqueue할 수 있어야 한다. `send` 성공은 queue 수락이며 자식 처리 완료가 아니다. 부분 write, `EAGAIN`, `EPIPE`를 구분한다. `close_input`은 명시적이며 반복 호출에 안전하다. |
| 출력 | stdout과 stderr를 분리한다. 소비자가 이미 본 byte를 `Snapshot`에서 다시 읽어 새 event로 세지 않도록 generation과 연속 sequence를 부여한다. Capture와 protocol 소비의 책임을 분리한다. |
| 메모리·작업량 | frame byte, 미완성 frame byte, 송수신 queue byte/event 수, pending request 수, poll당 처리량에 유한한 상한이 필요하다. 양수 bounds가 없으면 시작을 거절한다. SDK 내부 queue도 이 예산 밖에 숨기지 않는다. |
| backpressure | slow consumer가 무제한 축적을 유발하지 않는다. Queue가 가득 차도 deadline·취소·reaping을 계속 구동한다. Protocol frame을 조용히 버리지 않는다. Observer만 누락되면 별도의 gap 증거를 남긴다. |
| 종료 | 입력 종료 요청, write drain, stdout EOF, leader exit, cleanup 완료를 서로 다른 사실로 기록한다. 한쪽 EOF가 반대쪽 마지막 응답을 없애지 않는다. |
| 취소 | 협력적 취소를 시도할 수 있는 유한한 시간 뒤에는 기존 reactor stop/escalation을 사용한다. Cleanup 불명은 불명으로 반환하며 새 호출을 자동 생성하지 않는다. |

`Request`, peer `Executor`, root `ChildSpec`의 연결을 함께 검토한다. 어느 caller도
raw `Command::spawn`, detached reader, 두 번째 `waitpid`로 같은 자식을 관리해서는 안 된다.
SDK 채널을 쓸 경우 producer가 consumer보다 빨라도 모든 내부 queue의 상한이
유지되는지를 증명해야 한다. 불가능하면 해당 SDK transport를 선택하지 않고 기존 JSON·Rust 경계에서
가장 작은 bounded 연결부를 사용한다. Dependency와 수치 기본값은 이 검증을 통과한
구현 PR에서 고정한다.

### 합격·실패 시험

- Fake child가 한 frame을 여러 write로 나누거나 여러 frame을 한 write로 보내도
  byte 순서와 수락한 입력이 보존된다. 큰 frame, 잘린 UTF-8, 끝나지 않은 line은 상한 내에서 끝난다.
- Child가 stdin을 읽지 않으면서 stdout/stderr를 채워도 메모리가 bounds를 넘지 않고
  deadline·cancel이 처리된다. 실제 긴 sleep 대신 제어 가능한 clock과 pipe fixture를 쓴다.
- 입력 half-close 뒤 최종 응답, stdout 조기 종료, 종료 뒤 남은 pipe writer,
  cleanup 불명, 취소와 완료의 경합을 기존 supervisor 증거 모델로 판정한다.
- 기존 고정 stdin·출력 제한·Linux/macOS cleanup canonical test를 유지한다.
  새 mode에서만 필요한 fixture를 가까운 supervisor/peer suite에 추가한다.

## 5. RF-02 — 선택적 ACP v1 provider 하나

### 제안 계약

RF-01 이후 기존 custom-provider discovery와 write opt-in 안에서 **하나의 명시적
ACP transport 선택**을 추가한다. 기존 CLI route·기본 provider·fallback 정책은 그대로
유지한다. Custom adapter가 다시 별도의 ACP 자식을 띄우는 우회 wrapper를 만들지 않는다.
OMS reactor가 실제 ACP 자식을 직접 소유하도록 typed adapter 선택 경계를 확장한다.

첫 범위는 stdio, stable v1, 새 session 하나와 한 번에 하나의 prompt다.
Draft v2, HTTP, proxy chain, conductor, cross-provider history 복제와 session resume는
별도 후속 범위다. 지원하지 않는 요청은 실행 전에 명시적으로 거절한다.

1. **협상:** `initialize`의 protocol version과 실제 capability 응답을 저장한다.
   v1에 합의하지 못하면 prompt를 보내지 않는다. 생략된 capability는 미지원이다.
   Text prompt부터 지원하고, fs/terminal/elicitation 등 구현하지 않은 client 기능을
   광고하지 않는다. Capability 협상은 OS sandbox나 실행 권한을 대신하지 않는다.
2. **진행:** `session/new` 완료 뒤 `session/prompt`를 보내고 응답을 기다리는 동안
   `session/update`와 agent의 역방향 요청을 계속 처리한다. JSON-RPC id,
   session id와 연결 generation을 함께 검사한다. Tool 성공 notification만으로
   turn 성공이나 OMS verification을 만들지 않는다.
3. **권한:** 사전 부여한 범위와 일치하지 않는 permission은 허용하지 않는다.
   Offered `reject_once` option을 선택하거나, 유효한 거절 option이 없으면 turn을
   취소하고 pending permission에 `cancelled`를 응답한다. `allow_always`로 범위를
   확대하지 않는다. Agent가 쓴 title이나 option label은 권한의 근거가 아니다.
4. **context 소유권:** OMS는 task contract와 선택한 context의 digest·경계를
   전달하고, provider는 그 session의 대화·내부 도구 루프를 소유한다. Provider가
   추가로 읽은 rule·history는 관측될 때만 귀속한다. System prompt override나 native
   memory 소유권을 암묵적으로 가져오지 않는다.
5. **취소·종료:** `session/cancel`과 pending permission 정리를 먼저 수행한 뒤,
   bounded grace와 reactor cleanup으로 이어진다. 취소 notification 전송을 취소
   완료로 보지 않는다. 최종 protocol 결과, process exit, 출력 완전성, cleanup을
   함께 보며 누락된 terminal은 unknown이다.
6. **사용량:** #240의 물리 call receipt를 시작 전에 만들고 알려진 session identity를
   연결한다. Usage notification은 snapshot/revision과 출처가 확인될 때만 반영한다.
   통화·단위·누적 범위·estimate 여부가 불명확한 비용은 unknown으로 유지한다.
   #241 admission을 쓰는 caller는 이 unknown liability를 보존한다.

Permission과 cancellation의 wire 계약은 공식 [tool calls](https://agentclientprotocol.com/protocol/v1/tool-calls)와
[cancellation](https://agentclientprotocol.com/protocol/v1/cancellation)을 따른다.
로그는 bounded stderr로 분리하고 raw stdout의 비-protocol 문구를 정상 응답으로
삼키지 않는다. 알 수 없는 request에는 protocol 오류로 응답하고, id가 없는 notification에는
임의의 response를 보내지 않는다. 잘못된 envelope와 미협상 extension은 bounded 오류 정책으로
처리한다. 인증 부족·미지원 capability를 우회하려고 다른 계정이나 유료 provider를
자동 실행하지 않는다.

### 합격·실패 시험

- 실제 모델 대신 로컬 fake ACP peer로 initialize → session → prompt → update →
  response를 검증한다. Prompt 진행 중 permission request에 응답해야 완료되는 fixture가
  있어야 하며, 완료 후 한꺼번에 stdout을 해석하는 구현은 이 시험을 통과할 수 없어야 한다.
- 잘못된 version, 누락된 capability, 다른 session/id/generation, 중복 response,
  미승인 permission, stdout 잡문, frame 초과, 부분 terminal에서 권한·성공을 만들지 않는다.
- Permission 대기 중 취소, terminal과 취소 경합, write drain 실패, nonzero exit와
  cleanup 불명은 구분한다. Canonical receipt의 idempotency·누적 baseline 시험을 재사용한다.
- 기존 CLI provider golden contract와 custom read/write opt-in이 그대로 통과한다.
  실제 provider 사용은 별도의 명시된 비용·계정 권한을 가진 integration 평가에서만 수행한다.

## 6. RF-03/04 — 실행 중 증거와 stall 관측

### 관측과 해석

현재 goal/campaign의 정체 판단은 cycle/wave 단위이고 저장된 transcript timeline은
사후 분석이다. 둘 다 유지한다. Vibe의 bounded stream과 gap/watermark는 실시간
전달 경계의 참고 사례이며, 반복 tool 호출이 항상 잘못이라는 근거는 아니다.
기존 [CP-08](CONTROL-PLANE-EVOLUTION.md#cp-08--실행-timeline사용량진단)의
advisory 원칙을 유지하면서 다음 범위를 추가한다.

### RF-03: observer만 먼저 연결

RF-01의 실제 stdout 수신 경계에서 한 가지 지원되는 provider event 형식을 먼저
해석한다. ACP 전용으로 만들 필요는 없지만, 실행 종료 전 event가 나오는지를 시험해야 한다.
Normalized event는 최소한 `call_id`, session/connection generation, 수신 sequence,
monotonic 수신 시각, source/parser version, event kind, 관측 완전성을 가진다.
Tool id·상태·입출력 digest는 실제로 보고되었을 때만 채운다.

Raw argument, command, 결과 본문과 private reasoning을 새 로그에 복사하지 않는다.
기존 scrubber와 안전한 저장·크기 제한을 재사용한다. 동일 수신 byte를 다시 polling한
것과 실제 새 호출을 구분한다. Provider event id가 없으면 완벽한 재전송 dedup을
주장하지 않고 그 한계를 기록한다.

| 관측 상태 | 해석과 허용된 표시 |
|---|---|
| `progress_recent` | 확인한 tool 상태 전이·새 결과·기존 producer checkpoint의 최근 수신 |
| `awaiting_permission` | 응답 대기 증거가 있는 상태. 일반적인 무출력 stall timer와 구분 |
| `tool_active` | 시작이 보고되었고 terminal 결과가 아직 없음. 오래 걸리는 정상 작업일 수 있음 |
| `quiet` | 설정된 시간 동안 새 의미 event가 없음. 장애나 무과금의 증거가 아님 |
| `repetition_suspected` | 한정된 window 안의 같은 action/result fingerprint 반복. 정책 버전과 관측 수를 함께 표시 |
| `evidence_incomplete` | gap, 출력 truncation, parser 미지원, 재접속 등. 정상·정체로 억지 분류하지 않음 |

단순 byte 유입은 transport 생존 신호이며 의미 있는 진척과 별도다. 동일 action은
입력·결과·대상 generation을 함께 보되 비멱등 작업을 임의로 합치지 않는다.
원래 event history에 없는 repo 변화나 tool 성공을 추정해서 counter를 초기화하지 않는다.
최근 상태를 bounded metadata로 투영하고 gap 수·미분류 수를 함께 표시한다.

### RF-04: advisory와 hold를 분리

기본 observer는 설명만 제공한다. 그 자체로 kill, retry, provider 교체, 자동 repair를
실행하지 않는다. 명시적으로 검토된 controller policy가 있을 때만 **다음 dispatch나
continuation의 admission을 보류하는 `policy_hold`**를 추가할 수 있다.

`policy_hold`는 이미 실행 중인 provider를 정지시켰다는 뜻이 아니다. Current call의
관측·cleanup·usage 정산은 계속된다. Active cancellation이 필요한 정책은 해당 capability와
이미 부여된 authority를 별도로 확인하고 기존 supervisor 경로를 사용한다. 지원되는
취소 지점이 없으면 `control_unsupported`를 기록한다. SIGSTOP이나 PID 재사용 추정으로
pause를 흉내 내지 않는다.

Hold를 푸는 근거는 fresh evidence 또는 명시된 controller 결정이며 과거 event replay가
아니다. #238 observation이나 #239 reviewed repair의 승인을 서로 바꾸어 쓰지 않는다.
재개·재시도에는 #241 admission을 다시 적용하고 unknown receipt를 0으로 정산하지 않는다.

### 합격·실패 시험

- Fake stream의 event가 child exit 전에 observer에 도달한다. 저장된 transcript만
  재생한 시험은 `replay`로 표시하고 live-control 시험으로 세지 않는다.
- Permission 대기, 정상 장시간 tool, byte만 흐르는 출력, 동일 호출 반복, 중복 polling,
  missing id, gap, truncated frame, 시계 경계, 재시작을 결정적 fixture로 구분한다.
- Advisory만 켠 실행은 기존 호출 수·signal·provider 선택이 바뀌지 않는다.
  Hold는 새 admission을 막고 이미 실행 중인 call의 책임과 사용량을 유지한다.
- Cancellation과 completion이 경합해도 하나의 terminal 결과만 연결한다. Lossy observation은
  verification 성공, cleanup 완료, 비용 환불 또는 새 repair 권한이 되지 않는다.

## 7. RF-05/06 — context 귀속과 동일 과제 비교

### RF-05: 기존 manifest와 호출 identity를 연결

기존 compiler·injection ledger·`context-inspect`를 재사용한다. 새 memory DB,
검색 서비스, 자동 compression 정책은 이 범위에 없다. 각 실제 prompt 구성 지점에서
`operation_id`/`call_id`, request digest, context manifest digest, phase policy digest와
renderer version을 연결한다. Legacy record는 미측정 필드를 unknown으로 읽고 기존
inspection 결과·schema 호환성을 시험한다.

제안 layer는 `contract`, `task`, `retrieved`, `history`, `tool_schema`, `tool_result`,
`framing`이다. 각 바이트 범위의 출처가 확인된 경우에만 배정한다. Provider의 자동
규칙·history·compaction은 별도 `provider_added_unknown` coverage로 표현하며 0 bytes나
측정 완료로 표시하지 않는다. 한 payload 안의 범위를 둘 이상의 layer에 중복 합산하지
않지만, 같은 source가 다른 실제 호출에 다시 전송되면 각 호출의 입력으로 각각 기록한다.

| 측정 | 출처와 한계 |
|---|---|
| source bytes / selected payload bytes / framing bytes | 실제로 읽고 직렬화한 바이트에서 측정한다. Truncation marker는 injected payload에 포함하고 원본에서 빠진 바이트와 구분한다. |
| layer tokens | 정확한 tokenizer·version·model 근거가 있을 때 그 측정 basis를 표시한다. 없으면 null이다. Byte/4 같은 환산값을 reported token으로 만들지 않는다. |
| provider input/cache/output/cost | #240 receipt를 기준으로 한다. 호출 전체 보고값을 layer 추정치의 합으로 대체하거나 덧셈하지 않는다. |
| prefix 변화·cache 결과 | 직렬화된 실제 prefix digest와 알려진 경계를 기록한다. Digest 안정성은 cache hit 증거가 아니며 provider가 보고한 cache 사용량과 따로 비교한다. |
| omission·coverage | 제외 사유, required-context debt, 미측정 source, receipt와 manifest 연결 실패를 함께 보고한다. |

Partial/final/cumulative receipt는 기존 revision projection을 사용한다. Fallback의
물리 호출은 각각 남기고, 호출 전에 준비한 prompt가 실제로 전송되었는지도 구분한다.
준비 후 거절된 prompt를 provider 입력 비용으로 계산하지 않는다.

**합격 시험:** Unicode와 framing·truncation marker를 포함한 byte 합이 실제 request와
일치해야 한다. 같은 원본의 서로 다른 slice, 반복 전송, missing tokenizer, provider-added
context, fallback, late usage correction, legacy manifest를 검증한다. 측정 실패가 권한이나
성공을 만들어서는 안 되며 기존 required-context debt를 숨겨서는 안 된다.

### RF-06: 기존 benchmark에 paired protocol 추가

현재 `matched_cohorts`는 관측 집계다. 새 비교는 기존 runtime benchmark와 artifact
producer를 연결하되 **실행 전에 고정한 pair manifest**가 있어야 한다. 이 문서는
pair 실행기나 새로운 CLI flag를 제공하지 않는다.

Pair는 동일 task/case digest, repo base, acceptance/verify 명령 digest, 허용 도구,
provider·확인 가능한 model identity, effort, backend/image, timeout·budget을 공유한다.
비교하려는 context policy 하나만 treatment로 바꾸고 policy/renderer/tokenizer 버전을
남긴다. Fallback이나 served-model 불명으로 control이 깨지면 유효한 pair에서 제외하되
누락·실패 건수와 이유를 결과에서 제거하지 않는다.

- Task별 두 arm의 순서를 사전 배정하고 반복 수·seed·cache 조건·중단 조건을 기록한다.
  같은 변경된 worktree를 다음 arm에 넘기지 않는다. Cache warm/cold는 선언과 실제 관측을
  구분하고, 관측되지 않은 hit rate를 가정하지 않는다.
- 동일한 verifier로 성공·실패·미결을 판정한다. Token·벽시간·보고된 비용 외에 required
  evidence 보존, context debt, 재시도·repair, 기존 human correction/defect metric을 함께 본다.
- 비용은 #240의 known reported/estimate/unknown basis별로 구분한다. 유료 재실행에는
  명시된 평가 권한과 #241의 총량·호출 reservation을 적용한다. 더 좋은 수치를 얻으려고
  무한 재시도하거나 실패 arm을 버리지 않는다.
- 결과에는 pair 수, 미완료·결측 수, arm별 품질, task별 차이와 사전 정한 집계를 담는다.
  샘플 수·통계 방법·품질 허용 기준을 사전에 고정한다. 표본이 부족하면 불확실성을
  표시하며 speedup, cost saving, 품질 동등성을 단정하지 않는다.
- Offline replay는 parser·byte attribution·prefix 변화를 검증할 수 있다. 새로운 model
  응답, 실제 과제 성공률, end-to-end latency의 인과적 개선을 검증한 것으로 보고하지 않는다.

**합격 시험:** 서로 다른 task/base/verifier/basis의 비교 거절, 한쪽 실패·취소·unknown,
zero와 missing의 구분, operation/call 중복, warm/cold 혼합, 대칭적인 arm 순서와 결과
재읽기를 canonical benchmark suite에서 검증한다. 자동 시험은 fixture만 사용한다.
성능 수치는 별도로 승인된 실제 paired run의 artifact와 verifier evidence가 있어야 공개한다.

## 8. 구현 순서와 완료 조건

| PR 단위 | 좁은 변경 경계 | 의존성과 완료 조건 |
|---|---|---|
| RF-01 duplex supervisor | `oms-supervisor` process/capture와 peer/root typed plumbing | 기존 고정 stdin 회귀 없음, 양방향 backpressure·cancel·cleanup 시험 통과. ACP dependency 없음 |
| RF-02 ACP v1 하나 | 기존 custom provider discovery, typed adapter, 로컬 fake protocol peer | RF-01과 #240 이후. v1·capability·permission·context ownership·usage 연결을 증명 |
| RF-03 live observer | 실제 provider stream producer, 작은 normalized projection, 기존 read surface | RF-01 이후. 하나의 형식에서 exit 전 관측을 증명하며 나머지는 unsupported |
| RF-04 optional hold policy | 기존 controller admission과 attention/recovery 경계 | RF-03 이후. #239 repair 권한과 #241 budget 책임을 유지. Advisory 기본 동작 불변 |
| RF-05 context attribution | 기존 context manifest/inspection와 prompt 구성 지점 | #240 identity에 연결. Per-layer bytes·coverage를 추가하고 legacy 읽기 유지 |
| RF-06 paired benchmark | 기존 runtime benchmark와 artifact/verification 연결 | RF-05·#240 이후, 유료 campaign 평가에는 #241. 결과 없는 성능 주장 없음 |

RF-01과 RF-05는 별도 검토가 가능하다. ACP·observer 모두 supervisor ownership이 필요한
부분은 공유하되 두 번째 orchestrator를 만들지 않는다. 각 구현 PR은 실제 source HEAD와
protocol/dependency version을 다시 확인하고, 변경한 Rust crate의 canonical test와
필요한 root 통합 검사를 수행한다. Linux/macOS 차이를 숨긴 공통 성공 receipt는 허용하지 않는다.

### 보류: 여러 artifact의 원격 수집 manifest

[#243](https://github.com/eightmm/oh-my-setting/pull/243)의 단일 파일 재개 계약이
병합·검증된 뒤 별도 PR로 다룬다.
고정된 run·manifest digest에 bounded 파일 목록과 상대 경로·크기·SHA-256을 결합하고
파일별 partial/verified 상태를 유지하는 정도가 다음 범위다. 모든 필수가 검증되기 전에
bundle 완료를 기록하지 않는다. 경로 탈출·symlink·중복 경로·manifest 변경·부분 재개·손상
fixture가 필요하다. 현재 수집 PR이나 Apptainer 구현을 이 설계 문서에서 복제하지 않는다.

## 9. 이 문서의 검증 범위

이 변경은 문서와 documentation map 링크만 추가한다. 저장소 문서 reference 검사를
새 문서까지 포함해 수행하고 diff를 검토한다. 위 acceptance 항목들은 **미래 구현의
합격 조건**이며 이 문서 PR에서 통과한 runtime 시험이나 구현된 capability 목록이 아니다.
외부 소스는 위 commit에 고정했고, protocol 웹 문서는 조사일의 v1을 읽었다. SDK의
전체 buffering 한계, 실제 ACP provider 상호운용성과 paired task 결과는 후속 구현·평가에서
확인해야 한다.

## 10. 2026-11-21 하네스 강화의 현재 경계

### 목표와 협업 범위

확인 기준은 main `1dfa9a491dbdb8917d2bec19f4c1eafce72a6404`다.
[Claude의 #313](https://github.com/eightmm/oh-my-setting/pull/313)은 head
`3f07b7b7ddbdd3865a975e6302782a785d7259dd`의 명세를 읽었다. Provider capability,
custom adapter session, ACP, guard/sandbox 범위와 council 등의 연결 작업을 다룬다.
아래 보강은 그 경계와 함께 **완료를 믿을 근거, 다음 세션이 잃은 정보,
다시 쓸 수 있는 복구 경험, 하네스의 실제 효과**를 다룬다. #313의 구현이나 병합을
전제하지 않으며 그 파일·provider writer를 함께 수정하지 않는다.

Overall Main은 직접 수행과 필요한 분업을 선택한다. Planner, Lead, reviewer를 모든
작업의 필수 단계로 만들지 않는다. Provider native tool loop와 compaction을 보존하고,
기존 [작업 큐](ORCHESTRATOR-WORK-QUEUE.md)의 owner·채택 조건을 따른다.
아래 표는 새 작업 큐나 신규 ID 체계가 아니라 기존 작업의 구체화다.

### 현재 구현과 남은 공백

| 기존 기능 / source | 이번에 확인한 경계 | 처리할 위치 |
|---|---|---|
| [Task verify](../crates/oms-command-task/src/lib.rs), [native failure observer](../crates/oms/src/services_control.rs) | Exit 0의 failure resolve가 최종 verification receipt 수락보다 먼저 호출된다. 최종 task/contract/repository/CAS 거절이 있어도 advisory failure가 해제될 수 있다. | 아래 별도 correctness PR; DR-03의 검증 근거를 보존하는 선행 수정 |
| [Runtime benchmark](../crates/oms-runtime/src/benchmark.rs) | 양쪽 basis가 같은 결측값이면 numeric delta를 허용한다. 기록이 없는 manual/skill 합계 0도 차이로 계산한다. | 아래 별도 correctness PR; RF-06의 기존 비교 경계 |
| [Session handoff](../crates/oms-command-autonomy/src/handoff.rs), [decision selection](../crates/oms-command-autonomy/src/advisory.rs) | 전체 `truncated` 수는 있으나 어느 section이 빠졌는지 남지 않는다. Unavailable/withheld 설명 자체가 byte trimming에서 빠질 수 있고 decision/goal 선별 손실도 모두 표현하지 않는다. | REL-01의 consumer coverage 후속 |
| [Attempt failure projection](../crates/oms-command-attempt-bundle/src/bundle.rs) | 시간 구간으로 failure를 모으며 다른 작업의 failure일 수 있음을 이미 표시한다. [FailureObservation](../crates/oms-command-task/src/services.rs)에는 task/attempt/accepted receipt identity가 없다. | [DR-03/04](MEMORY-CONSOLIDATION.md)의 출처 연결·재사용 적격성 |
| [Native state adapter](../crates/oms/src/services_control.rs), [state projection](../crates/oms-command-state/src/state_projection.rs) | Native state 계열의 canonical runtime envelope 연결은 이미 존재한다. Producer 실패는 degraded이며 library-only fallback과 구분된다. | FEATURE-SCOPE의 오래된 설명만 정정; 새 envelope 연결 작업 없음 |
| [Recipe resume](../crates/oms-command-state/src/recipe.rs), [experiment](../crates/oms-runtime/src/experiment.rs) | Plan·command·environment 등 기존 실행 identity와 연구 계약이 있다. 일반 checkpoint engine이나 별도 실험 대장이 필요한 근거는 확인하지 않았다. | 기존 receipt·run ledger·선택형 contract의 caller별 보강 |

### 먼저 분리한 구현 PR

두 수정은 위 기준 source에서 회귀를 재현한 별도 변경이다. 이 문서 PR은 해당 Rust
변경을 포함하지 않는다. PR 생성·리뷰·CI·병합 상태는 실제 PR과 head의 근거로 확인하며,
명세 작성이나 local fixture 통과를 병합·배포 완료로 표시하지 않는다.

| 변경 | 구현 경계 | 합격 기준 / 남은 한계 |
|---|---|---|
| [#314 Benchmark 비교의 unknown 보존](https://github.com/eightmm/oh-my-setting/pull/314) | 현재 producer basis만 허용하고 기존 sample count와 unknown metric 목록으로 미측정을 구분한다. 원본 값과 비교 거절 이유를 보존한다. | Missing/malformed/unsupported basis, zero sample, candidate-only skill을 null delta로 처리. 실제 관측 0과 양의 sample 수 차이는 유지. 같은 task·환경·duration source라는 보장은 추가하지 않는다. |
| [#315 최종 verify 수락 뒤 failure resolve](https://github.com/eightmm/oh-my-setting/pull/315) | 기존 두 단계 finalize 중 성공 receipt 수락을 exit-0 observer보다 앞에 둔다. Nonzero·skip·advisory 오류 계약은 유지한다. | Task rotation, Verify 변경, repo 변경, 최종 CAS 실패 시 resolve가 호출되지 않는다. 정상 pass의 observer는 이미 저장된 receipt를 읽는다. Resolve는 여전히 command 단위이며 cross-domain transaction이나 정확한 recovery lineage는 아니다. |

### 후속 작업의 목적·범위·검증

P0/P1/P2는 이 보강 안의 진행 순서이며 기존 작업 큐 전체의 우선순위를 다시 정하지 않는다.
각 구현은 착수 시 실제 main과 기존 PR의 상태를 재확인한다.

| 축 / 기존 owner | 목적과 다음 구현 범위 | 우선순위·의존성 | 검증 기준과 다음 작업 |
|---|---|---|---|
| 완료 판정 / task·DR-03 | 성공한 명령, 수락된 receipt, 실제 acceptance/publication을 구분한다. 위 verify 순서 수정을 먼저 검토한다. | P0; 현재 task reference/CAS와 native observer | 회귀 fixture와 해당 crate 검사. 이후 recovery identity를 붙일 때 이 순서를 보존한다. |
| 결과 비교 / RF-06 | 미측정·0·관측 차이를 구분한 뒤 실제 선택 문제에 한해 동일 과제 비교를 연결한다. | P0 비교 수정; P2 paired protocol은 기존 RF-06 착수 조건 유지 | 같은 task/base/verifier와 실패·결측 포함 결과. 다음 판단은 review/분업/context policy 중 한 요소가 품질·시간·수정량에 주는 효과다. 측정 없는 자동 routing이나 상시 평가를 켜지 않는다. |
| 인수인계 / REL-01 | 다음 세션이 읽지 못한 source와 renderer 손실을 section별로 알 수 있게 한다. | P1; 기존 handoff renderer와 canonical peer fixture | 최소 512-byte budget, unavailable/withheld/truncated/empty를 구분. 먼저 아래 bounded 표시 계약을 구현한다. RF-05 전체 attribution이 선행 조건은 아니다. |
| 복구 경험 / DR-03/04 | 검증된 복구 절차는 failure→action→accepted verification을 정확한 identity로 연결한다. 실패·미결 경험은 불확실성을 유지해 보존한다. | P1; 위 verify 수정, 기존 DR 출처·review·supersede 계약 | 동시 task의 failure를 복구 성공으로 잘못 묶지 않는다. 다음 단계는 기존 failure row와 attempt projection의 optional correlation 계약을 좁혀 정하는 것이다. |
| 환경·검증 해석 / VW-04·REL-02 | 실제 수행·생략·timeout·환경 제약과 model/작업 실패를 분리한다. | P1, 기존 VW/REL 변경에 흡수; 별도 telemetry 선행 없음 | 기존 gate/receipt에 이미 있는 SHA·실행 범위·exit·선택 사유부터 사용. 다음 변경에서 요청한 검사와 실행한 검사의 차이 및 근거 없는 분류를 명시한다. 새 CI job·전체 재실행·무조건 retry 없음. |
| 작업별 성과 계약 / 기존 task·recipe·experiment | 코드 외 연구·실험에서도 산출물과 성공 기준을 먼저 정한다. | P2; 실제 반복 workflow와 기존 verifier/산출물 owner | 코드 작업은 tree와 검사 결과, 조사 작업은 주장·출처·미확인점, ML 작업은 dataset/split/seed/metric/artifact를 해당 contract에서 확인한다. 하나의 실제 workflow부터 기존 template/recipe로 적용하고 global 규칙으로 강제하지 않는다. |

### REL-01: handoff coverage의 좁은 다음 변경

기존 일곱 heading과 전체 truncation footer를 유지하며, 남기는 content row의 source
tag를 보존한다. 표시할 것은 source별 내용의 진실성이나 검증 완료가 아니라
**관측된 전달 손실**이다.

- 각 section에 producer omission, renderer omission, withheld, unavailable의 독립 상태를
  남긴다. 이 metadata를 일반 row와 분리해 최종 budget trimming에서 함께 제거하지 않는다.
- Task packet이 withheld/unavailable이면 Goal과 Decisions 양쪽에 반영한다. 정상적으로
  빈 목록인 경우와 읽지 못한 경우를 구분하고, 닫힌 task의 정책상 제외를 임의 누락으로 세지 않는다.
- [Task-packet decision selector](../crates/oms-command-autonomy/src/advisory.rs)의 newest-six /
  4096-byte 제한, Goal excerpt, row 크기·개수 제한, 최종 byte 제한을 모두 점검한다.
  기존 전역 count와 section 상태가 같은 한 건을 중복 집계하지 않도록 owner를 정한다.
- 최소 512-byte 출력에서도 metadata·heading·footer를 남기고 secret/private source는 계속
  withheld한다. 짧은 flag와 legend는 가능한 표현 후보이며 공개 JSON/schema를 새로 만들지 않는다.
- 기존 [peer handoff fixture](../crates/oms-command-peer/tests/peer_contract.rs)의 complete,
  unavailable, interrupted, malformed, sensitive, line/read-budget 사례를 확장한다. 정상 빈
  결과와 모든 content row가 잘린 결과를 구분하고, gap 없음이 fresh verification을 뜻하지 않음을 검사한다.

이 문서는 표시 형식을 구현하거나 512-byte 시험을 실행하지 않았다. 구현 PR에서 실제
최악 길이·UTF-8 경계·비밀 제거·native consumer를 확인한다.

### DR-03/04: 실패 경험은 exact 연결부터

기존 fail-ledger와 attempt bundle에 실제 producer가 가진 optional task/attempt/receipt
correlation을 연결하는 범위를 먼저 설계한다. 기록에 없는 ID를 시간 근접성으로 만들어
넣지 않는다. Legacy row는 읽을 수 있어야 하며 시간 구간의 후보 연결은 계속 불확실하다.
Command-level resolve의 범위를 silently 바꾸지 않고 별도 호환성·concurrent-task 사례를 검토한다.

검증된 복구 절차로 재사용하려면 failure, 수행한 action, 수락된 검증 근거가 정확한
identity로 이어져야 한다. 실패·미결·효과 없는 조치도 기존 불확실성을 유지해 보존한다.
과거의 성공은 새 repo tree·환경·계약에서의 성공을 보장하지 않는다. 기존 memory provenance와
review eligibility, hygiene, supersede/retire를 사용하며 skill 후보도 기존 skill-forge로
넘긴다. 새 memory DB, 자동 repair loop, 과거 검증에 의한 현재 task 승인 권한은 만들지 않는다.

### RF-06: 효과를 확인하며 하네스 단순화

기존 관측 snapshot은 문제를 찾고 실험 대상을 정하는 자료다. 비교값이 계산된다는 사실이
동일 조건이나 인과적 개선을 보장하지 않는다. 특히 현재 duration은 provider sample이
없으면 lifecycle-event sample을 사용하며 그 선택이 snapshot basis에서 분리되지 않는다.

반복되는 실제 선택 문제가 생기면 7절의 pair protocol을 적용한다. 예를 들어 Main 단독과
독립 reviewer 추가를 비교할 때 같은 task/base/verifier·환경·예산을 고정하고 해당 요소만
변경한다. 품질·실패·사람의 수정량·시간과 알려진 사용량을 함께 보며 실패 arm을 빼지 않는다.
효과를 입증하지 못한 강제 분업·반복 review·context 주입은 줄이는 후보로 검토한다.
현재 model/effort의 가용한 조합을 사용하며 vendor 이름이나 고정 tier만으로 경로를 정하지 않는다.
이 보강에서는 유료 model/API 평가나 GPU/원격 작업을 실행하지 않았다.

### 공식 자료에서 가져온 판단 근거

아래 원문을 2026-11-21에 읽었다. 외부 결과는 OMS 성능 개선의 실측치가 아니며,
새 기능의 필요성은 위 현재 source/caller에서 따로 확인한다.

| 자료 | 확인한 내용 | OMS에 적용할 판단 |
|---|---|---|
| Anthropic, [Demystifying evals for AI agents](https://www.anthropic.com/research/demystifying-evals-for-ai-agents), 2026-01-09 | Transcript의 주장과 실제 환경의 outcome, grader와 실행 환경을 구분한다. | Agent의 완료 보고와 실제 수락된 verification을 구분하고 작업별 성공 기준을 둔다. |
| Anthropic, [Harness design for long-running apps](https://www.anthropic.com/research/harness-design-long-running-apps), 2026-03-24 | 구성요소를 하나씩 제거하며 효과를 확인했고 model 변화 후 sprint 구조를 제거했다. Evaluator의 효과는 작업 난도와 model 능력에 따라 달랐다. | 단일·다중 agent와 review를 고정 계층으로 만들지 않고 같은 조건의 품질·비용 근거로 선택한다. |
| Anthropic, [Quantifying infrastructure noise in agentic coding evals](https://www.anthropic.com/research/infrastructure-noise), 2026-02-05 | 자원·제한·환경 차이가 agent 평가 결과에 영향을 주며 설정과 enforcement를 보고해야 한다. | 환경 불명·timeout·실행하지 않은 검사를 model 실패나 통과로 대체하지 않는다. 외부 실험의 수치를 OMS 개선율로 옮기지 않는다. |

문서 보강의 검사는 상대 reference와 diff/source scrubber 검사로 한정한다. 별도 구현
PR의 실제 시험 결과는 그 PR에 기록한다. 후속 기능, 실제 provider 동작, 성과 개선,
병합·배포 상태는 이 문서만으로 완료 처리하지 않는다.
