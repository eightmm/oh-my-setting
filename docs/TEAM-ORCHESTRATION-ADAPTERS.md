# Team provider adapters: capability, observation and control

작성/공식 문서 확인: 2026-11-17. **DESIGN ONLY**.
OMS 조사 기준: `32923cd388e581a6d6d24525b200fca746cda4d0`.
상위 계약: [#245 architecture](https://github.com/eightmm/oh-my-setting/pull/245),
[#246 runtime](https://github.com/eightmm/oh-my-setting/pull/246).
대상 작업은 TM-07/08/09다. Provider 문서의 기능과 OMS에서 검증한 capability를
구분한다. 이 문서는 native 실행이나 설치된 CLI의 해당 기능을 검증한 결과가 아니다.

## 1. 공식 문서에서 확인한 사실

| Provider | 확인한 공개 표면 | 아직 보장하지 않는 것 |
|---|---|---|
| Codex | [App Server](https://learn.chatgpt.com/docs/app-server)는 stdio JSON-RPC, initialize/initialized, thread/turn, streamed events, approval, interrupt를 문서화한다. `thread/list`의 sourceKinds에 subAgent 계열이 있고 parent/ancestor filter는 experimental opt-in이다. | 모든 설치 버전에서 동일 schema, native child의 cap/개별 제어/전체 cleanup, OMS 독립 검증 |
| Codex subagents | [Subagents](https://learn.chatgpt.com/docs/agent-configuration/subagents)는 현재 parent sandbox/approval 상속과 concurrency 설정을 설명한다. Custom agent instruction만으로 별도 read-only sandbox가 생기지 않는다. | Worktree의 자동 분리, 역할 이름에 따른 OS 권한 차등, native 내부 비용의 완전한 귀속 |
| Claude | [Agent Teams](https://code.claude.com/docs/en/agent-teams)는 experimental/opt-in이며 teammate 생성에 interactive session이 필요하다고 명시한다. `-p`/Agent SDK에서는 named agent도 ordinary subagent로 동작한다. | Headless teammate 생성, 일반 subagent와 Native Team의 동등성, 완전한 resume/child stop 보장 |
| Claude headless | [Programmatic mode](https://code.claude.com/docs/en/headless)와 [CLI reference](https://code.claude.com/docs/en/cli-reference)는 비대화형 실행, 구조화/streaming output과 session 기능을 제공한다. | SDK package를 OMS에 추가해야 한다는 요구, JSON result만으로 team control이 된다는 주장 |
| Antigravity Swarm | [Swarm](https://antigravity.google/docs/swarm/)은 coordinator/workers/verifier와 shared board를 문서화한다. Workspace는 기본 공유이며 개별 working copy는 선택적이다. | OMS가 native 작업 공간·git landing·worker 제거를 통제한다는 보장 |
| Antigravity headless | [Headless CLI](https://antigravity.google/docs/cli/headless/)는 stream-json의 init/step_update/result, subagent_info, 연속 stdin user prompts를 문서화한다. Streaming session의 usage 등 metadata는 누적 범위다. | Swarm board의 완전한 machine API, 개별 worker cancel/resume, 내부 spawn hard cap |

특히 AGY를 완전히 불투명한 실행기로 단정하지 않는다. `subagent_info`는 실제 관측
어댑터의 출발점이다. 반대로 event가 있다고 외부 제어까지 지원된다고 승격하지 않는다.
위 문서는 가변 URL이므로 구현 PR은 확인일, 실제 binary version, 생성 schema/digest와
fixture 출처를 고정해야 한다. 온라인 최신 기능을 사용자의 설치 상태로 간주하지 않는다.

## 2. 하나의 team 계약, 서로 다른 capability

기존 [provider routing](../custom-skills/oms-agent-harness/references/provider-routing.md)의
transport/model-family 구분과 custom adapter opt-in을 유지한다. Provider를 team,
model router, review policy와 혼동하지 않는다. 예를 들어 AGY의 모델 라우팅 기능은
Swarm 협업 기능과 별도다. 모델 이름을 새 전역 enum에 하드코딩하지 않는다.

각 capability는 다음 증거를 가진다: documented source/version, locally probed version,
fixture evidence, optional live evidence, control scope, known limitation, last checked time.
단일 `supports_swarm=true`는 충분하지 않다.

| Capability | 필요한 증거 |
|---|---|
| start / resume / send | 실제 지원하는 interface, 정확한 session identity와 권한, send uncertainty 처리 |
| observe_children | explicit parent binding, parser/version, pagination/coverage/gap |
| steer_member / cancel_member | 실제 target ID/turn/generation, 허용된 method와 결과/cleanup 의미 |
| cancel_team | 전체 owned subtree의 종료 확인 또는 불명 한계 |
| isolated_workspace | 실제 각 member working copy와 writable boundary; prompt에 요청한 사실과 구분 |
| bounded_fanout | effective cap, primary 포함 여부, descendants/depth 적용 범위; prompt-only는 미충족 |
| usage | delta/absolute/cumulative scope, parent-inclusive 여부, receipt identity, missing fields |
| structured_result | 요청한 schema와 실제 artifact binding; 자연어 성공과 구분 |
| headless / interactive | 같은 provider라도 mode별 capability를 별도로 기록 |

Unsupported capability를 다른 이름의 가짜 성공으로 바꾸지 않는다. Automatic fallback은
사용자가 허용한 mode/model/account와 새 admission 안에서만 가능하다. Provider 설정,
인증 경로, 결제 방식 또는 글로벌 hooks를 암묵적으로 바꾸지 않는다.

## 3. 현재 supervisor 재사용과 provider별 연결

[#244 RF-01](https://github.com/eightmm/oh-my-setting/pull/244)은 공통 transport의 설계 근거다.
2026-11-18 source에는 이미 [Reactor의 `Request.duplex`](../crates/oms-supervisor/src/process.rs)와
[native TeamRequest consumer](../crates/oms/src/services_team_requests.rs)가 있다. 기존 core를
재사용하고 provider별 protocol/권한/관측 caller를 연결한다. 이 사실은 built-in provider의
TeamRequest나 native team control이 구현되었다는 뜻이 아니다.

고정 stdin/completion adapter에 live JSON-RPC를 억지로 붙이거나 별도 reader process/reaper를
생성하지 않는다. 기존 supervisor가 stdio, process identity, signals, deadlines, cleanup의
유일한 소유자다. 공통 API 변경은 REL-04/CC-06과 같은 owner 아래 조정한다.

공통 연결은 frame/partial-frame/queue bytes, pending requests, events per poll,
output retention에 유한한 상한을 요구한다. Slow consumer에서도 cancellation과
reaping은 진행한다. Protocol frame을 조용히 버리지 않으며 observer 누락은 gap으로
기록한다. Stderr는 protocol stdout과 분리한다. 터미널 출력/ANSI 해석을 control API로 사용하지 않는다.

JSON-RPC request ID, provider thread/turn ID, connection generation, OMS attempt를
함께 확인한다. 재접속은 과거 turn start를 자동 재전송하지 않는다. Runtime이 도구
permission에 응답해야 진행되는 fixture를 포함하여 완료 후 로그 파싱만 하는 구현이
통과하지 못하게 한다. ACP가 필요하면 RF-02와 같은 경계를 사용하며 ACP adoption
자체를 모든 provider 구현의 필수 dependency로 만들지 않는다.

## 4. TM-07 — Codex App Server

### 구현 범위

첫 경로는 기존 부모 runtime이 소유하는 invocation/team-scoped **stdio process**다.
새 daemon, 기본 network listener, SSH bridge, TypeScript/Python runtime을 넣지 않는다.
하나의 App Server가 서로 다른 owner/team을 무분별하게 공유하여 process kill이
다른 작업을 종료시키는 구조를 피한다. 공용 process가 필요하면 별도 lifecycle ADR과
per-thread cleanup 증거가 선행되어야 한다.

실제 Codex binary의 JSON schema를 고정하고 지원 version/capability별 parser를
fixture로 검증한다. 모든 experimental API를 한꺼번에 켜지 않는다. Parent/ancestor
filter가 필요하면 정확한 기능에 대한 opt-in과 schema 시험을 요구한다.

### Lifecycle mapping

| Provider 표면 | OMS 처리 제안 |
|---|---|
| initialize -> initialized | Version/schema/capability를 확정한 connection generation |
| thread/start 또는 검증된 thread/resume | OMS-owned Team/MemberBinding과 native ID 결합; 최근 세션을 추정 선택하지 않음 |
| turn/start | 전송 전 intent/usage reservation. Response 손실 시 재전송 아닌 reconciliation |
| item/turn events | 기존 bounded observation projection. Tool 결과는 untrusted evidence |
| turn/steer | 정확한 active turn과 권한이 일치하는 경우만. 제한된 team message로 routing |
| approval request | Existing durable approval와 exact scope binding; 새 global allow나 yolo로 우회 금지 |
| turn/interrupt | 취소 요청. Terminal status/descendant cleanup 확인 전 성공 처리 금지 |
| thread/list child filters | Explicit owned root 아래만 페이지 탐색. sourceKinds/cwd만으로 ownership 판정 금지 |

Thread fork는 continuation과 구분하고 context/usage baseline을 새 provenance로
연결한다. Archive/delete는 cancellation이나 cleanup의 대체 수단으로 쓰지 않는다.
외부 세션의 history를 조사하거나 제어하는 범위는 별도 opt-in이다.

### Native child와 write gate

Codex native child가 현재 parent sandbox를 상속하므로, role prompt를 read-only로
써도 독립적인 sandbox가 된 것으로 표시하지 않는다. 먼저 parent 전체를 read-only로
검증하거나 각 writer를 OMS-owned 독립 실행으로 분리한다. Native write team은
workspace isolation, effective fan-out bound, owned child lineage, cancellation/usage
coverage가 계약을 만족할 때만 허용한다. 충족하지 않으면 기존 `oms-team` 또는
명시적 unsupported 결과를 제공한다.

### 완료 시험

Fake App Server에서 handshake 순서, fragmented/oversized frame, 역방향 approval,
wrong thread/turn, duplicate response, child pagination, missing parent relation,
experimental 거절, overload, interrupted turn, reconnect ambiguity, orphan child를
시험한다. 실제 CLI smoke는 별도 승인된 bounded read-only 실행으로 하며 fixture와
live 결과를 구분한다. App Server가 있다는 것만으로 native-team 통합 완료로 선언하지 않는다.

## 5. TM-08 — Claude: managed team과 native team 분리

**MVP의 Claude 팀은 OMS가 여러 ordinary CLI workers를 관리하는 `oms-team`이다.**
각 invocation이 기존 read/write 권한, worktree, supervisor, receipt를 사용한다.
따라서 Native Agent Teams가 headless에서 생성되지 않아도 MVP는 가능하다.
CLI가 ordinary subagents를 추가로 만들 수 있으면 effective fan-out 정책을 검증하고,
엄격한 제한을 적용할 수 없는 route는 strict bounded mode에서 거절한다.

Native team branch는 먼저 capability/observation만 다룬다. Interactive session과
teammate를 정확히 구분하고 선택한 세션에만 연결한다. `-p` 실행에서 team 환경 설정을
켰다는 이유로 teammate가 생성되었다고 기록하지 않는다. Agent panel의 항목 또는
role name만으로 native team 형성을 추정하지 않는다.

[CLI 문서](https://code.claude.com/docs/en/cli-reference)의 JSON session 관측 같은
지원 표면을 검토하되 background session, ordinary subagent, teammate identity를
합치지 않는다. Native 상태 파일 읽기가 필요한 경우 기존 owned-session adapter의
범위·symlink/ownership·read bounds를 유지하고 전체 provider home 스캔은 금지한다.
관측 파일을 직접 수정하여 message/approval/task claim을 흉내 내지 않는다.

Interactive control이 안정적인 공개 interface로 증명되지 않으면 **관측만 지원**한다.
Tmux send-keys, 자동 승인 입력, 비공개 mailbox 쓰기를 기본 자동화로 구현하지 않는다.
새 SDK dependency나 auth mode 변경은 별도 검토다. 특히 headless isolation을 위해
`--bare` 같은 mode를 자동 적용하지 않는다. 공식 headless 문서는 해당 mode가 hooks와
인증 동작을 바꾼다고 설명하므로 기존 계정·결제·정책을 보존해야 한다.

### 완료 시험

Fake CLI의 ordinary subagent와 teammate metadata, headless/native mismatch, stale native
binding, missing usage, permission denial, session reconnect, duplicate deliveries,
cleanup uncertainty를 구분한다. 단일 기존 Claude invocation 결과와 no-team 기본 경로는
변경 없이 유지한다. Native start/control이 미지원이면 capability 결과와 이유를 그대로
반환한다. Team을 생성하지 않은 fixture를 Native Agent Teams live 시험으로 세지 않는다.

## 6. TM-09 — Antigravity: structured observation부터

Headless stream의 `subagent_info`에서 실제 보고된 type/role/conversation/workspace
관계를 읽는 bounded observer부터 구현한다. Native log/workspace URI는 untrusted
reference다. 임의 경로 접근이나 remote fetch 권한으로 사용하지 않으며 기존 허용
root·ownership·size·redaction 검사를 통과한 자료만 읽는다.

Stream-json의 init/step_update/result를 parser fixture로 고정한다. 연속 stdin user
입력은 한 turn의 result 뒤에 다음 prompt를 보내는 제어로 다루고, 이 기능을 임의
worker에 대한 mid-turn interrupt API로 해석하지 않는다. Streaming metadata의 누적
usage는 기존 session/account baseline으로 차분하며 step와 final을 중복 합산하지 않는다.

Swarm board UI는 협업을 보여주지만 OMS task DAG/acceptance의 권위가 아니다.
Swarm이 기본 workspace를 공유할 수 있으므로 Native write gate는 isolated working
copy의 실제 경계와 최종 patch 수집을 확인한다. Native worker 제거가 worktree를
삭제할 수 있는 lifecycle은 artifact/evidence 보존과 충돌하지 않아야 한다. OMS 최종
integration/publication 권한을 provider coordinator에게 넘기지 않는다.

개별 worker start/steer/cancel/resume, board cursor, all-descendant cleanup, hard fan-out
cap은 각각 **unknown until verified**다. 공식 API가 확인되지 않으면 adapter는 해당
control을 활성화하지 않는다. `agy -p` 지원 여부와 `/swarm` headless 제어 가능 여부를
하나의 Boolean으로 합치지 않는다. AGY SDK의 Python 예제가 있다는 이유로 OMS에
Python runtime을 다시 넣지 않는다.

### 완료 시험

Malformed/fragmented event, unknown subagent shape, missing terminal, duplicate step,
session-cumulative usage, parent/child overlap, unsafe URI, shared workspace, disappearing
working copy, quota stop, incomplete cancellation을 시험한다. AGY CLI read-only smoke와
Swarm live-control smoke는 서로 다른 증거다. 후자를 수행하지 않았으면 unsupported/
unknown matrix를 유지한다. ACP 확장은 RF-01/02와 합쳐 하나의 supervisor owner를 사용한다.

## 7. 모델 전환과 fallback

Lead의 phase별 모델 선택은 team topology와 별도 policy다. 초기에는 정적 role routing을
사용하고, retry/escalation은 동일 권한·남은 예산·횟수 제한 아래 새 attempt로 만든다.
다른 provider로 넘어갈 때 native conversation history가 그대로 이어졌다고 주장하지
않는다. Verified context/result packet으로 fresh invocation을 만들고 context omission을 기록한다.

Served model이 보고되지 않으면 requested model로 덮어쓰지 않는다. Subscription quota,
provider client estimate, API reported cost, OMS estimate를 분리한다. Authentication 실패나
rate limit을 API 결제 전환·다른 계정 자동 로그인·무제한 fallback의 이유로 쓰지 않는다.

## 8. PR 전달과 미완료 표기

TM-07/08/09는 각 provider의 capability/fixture/parser 경계를 기록한다. 현재 실행 순서는
[통합 작업 큐](ORCHESTRATOR-WORK-QUEUE.md)를 따르며 필요한 built-in 한 경로부터 연결한다.
세 provider의 native control 전체를 첫 managed 경로의 필수 선행 작업으로 만들지 않는다.
공통 frame/supervisor/usage/registry는 RF-01 및 TM-01~05에서 재사용하며 병렬로 같은
핵심 파일을 수정하지 않는다. Code PR에는 exact binary/schema/source version,
지원/미지원 표, canonical test와 public wiring, permission·output·cleanup 한계,
rollback 방법이 필요하다. 실제 모델 호출은 승인된 비용과 대상이 있을 때만 수행한다.

이 명세는 세 provider를 기능적으로 동일하다고 가장하지 않는다. `oms-team`은 일관된
공통 실행 계약을 제공하고 native-team은 검증된 기능만 선택적으로 추가한다.
