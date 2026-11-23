# Agent harness: 임의 에이전트 연결과 협업 표면 작업 패킷

기준 source: `1dfa9a491dbdb8917d2bec19f4c1eafce72a6404` (2026-11-21).
2026-11-23 main `051fd3ea`에서 다시 대조해 AH-05의 `tool_guard` 행과 AH-09의 MCP Room
경로를 정정했다.
상태: **명세 전용; 아래 패킷은 미구현·미측정**. 예외로 AH-05 1단계의 표 행은
main `18920ced`의 `tool_guard`로 존재한다.
관련: [provider capability 표](../config/provider-capabilities.json),
[provider routing과 custom adapter](../custom-skills/oms-agent-harness/references/provider-routing.md),
[Team provider adapters](TEAM-ORCHESTRATION-ADAPTERS.md),
[후속 연구 RF-01/02](RESEARCH-FOLLOWUPS.md), [Feature scope](FEATURE-SCOPE.md),
[통합 작업 큐](ORCHESTRATOR-WORK-QUEUE.md).

## 방향과 이 문서의 역할

2026-11-21 소유자 방향: **OMS는 특정 provider 묶음이 아니라 임의의 코딩 에이전트를
연결하는 하네스이자 에이전트 협업 도구로 동작해야 한다.** 이 문서는 그 기준에서 현재
source에 없는 연결을 13개 패킷으로 기록한다. 새 runtime, 상주 서버, 두 번째 상태 저장소를
요구하지 않으며 기존 supervisor·capability 표·Room·MCP producer를 확장하는 범위만 다룬다.

[Feature scope](FEATURE-SCOPE.md#provider-support)는 새 transport에 "이름 있는 반복
workflow"를, duplex/ACP에는 "기존 bounded CLI transport가 처리하지 못하는 provider
workflow"를 요구한다. 위 방향은 그 요구의 근거가 될 수 있지만 조건 문장을 자동으로
바꾸지 않는다. 각 구현 PR이 해당 조건의 충족 근거를 적고, 필요한 경우 같은 PR에서
Feature scope 문장을 고친다.

## 채택 상태

| ID | 채택 | 범위 / 흡수할 owner | 착수·보존 조건 |
|---|---|---|---|
| AH-01 | 유지 | capability 등급과 승격 기준; Room·council의 provider 상수 제거 | 표의 status 의미 불변; 증거 없는 승격 금지 |
| AH-02 | 흡수 | 네 native provider의 `unknown` 칸; TM-07/08/09, REL-01 owner | 새 표·새 fixture family 없이 기존 fixture 확장 |
| AH-03 | 유지 | custom adapter의 선택형 세션 계약 | RF-01 Reactor duplex 재사용; 기존 one-shot 계약 호환 |
| AH-04 | 조건부 | RF-02 ACP v1 client 하나 | AH-03의 frame·권한 경계 확정 뒤; 대상 provider와 workflow 명시 |
| AH-05 | 유지 | transport별 guard coverage 표기와 훅 없는 경로의 보완 | command guard 축소 금지; 근거 없는 `tool_guard` `supported`·`partial` 표기 금지 |
| AH-06 | 유지 | worker sandbox 적용 범위·network·쓰기 범위 | 기존 opt-in 의미 보존; 기본값 변경은 별도 승인 |
| AH-07 | 조건부 | CLI가 없는 모델의 carrier 경로 | OMS 내부 tool loop 추가 금지; 실제 요구와 carrier 선택 |
| AH-08 | 유지 | adapter conformance 시험과 live probe 구분 | 추론 호출·로그인·설치 없이 fake peer부터 |
| AH-09 | 조건부 | MCP의 Room 참여 표면 | CC-10 경계 유지: CLI 전체 복제 금지; 권한·admission 부여 없음 |
| AH-10 | 유지 | council 기본 좌석의 설정화 | 기본 좌석 불변; 설치만으로 fan-out 증가 금지 |
| AH-11 | 조건부 | 사람·다른 호스트가 같은 작업판을 읽는 경로 | CP-11/12/13과 multi-host Stage 순서; 원격 제어는 보류 유지 |
| AH-12 | 조건부 | OMS를 다른 client가 호출하는 표면 | 구체적 호출자 요구; AH-04 이후 |
| AH-13 | 흡수 | tmux 전제와 터미널 backend; CP-15 owner | native Windows 거부는 저장소 규칙으로 유지 |

**유지 6개, 흡수 2개, 조건부 5개.** ID는 근거 추적용이며 13개 독립 프로젝트나 출시
조건이 아니다. 우선순위는 측정된 병목 순위가 아니다.

## 1. 무엇을 다시 만들지 않는가

| 이미 있는 기능 | 확인 source | 보존할 점 |
|---|---|---|
| 23개 built-in transport와 read/write 계약 | [providers.rs](../crates/oms-command-peer/src/providers.rs) `builtin_binary`, `transport` | provider별 argv·권한 preset을 공통 계약으로 뭉개지 않음 |
| Marker 이름의 custom adapter 발견과 write opt-in | 같은 파일 `oms-agent-adapter-{provider}`, `OMS_PROVIDER_WRITE_ADAPTERS` | marker 없는 실행 파일은 발견하지 않음; write는 정확한 ID opt-in |
| 기능별 capability 표와 참조 검증 | [provider-capabilities.json](../config/provider-capabilities.json), [provider_check.rs](../crates/oms-command-lifecycle/src/provider_check.rs) | `supported`는 code path와 시험이 모두 있을 때만 |
| Raw duplex를 소유하는 Reactor | [process.rs](../crates/oms-supervisor/src/process.rs) `Request.duplex` | stdio·signal·deadline·cleanup의 소유자는 하나 |
| bwrap / sandbox-exec worker sandbox | [sandbox.rs](../crates/oms-supervisor/src/sandbox.rs) `wrap_command` | backend가 없을 때 strict는 실행 전 거절 |
| Room 참여·전송·updates cursor·ack | [oms-command-room](../crates/oms-command-room/README.md) | 참여·ack는 승인이나 admission이 아님 |
| 읽기 위주 MCP projection 32개 | [tools.json](../crates/oms-mcp-server/src/tools.json) | 응답 cap·root·withhold 규칙 |
| AGENTS.md와 skill의 provider별 설치 | [integrations.rs](../crates/oms-command-lifecycle/src/integrations.rs) | install/uninstall 소유권 전환의 가역성 |

## 2. 확인한 현재 경계

Capability 표의 `providers`는 23개다. `claude`, `codex`, `antigravity`, `pi`만 기능별
칸을 갖고 나머지 19개는 `exec-only` profile을 참조한다. `exec-only`는 `worker_call`만
`supported`, `stop`은 `partial`, `launch_main`·`observe`·`resume`·`send_message`·
`approval`·`usage`·`handoff`·`tool_guard`는 `unsupported`다. 그중 8개(`muse`, `kimi`, `kiro`,
`goose`, `cline`, `kilo`, `openhands`, `auggie`)는 `write-only-worker` override로 읽기
호출을 실행 전에 거절한다.

네 native provider도 `worker_call` 외에는 `partial`이 대부분이다. `approval`은 Codex와
Pi가 `unsupported`, Claude와 Antigravity가 `unknown`이다. `usage`는 Antigravity와 Pi가
`unsupported`다. `handoff`는 Pi가 `unsupported`, 나머지 셋이 `unknown`이다.
`tool_guard`는 Claude·Codex·Antigravity가 `partial`, Pi가 `unsupported`다.

이 수치는 표와 source 대조이며 설치된 CLI의 실제 동작을 실행해 본 결과가 아니다.

## 3. AH-01 — capability 등급과 승격 기준

### 확인한 문제

표에는 "네 provider"와 "그 외 전부"의 두 층만 있다. 세션을 재개할 수 있는 CLI와
한 번 실행만 되는 CLI가 같은 `exec-only`에 묶이고, 승격에 필요한 증거가 문서에 없다.
또한 등급이 표가 아닌 상수로 고정된 곳이 있다. Room의 Main 소유 provider는
[model.rs](../crates/oms-command-room/src/model.rs) 10행 `MAIN_OWNERS`에 네 이름으로
적혀 있어, 표에서 `launch_main`을 얻어도 Room에서는 Main이 될 수 없다.

### 구현 지시

1. 표에 중간 profile을 추가한다: 세션 identity와 `resume`·`stop`을 증명한 등급,
   그 위에 `usage`·`observe`를 증명한 등급. 이름과 칸 구성은 구현 PR이 정하되
   기존 `exec-only`와 네 provider의 현재 status는 바꾸지 않는다.
2. 등급별 승격 조건을 표의 `doc` 또는 인접 계약에 적는다. 조건은 code path,
   AH-08 conformance 통과, 고정된 binary version의 fixture 출처다.
3. `MAIN_OWNERS`는 Room log 검증([validate.rs](../crates/oms-command-room/src/validate.rs)
   349행)과 carry의 bind 판정(`carry.rs` 297행)이 함께 쓴다. 표 조회로 단순 치환하면
   표가 바뀔 때 이미 기록된 log의 유효성이 달라진다. 따라서 새 Main 소유 provider는
   `main-resume`처럼 구 writer가 거절하는 versioned log 규칙으로 추가하고, 기존 log의
   판정은 현재 네 이름으로 고정한다. 표 조회는 새 참여를 받는 지점에만 둔다.
4. [CC-01](CALLABLE-CONSOLIDATION-WORK-PACKETS.md)의 provider identity/alias 공유와
   같은 owner에서 진행한다. 두 번째 provider registry를 만들지 않는다.

### 완료 조건과 시험

`oms doctor --providers --check`가 새 profile의 참조를 검증한다. 표에서 한 provider의
등급을 내리면 Room의 Main 참여와 panel launch가 같은 판단으로 거절되는 fixture를
기존 Room 계약 시험과 panel launch 시험에 추가한다. 승격 fixture 없이 status만 올린
표는 검증에 실패해야 한다.

## 4. AH-02 — native provider의 `unknown` 칸 (흡수)

`unknown`은 구현 부족일 수도, fixture 부족일 수도 있다. 칸별로 어느 쪽인지 먼저
분류하고 [TM-07/08/09](TEAM-ORCHESTRATION-ADAPTERS.md)와
[REL-01](ORCHESTRATOR-RELIABILITY-WORK-PACKETS.md)의 해당 fixture에 연결한다.
별도 추적 표를 만들지 않는다. Codex `approval`, Antigravity `usage`처럼 `unsupported`인
칸은 provider가 그 표면을 제공하는지 공식 문서와 설치된 version으로 확인한 뒤에만
범위에 넣는다.

## 5. AH-03 — custom adapter의 선택형 세션 계약

### 확인한 문제

Custom adapter 호출은 `run --access --workdir --prompt-file [--model] [--effort]` 하나다
([providers.rs](../crates/oms-command-peer/src/providers.rs) 1766–1768행). 최종 답은
stdout, 진단은 stderr다. 세션 identity, 재개, 진행 중 event, 권한 요청, 사용량 보고,
취소 완료 신호가 없으므로 새 에이전트는 구현을 아무리 갖춰도 `exec-only`를 넘지 못한다.

### 구현 지시

1. 기존 one-shot 계약은 그대로 둔다. Adapter가 `--help` probe에서 세션 계약 version을
   광고할 때만 새 호출 형태를 사용한다. 광고가 없거나 해석할 수 없으면 one-shot이다.
2. 세션 계약은 [RF-01](RESEARCH-FOLLOWUPS.md)의 bounded duplex 위에 둔다. Frame 크기,
   대기 request 수, poll당 event 수, 출력 보존량에 유한한 상한을 두고 초과는 gap 또는
   typed failure로 기록한다. 별도 reader process를 만들지 않는다.
3. 최소 message 집합: 세션 시작과 identity, prompt 제출, 진행 event, 권한 요청과 응답,
   사용량 snapshot, 취소 요청과 종료 결과. 각 message는 OMS attempt와 연결 generation을
   함께 검사한다. 다른 세션·세대의 응답은 성공을 만들지 않는다.
4. 권한 요청은 사전 부여한 범위와 일치할 때만 허용한다. Adapter가 쓴 title이나
   label은 권한의 근거가 아니다. 응답하지 못한 요청은 취소로 정리한다.
5. 사용량은 단위·누적 범위·estimate 여부가 확인될 때만 반영하고 나머지는
   `unknown`으로 둔다. 물리 call receipt는 호출 전에 만든다.
6. Write 권한은 계속 `OMS_PROVIDER_WRITE_ADAPTERS`의 정확한 ID opt-in이다. 세션
   계약은 commit·push·landing 권한을 주지 않는다.

### 완료 조건과 시험

로컬 fake adapter로 시작 → prompt → 권한 요청 → 응답 → 사용량 → 종료를 검증한다.
진행 중 권한 요청에 응답해야 끝나는 fixture를 두어 종료 후 stdout을 한꺼번에 해석하는
구현이 통과하지 못하게 한다. 잘못된 version 광고, frame 초과, 다른 세대의 응답,
stdout 잡문, 취소와 종료의 경합에서 성공이나 권한이 생기지 않아야 한다. 기존 custom
adapter golden contract는 수정 없이 통과한다.

## 6. AH-04 — ACP v1 client 하나 (조건부, RF-02 활성화)

[RF-02](RESEARCH-FOLLOWUPS.md)의 계약을 그대로 사용한다. 이 패킷이 더하는 것은 대상
선정과, RF-02 작성 이후 달라진 protocol 상태의 반영이다.

- 대상은 현재 `exec-only`이면서 ACP agent 표면을 제공하는 CLI 하나로 정한다.
  선정 근거는 설치된 version의 `initialize` 응답이며 online 목록이 아니다.
- RF-02의 첫 범위는 재개를 제외했다. 아래 [외부 근거](#17-외부-근거와-확인-수준)에
  따르면 `session/resume`와 `usage_update`가 v1에서 안정화되었다. 구현 PR이 schema와
  설치된 agent의 capability 응답으로 다시 확인한 뒤, 광고된 경우에 한해 범위에 넣는다.
- v2는 draft 표기다. v1 협상에 실패하면 prompt를 보내지 않고, v2 전용 기능을 v1
  세션에서 가정하지 않는다.
- AH-03과 frame 상한·권한 판정·receipt 처리를 공유한다. 같은 Reactor 경로를 두 번
  구현하지 않는다. ACP를 모든 provider의 필수 dependency로 만들지 않는다.
- ACP SDK crate 도입은 새 의존성 결정이다. 추가한다면 정확 pin과 `unsafe` 사용 여부,
  build 비용을 PR에 기록하고, 직접 구현과 비교한다.

완료 조건은 RF-02의 합격·실패 시험이다. 통과한 provider는 AH-01의 등급 기준으로만
승격한다.

## 7. AH-05 — transport별 guard coverage

### 확인한 문제

Command guard, fail ledger, resume 같은 훅은 provider의 훅 표면에 설치된다.
설치 source([integrations.rs](../crates/oms-command-lifecycle/src/integrations.rs),
[plugins.rs](../crates/oms-command-lifecycle/src/plugins.rs))가 다루는 이름은 Claude,
Codex, Antigravity이고 Pi는 hook bridge([lib.rs](../crates/oms-command-hooks/src/lib.rs)
`pi_hook`)를 쓴다. 그 밖의 transport와 custom adapter는 CLI의 permission preset과
worktree 격리만 적용받는다. 2026-11-22 main `18920ced`가 capability 표에 `tool_guard`
기능을 추가했다. Claude·Codex·Antigravity는 설치 writer와 callback의 fixture 근거로
`partial`, Pi와 `exec-only`는 `unsupported`이며 live 전달과 차단은 확인하지 않았다.
`oms doctor --providers`는 이 정적 `tool_guard` 행을 이미 출력한다
([doctor.rs](../crates/oms-command-lifecycle/src/doctor.rs) `providers`). 어느 호출이 command
guard 아래에서 실행되었는지는 아직 호출 receipt나 panel 호출 상세에 남지 않는다.

### 구현 지시

1. 표의 기존 `tool_guard` 행을 재사용하며 새 기능 행을 추가하지 않는다. 값의 근거
   source와 시험은 현재 형식을 따른다.
2. 훅이 없는 transport의 write 호출은 AH-06 sandbox 적용 여부를 결과 receipt에
   남긴다. 보호 수단이 없는 조합은 실행 전에 그 사실을 표시한다.
3. 기존 guard의 범위를 좁히지 않는다. 훅 filter로 guard 실행을 줄이는 최적화는
   이 패킷의 범위가 아니다.

### 완료 조건과 시험

`oms doctor --providers`와 panel의 호출 상세가 같은 `tool_guard` status(`supported`,
`partial`, `unsupported`, `unknown`)를 보여 준다. 훅 없는 transport의 write fixture에서
`tool_guard`가 `unsupported`가 아니면 실패한다. 설치·제거 lifecycle 시험은 그대로 통과한다.

## 8. AH-06 — worker sandbox 적용 범위

### 확인한 문제

Worker sandbox는 구현되어 있지만 `OMS_WORKER_SANDBOX`가 `auto` 또는 `strict`일 때만
적용된다. 값이 없으면 감싸지 않는다([providers.rs](../crates/oms-command-peer/src/providers.rs)
2089–2131행). `auto`는 Codex를 건너뛴다. 이 호출 경로는 `allow_network: true`를 고정으로
넘기고, 사용자 홈의 provider 설정 directory와 일반 config·cache directory를 쓰기
가능으로 연다. 따라서 custom adapter와 훅 없는 transport는 기본 상태에서 호스트 사용자의
network와 자격증명을 그대로 가진다.

### 구현 지시

1. 먼저 계측한다: transport별로 sandbox가 적용된 호출 수, backend, 거절 수.
   기존 receipt에 backend 이름을 남기는 것으로 충분한지 확인한다.
2. Network 차단은 대부분의 route에 쓸 수 없다. Provider CLI가 추론 요청에 network를
   쓰므로 sandbox에서 network를 끊으면 호출 자체가 실패한다. 선택지는 로컬 모델을
   쓰는 adapter처럼 추론에 외부 network가 필요 없는 route로 한정하고, 그 밖의 route는
   현재의 허용을 유지한다. Host 단위 허용 목록은 이 backend 범위 밖이다.
3. 쓰기 가능 directory를 route의 provider에 필요한 것으로 좁힌다. 좁힌 뒤 인증이
   깨지는 provider는 목록에 남기고 이유를 적는다.
4. Custom adapter와 guard coverage가 없는 transport에 `auto`를 기본 적용하는 안은
   **기본값 변경**이다. 계측과 호환 fixture를 갖춘 별도 PR로 제안하고 승인받는다.

### 완료 조건과 시험

기존 `adaptive_routing_enforces_strict_sandbox_without_backend`와 rw directory fixture를
확장한다. Network를 끈 route에서 backend가 없으면 실행 전에 거절한다. macOS
`sandbox-exec` 경로는 실행 검증 여부를 PR에 구분해 적는다.

## 9. AH-07 — CLI가 없는 모델 (조건부)

OMS는 tool loop를 소유하지 않고 CLI transport를 호출한다. `oms harness`도 설치된
CLI의 UI와 tool loop를 고르는 [bridge](CODEX-HARNESSES.md)다. API나 로컬 endpoint만 있는
모델을 직접 붙이는 경로는 없으며, OpenCode·Aider·Goose 같은 multi-model carrier가
`provider:model=...` 형태로 모델을 싣는다. 이 패킷은 OMS 안에 추론 client나 tool loop를
추가하지 않는다. 실제 요구가 생기면 (a) 기존 carrier로 충분한지, (b) AH-03 세션 계약을
구현한 얇은 adapter recipe가 필요한지 순서로 판단한다. 모델 family 추론이 `unknown`인
carrier 결과를 독립 증거로 세지 않는 현재 규칙을 유지한다.

## 10. AH-08 — adapter conformance 시험

### 확인한 문제

`oms doctor --providers --check`는 표가 가리키는 source와 시험 참조의 존재를 검증한다
([provider_check.rs](../crates/oms-command-lifecycle/src/provider_check.rs)). Provider를
실행하지 않는다. 새 adapter 작성자가 계약 준수를 스스로 확인할 방법이 없고, 승격
판단(AH-01)에 쓸 공통 증거 형식도 없다.

### 구현 지시

1. One-shot 계약과 AH-03 세션 계약 각각에 대해 결정적 conformance 시나리오를 둔다:
   probe 상한, prompt가 `-`로 시작하는 입력, 읽기 호출의 쓰기 시도, 종료 code 전달,
   background process 잔류, 출력 상한 초과.
2. 대상은 adapter 실행 파일 하나다. 시험은 임시 저장소와 disposable worktree에서
   돌고 추론 요청·로그인·설치·설정 변경을 하지 않는다. 추론이 필요한 항목은
   adapter가 제공하는 fixture mode가 있을 때만 실행하고 없으면 `unknown`으로 남긴다.
3. 결과는 capability 표와 같은 status 어휘로 기능별로 낸다. 통과 결과를 표에
   자동으로 쓰지 않는다. 표 변경은 검토되는 commit이다.
4. 실제 provider를 호출하는 live probe는 별도 명시 옵션과 비용·계정 고지가 있을
   때만 실행하며 결정적 conformance와 결과를 섞지 않는다.

One-shot 계약의 시나리오는 reference 스크립트 초안
([#319](https://github.com/eightmm/oh-my-setting/pull/319))으로 먼저 고정한다. Native
경로는 같은 시나리오와 status 어휘를 옮긴다. 2026-11-23 기준 #319와 native doctor 연결
[#333](https://github.com/eightmm/oh-my-setting/pull/333)은 open draft이며 main에 없다.

### 완료 조건과 시험

저장소의 fixture adapter(`oms-agent-adapter-fixture`)가 통과하고, 계약을 어기는 변형
(background process 잔류, 읽기 호출에서 worktree 변경)이 해당 항목에서 실패한다.
CI에는 결정적 시나리오만 넣는다.

## 11. AH-09 — MCP의 Room 참여 표면 (조건부)

### 확인한 문제

MCP 도구 32개 중 `readOnlyHint`가 false인 것은 `oms_peer_start`와 `oms_state_compact`
둘이다. Room의 send·updates·ack는 이미 `oms_peer_start`(message·ack)와 `oms_peer_result`
(thread updates)의 `participant` 경로로 열려 있다([peer.rs](../crates/oms-mcp-server/src/peer.rs)
`thread_exchange`). 이 경로는 기존 Room과 joined participant만 받고 `oms room` argv를
조립해 실행한다. Join 표면은 없고, `participant`는 호출자가 낸 문자열이어서 호출자
identity와의 일치를 강제하지 않는다.

### 구현 지시

1. 범위는 빠진 join과 participant identity 결속이다. 기존 send·updates·ack 경로를
   재사용하고 두 번째 Room 도구를 만들지 않는다. Plan claim, task 변경, patch
   admission, landing은 포함하지 않는다.
2. [CC-10](CALLABLE-CONSOLIDATION-WORK-PACKETS.md)의 경계를 지킨다. CLI와 MCP의
   노출 범위를 같게 만드는 것은 목표가 아니다. 구체적인 MCP 전용 참여자가 있을 때
   착수한다.
3. 새 join 표면은 native Room core를 직접 호출하고 argv를 조립해 CLI를 실행하지 않는다.
   참여자 identity는 호출자가 임의로 고를 수 없게 server 구성에서 온 값에 묶는다.
   기존 send·ack·updates 경로도 아직 `oms room` argv를 조립한다
   ([peer.rs](../crates/oms-mcp-server/src/peer.rs) `thread_exchange`). 이 경로를 native Room
   core로 옮기는 일도 join·identity 결속과 함께 남은 작업이다. 참고로 Room relay의 child
   session은 이미 send·ack를 한 participant로 고정한다
   ([relay.rs](../crates/oms-command-room/src/relay.rs)의 participant 검사).
4. Room의 기존 규칙을 그대로 따른다: 참여·ack는 승인이나 admission이 아니다.
   전송 결과가 불명확하면 불명확으로 반환한다. 응답은 기존 cap과 scrubber를 지난다.

### 완료 조건과 시험

기존 [room_contract](../crates/oms-command-room/tests/room_contract.rs)의 join/send/
`updates --after`/ack 시나리오를 MCP 경로로 반복해 같은 log가 만들어지는지 비교한다.
다른 참여자 이름으로의 전송, 닫힌 Room, 상한 초과는 CLI와 같은 오류로 거절한다.

## 12. AH-10 — council 기본 좌석의 설정화

### 확인한 문제

`peer-ask`와 `peer-review`의 기본 좌석은 [peer.rs](../crates/oms-command-peer/src/peer.rs)
1082행과 1099행에 `codex`, `claude`, `antigravity`로 적혀 있다. 다른 transport를 쓰는
설치에서는 매 호출마다 `--providers`를 지정해야 한다. `config/panel-routing.json`의
`council.extra`는 panel 경로의 추가 좌석만 다룬다.

### 구현 지시

1. 기본 좌석을 설치가 정할 수 있게 한다. 설정이 없으면 현재 세 이름이다. 두 경로가
   있다. (a) `OMS_ADVISOR_PROVIDER`·`OMS_CONSULT_PROVIDER`와 같은 환경 변수: `peer.rs`
   한 곳의 변경이다. 초안 [#318](https://github.com/eightmm/oh-my-setting/pull/318)은
   2026-11-23 기준 open draft이며, 확인한 local ref(`a67c125c`)는 `OMS_PEER_PROVIDERS`를
   추가한다. 그 뒤 head가 (b)의 설정 키까지 연결한다는 내용은 PR 설명 기준, 미확인이다.
   (b) `config/panel-routing.json`의 새 키: 이 정책은 schema 2이고 알 수 없는 field를
   거절하므로 `oms-panel-core`의 정책 struct와 schema 판단이 함께 바뀐다. Panel council은
   이미 이 정책에서 좌석을 만들므로 고정값은 CLI 경로에만 있다. 새 설정 파일은 만들지 않는다.
2. 설정된 좌석도 현재 규칙을 그대로 통과해야 한다: 발견·usability probe, 읽기 모드가
   문서화된 transport만 read/council 좌석, writer와 같은 model family의 경고.
3. 설치된 CLI가 늘어난다고 좌석이 자동으로 늘지 않는다. 좌석 변경은 명시적 설정이다.

### 완료 조건과 시험

설정이 없을 때 기존 peer 계약 시험이 수정 없이 통과한다. Write-only transport를 좌석에
넣은 설정은 실행 전에 거절한다. 설정된 좌석과 `--providers`가 함께 있으면 명시 인자가
우선한다.

## 13. AH-11 — 사람과 다른 호스트가 보는 작업판 (조건부)

Room의 역할은 `main`, `worker`, `advisor`, `reviewer`, `researcher`이며 사람은 참여자가
아니라 지연 escalation의 대상이다. Room·Plan 상태는 저장소의 로컬 state root에 있다.
두 사람 또는 두 호스트가 같은 작업판을 보는 경로는 [CP-11](CONTROL-PLANE-EVOLUTION.md)
(읽기 전용 다중 호스트 관측), CP-12(로컬 읽기 전용 웹 뷰), CP-13(원격 typed control,
기본 보류)와 [multi-host 계획](MULTI-AGENT-MULTI-HOST-PLAN.md)이 소유한다. 이 패킷은
그 순서를 바꾸지 않고 요구만 기록한다: 첫 단계는 읽기 전용 공유이며, 원격에서 Room에
쓰거나 승인하는 경로는 identity·권한 계약이 생기기 전에는 열지 않는다. 중앙 server나
mesh 권위는 [참고 대장](CONTROL-PLANE-REFERENCES.md)의 비채택 결정을 따른다.

## 14. AH-12 — OMS를 호출하는 표면 (조건부)

다른 orchestrator나 editor가 OMS에 작업을 맡기는 표준 표면은 MCP의 `oms_peer_start`
하나다. OMS가 ACP agent 또는 A2A server 역할을 하는 구현은 없다. 구체적인 호출자가
있을 때만 검토하며, 그 경우에도 OMS의 admission·review·landing gate를 건너뛰는 입구를
만들지 않는다. A2A는 불투명한 원격 agent 사이의 task 교환을 다루므로 로컬 worktree와
patch admission이 중심인 OMS의 첫 후보는 아니다. AH-04가 끝난 뒤 재평가한다.

## 15. AH-13 — tmux 전제와 지원 host (흡수)

Native chat board는 tmux가 PATH에 있어야 한다
([native_panel.rs](../crates/oms-command-panel/src/native_panel.rs) 623행). 다른 터미널
backend는 [CP-15](CONTROL-PLANE-EVOLUTION.md)가 소유한다. Native Windows 거부는
[저장소 규칙](../AGENTS.md)이며 이 문서는 바꾸지 않는다. 임의 에이전트 연결(AH-03/04)은
panel 없이 `oms consult`·`oms peer-delegate`로 동작해야 하므로 tmux를 새 선행 조건으로
만들지 않는다.

## 16. 순서와 PR 제출 조건

| 단계 | 패킷 | 이유 |
|---|---|---|
| 1 | AH-08 | 승격과 새 adapter 수용의 판정 기준이 먼저 필요하다 |
| 2 | AH-01, AH-10 | 표와 설정을 읽는 작은 변경; 서로 다른 파일 |
| 3 | AH-03 | 세션 계약; RF-01 재사용 |
| 4 | AH-05, AH-06 계측 | 표기와 계측은 기본값 변경 없이 가능 |
| 5 | AH-04 | AH-03의 경계를 공유하는 ACP client |
| 조건 충족 시 | AH-07, AH-09, AH-11, AH-12 | 위 표의 착수 조건 |

같은 파일(`providers.rs`, capability 표)을 바꾸는 PR은 순차 병합한다. 각 PR은 최신
main 기준 한 단계이며 before/after를 적는다: 표 status 변화, 추가된 fixture, 실행하지
못한 검증. Fixture 통과를 live 호환으로, 문서 병합을 기능 완료로 적지 않는다.

## 17. 외부 근거와 확인 수준

확인 수준은 [참고 대장](CONTROL-PLANE-REFERENCES.md)의 어휘를 따른다. 아래는 모두
**조사**(검색 결과 발췌를 읽고 요약; 해당 페이지 전문·schema·SDK source는 직접 확인하지
않음)이며 확인일은 2026-11-21이다. 구현 근거로 쓰기 전에 정확한 version과 schema를
다시 고정한다.

| 출처 | 내용 | 연결 |
|---|---|---|
| [ACP updates](https://agentclientprotocol.com/updates) | `session/resume` 안정화(2026-04-22), `usage_update` 안정화, v2 draft 공개(2026-07-20) | AH-04 |
| [ACP v2 migration](https://agentclientprotocol.com/protocol/v2/migration) | version 협상 방식 유지, v2는 draft 표기, v1 peer 지원을 함께 유지하라는 권고 | AH-04 |
| [ACP registry](https://agentclientprotocol.com/get-started/registry) | 인증을 지원하는 agent의 curated 목록; 여러 CLI agent가 등록됨 | AH-04 대상 선정의 출발점 |
| [A2A project](https://a2a-protocol.org) | Agent Card와 task lifecycle 기반의 agent 간 protocol, 1.0대 안정판 | AH-12 |
| [Agent Skills](https://agentskills.io) | `SKILL.md` folder 형식의 공개 표준 | 기존 skill 설치 경로; 새 패킷 없음 |

확인 범위: capability 표 전체, `providers.rs`의 발견·custom adapter·write opt-in·sandbox
호출부, `sandbox.rs`의 backend 선택, `peer.rs`의 좌석 기본값, Room `model.rs`의 역할과
Main 소유 상수, MCP `tools.json`의 도구와 annotation, doctor의 provider 경로.
미확인 범위: 각 provider CLI의 실제 실행, 훅이 설치된 뒤 각 CLI가 실제로 호출하는지, macOS
sandbox 경로, ACP·A2A의 schema와 SDK. 작성 환경에 고정 toolchain이 없어 Rust 시험을
실행하지 않았다.

## 18. 비용 관점에서 다시 본 순서 (2026-11-21 보강)

협업과 위임의 token 비용을 기준으로 16절의 순서를 다시 본 결과다. 새 패킷이나 새
계측을 더하지 않는다. 필요한 값은 이미 기록되고 있으므로 설치된 저장소에서 먼저 읽는다.

| 먼저 읽을 값 | 기존 출처 | 값에 따른 판단 |
|---|---|---|
| role별 admit당 token, admit되지 않은 위임 token | `oms artifact-index telemetry --by role` | write role의 미admit 비중이 높으면 Main의 위임 기준 문구를 검토한다. 낮으면 문구를 바꾸지 않는다 |
| role별 cache-read share | 같은 명령 (`OMS_TELEMETRY_CACHE_MIN_PCT` 경고) | 낮은 role은 호출마다 새 세션을 여는 경로다. AH-03/04의 세션 재개가 비용 근거를 얻는다 |
| 반복 실패 위임, 세션별 훅 주입 byte, 큰 저장 출력 | `oms artifact-index telemetry --waste` | 상위 항목이 가리키는 brief·훅·출력 하나를 고친다. 일괄 축소를 하지 않는다 |
| MCP 도구 정의 크기 | `oms doctor --mcp --probe-tools NAME`, [tools.json](../crates/oms-mcp-server/src/tools.json) | 32개 정의는 compact JSON으로 약 26 KB다(`051fd3ea`에서 26,282 byte). client가 전부를 세션에 싣는지 provider별로 확인한 뒤에만 subset을 검토한다 |

위 값은 provider가 보고한 수치이며 가격이나 승인 근거가 아니다. 수치를 읽기 전에는
위임 기본값, council 좌석, 도구 설명을 비용 명목으로 바꾸지 않는다.

비용을 기준으로 삼으면 AH-03과 AH-04가 AH-01·AH-10보다 앞선다. 사용량 보고와 세션
재개가 없는 transport는 tier에 넣어 비교할 수 없고, 호출마다 cache 없이 저장소를 다시
읽기 때문이다. 순서는 **기존 값 읽기 → AH-08 → AH-03(사용량·재개 message 먼저) → AH-04
→ 나머지**다. 실행 비교와 작업별 성과 계약은 [후속 연구](RESEARCH-FOLLOWUPS.md)의
RF-06 owner가 다루며 이 문서에서 복제하지 않는다.

정정 기록: 이 절을 쓰면서 "admit된 patch당 token" 지표가 없다고 보고 낭비 뷰에 같은
값을 추가하는 PR을 열었으나, `--by role`이 이미 `tokens_per_admitted`를 보고하고 있어
닫았다. 새 지표를 제안하기 전에 [telemetry 문서](COMPONENTS.md)의 기존 값을 확인한다.
