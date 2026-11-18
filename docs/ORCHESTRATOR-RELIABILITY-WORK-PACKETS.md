# Orchestrator 신뢰성 구현 작업

**상태: DESIGN ONLY — 구현·실 provider 실행·성능 검증 완료를 의미하지 않는다.**
소스 기준은 `f9a695ea2de90a160754252c1ee9cbaa4dd7a488`, 공식 문서 확인일은 2026-11-18이다.
이 문서는 현재 Rust 경로에서 확인한 빈틈을 독립적으로 검토할 수 있는 코드 PR 단위로 나눈다.
[PR #259](https://github.com/eightmm/oh-my-setting/pull/259)의 retirement, task/worktree receipt,
proposal apply, composite acceptance 설계는 별도 선행 작업이다. 여기서 다시 정의하지 않는다.
새 daemon, Python 구현, 두 번째 scheduler/DB, provider 권한·과금 기본값 변경을 제안하지 않는다.

## 1. 기존 구현과 추가할 부분

| 작업 | 이미 있는 구현 | 추가할 검증 가능한 결과 |
| --- | --- | --- |
| REL-01 | Claude/AGY native child transcript 관측, PID 전후 확인, 읽기·표시 상한 | 관측 불가·일부만 관측·실제 0개를 구분하는 상태 투영 |
| REL-02 | account overflow, 명시적 model fallback, clean-tree guard, physical receipt | provider 실패 근거와 작업 출력의 분리, 자동 재실행 적격성 |
| REL-03 | 개별 호출 timeout, campaign admission, account cooldown 순위 | 선택적 총 operation deadline과 cooldown admission 정책 |
| REL-04 | Codex auth/catalog Broker, 크기 제한 reader, 128개 응답 queue | 쓰기부터 종료까지 deadline·취소·backpressure를 갖춘 기존 Reactor 연결 |

각 작업은 지원되지 않는 native 기능을 OMS가 제공한다고 표시하지 않는다.
Native transcript의 완료 표시는 provider가 보고한 상태이며 task acceptance나 process cleanup 증거가 아니다.
알 수 없는 사용량·계정 적격성·관측 총수는 계속 unknown으로 남긴다.

## 2. REL-01 — native child 관측 범위와 누락을 표시한다

### 근거와 재사용

[native_children.rs](../crates/oms-command-panel/src/native_children.rs)의 `observe`는 provider별
`Option<Vec<Value>>`를 빈 배열로 합친다. 지원하지 않는 provider, source 부재·읽기 실패,
parent 매칭 실패, 관측 중 PID identity 변경이 모두 child 0개와 같은 출력이 된다.
`jsonl`은 파일 4 MiB, 한 줄 256 KiB 상한을 두고 invalid JSON을 생략한다.
Claude와 AGY는 각각 발견한 child 중 최근 3개만 반환하며 생략 수를 전달하지 않는다.
AGY parent 탐색은 directory entry 2,048개와 최근 후보 24개로 제한된다.
Child transcript 읽기에 실패해도 terminal handback이 없으면 row의 `state`는 `working`이다.

[orchestrator.rs](../crates/oms-command-panel/src/orchestrator.rs)의 `observe`,
[render.rs](../crates/oms-command-panel/src/render.rs)의 native row projection을 그대로 연결점으로 쓴다.
현재 `native_children.rs` 전용 fixture는 확인되지 않았다. 기존 panel test 경계를 확장한다.
이 작업은 [RESEARCH-FOLLOWUPS.md](RESEARCH-FOLLOWUPS.md)의 RF-03 live stream stall detector와 다르다.
이미 존재하는 transcript observer가 얼마나 확인했는지 정확히 보고하는 범위다.

### 구현 범위

1. 내부 결과를 rows와 observation metadata로 나눈다. 이름·wire schema는 코드 PR에서 확정한다.
   Provider, source/parser version, 관측 시각, 현재 parent/pane/process identity,
   coverage와 누락 사유를 전달한다. 기존 row 소비자의 호환 경로도 함께 갱신한다.
2. 완전 관측, 부분 관측, unavailable, unsupported, stale, ambiguous를 구분한다.
   Source가 없다는 이유로 `observed_zero`를 만들지 않는다. 총수를 모르면 null로 둔다.
   Parse 실패·초과 파일·부분 마지막 줄·탐색 상한·표시 상한은 각각 evidence를 남긴다.
3. Child source가 없거나 오래되었을 때 관측되지 않은 실행 상태를 `working`으로 단정하지 않는다.
   Provider가 보고한 terminal, 살아 있는 process, OMS acceptance는 별도 근거로 유지한다.
4. 기존 Store 경계, redaction, 크기 상한, PID 전후 확인을 보존한다.
   AGY prompt anchor는 관측 상관관계다. canonical Team ownership이나 전체 child 명단으로 승격하지 않는다.
   숨겨진 source를 더 넓게 뒤지는 방식으로 누락을 해결하지 않는다.

### 실패 사례와 acceptance

- Child 5개 fixture에서 3개 row와 알려진 생략 수를 표시한다. 전체 탐색을 못 했다면 전체 수는 unknown이다.
- 빈 정상 source, source 부재, oversized source, malformed line, truncated tail이 같은 상태가 되지 않는다.
- PID 재사용·AGY 중복 parent·후보 탐색 상한 초과는 완전 관측으로 표시하지 않는다.
- Child source 부재가 `working`의 근거가 되지 않고, native terminal이 canonical task를 완료시키지 않는다.
- 기존 panel에서 정상 Claude/AGY row와 redaction이 유지되며 provider 실행이나 제어 RPC가 추가되지 않는다.

**의존성:** REL-01의 coverage 계약을 먼저 합의한 뒤 PERF-01 증분 읽기를 구현한다.
두 코드 PR은 `native_children.rs`를 함께 수정하므로 순차 병합한다.
PERF-01은 rotation·truncate·cache hit에서도 이 누락 의미를 보존한다. 계측 계획은 그 작업이 소유한다.

## 3. REL-02 — 실패 유형과 자동 재실행 근거를 분리한다

### 근거와 재사용

[providers.rs](../crates/oms-command-peer/src/providers.rs)의 `capacity`는 nonzero 또는 incomplete
결과의 decoded stdout와 stderr에서 `quota`, `rate limit`, `capacity` 등의 문자열을 찾는다.
`provider_fault`도 code·완료 여부와 문자열을 함께 사용한다. 따라서 실패한 작업의 설명이나
tool 출력에 해당 단어가 들어간 경우를 실제 provider capacity rejection과 구분하지 못한다.
`decode_output`은 provider envelope를 `(String, bool)`로 평탄화해 오류 근거를 보존하지 않는다.
이는 성공 출력의 모든 `quota` 문자열이 재시도를 일으킨다는 뜻은 아니다.

`execute_env`의 같은 provider 내 1회 account overflow와 `execute_with_fallback_env`의 명시적
model fallback, pinned account, unchanged-tree 검사, physical admission·usage settlement를 재사용한다.
현재 두 account와 두 model 조합은 최대 4회 physical call이며 별도 repair round는 별개다.
[peer_contract.rs](../crates/oms-command-peer/tests/peer_contract.rs)의
`retries_reselect_unpinned_accounts`는 4회 호출과 untracked 파일 변경 시 재시도 중단을 이미 확인한다.

### 구현 범위

1. Raw native envelope를 평탄화하기 전에 실패 종류·근거 출처·실패 단계·수락 여부를 추출한다.
   Capacity, auth/permission, unsupported model, timeout/cancel, task failure, unknown을 구분한다.
   이 목록은 내부 설계안이며 모든 provider의 동일한 wire code를 가정하지 않는다.
2. 문서화된 structured error를 우선하고 텍스트 전용 provider는 확인한 diagnostic fixture로 제한한다.
   인용문·사용자 답변·tool 본문의 문자열만으로 capacity를 확정하지 않는다.
   분류할 수 없으면 unknown으로 반환하고 근거 없는 fallback을 새로 허용하지 않는다.
3. `retryable`과 `safe_to_replay`를 분리한다. Clean git tree만으로 외부 부작용 부재를 증명할 수 없다.
   명확한 실행 전 거절이나 기존 idempotency 근거가 없고 작업 수행 여부가 모호하면
   partial evidence를 남겨 caller가 continuation을 결정하도록 한다. 이는 새 정책 제안이다.
4. 기존 account pin·동일 provider 경계·명시적 fallback model·횟수 상한을 유지한다.
   Receipt/settlement 실패 후 재호출 금지, unknown usage 보존도 유지한다.
   지원할 built-in 하나부터 변경하고 검증하지 않은 provider는 기존 지원 범위와 차이를 명시한다.

### 실패 사례와 acceptance

- 작업 실패 설명에 인용된 `quota`와 실제 trusted capacity 오류를 구분한다.
- Auth/permission·cancel·timeout·부분 실행 후 불확실한 종료가 자동 model/account 전환을 허용하지 않는다.
- 명확한 capacity fixture는 기존 상한 안에서만 retry하며 pinned account를 바꾸지 않는다.
- 기존 physical receipt fixture에서 call ID는 매번 다르고 operation ID와 usage evidence는 연결된다.
- 분류 결과가 없는 usage를 0으로 만들지 않으며 receipt 실패는 다음 physical call을 막는다.

**의존성:** 독립 코드 PR로 시작한다. REL-03의 cooldown admission에는 이 분류 근거를 전달한다.
기존 synthetic stderr fixture를 그대로 authoritative provider 오류로 취급하지 않고 출처를 명시한다.

## 4. REL-03 — 총 operation 시간과 cooldown admission을 제한한다

### 근거와 재사용

[providers.rs](../crates/oms-command-peer/src/providers.rs)의 `execute_env`는 overflow iteration마다
`Invocation.timeout = Duration::from_secs(timeout)`을 다시 만든다. Model fallback도 같은 timeout을 전달한다.
[delegate.rs](../crates/oms-command-peer/src/delegate.rs)의 repair loop는 이 호출을 다시 수행할 수 있다.
현재 timeout은 개별 physical call의 한도이며 모든 재시도·repair를 합친 일반 operation 한도는 아니다.
Campaign 경로의 physical admission과 deadline 검사는 이미 있으므로 이를 누락 기능으로 분류하지 않는다.

`record_seat_health`는 capacity hit에 300초 cooldown을 기록하지만 `select_account_adaptive`는
cooldown을 정렬 우선순위로 사용한다. 모두 cooldown 중이어도 첫 eligible account를 고를 수 있다.
이는 현재 문서화된 동작이며 cooldown 기록이 없는 것이 아니다.
해당 health는 repository별 상태이므로 다른 repository까지 적용되는 전역 provider 제한이 아니다.

### 구현 범위

1. 기존 per-call `--timeout` 의미를 유지하고 기존 request/task admission 경계에 선택적인
   operation deadline 정책을 추가한다. 정책 미지정 시 기존 상한과 동작을 보존한다.
   동일 logical operation의 account overflow·model fallback·repair에 단일 budget을 전달한다.
2. 각 호출은 per-call 한도, 남은 operation 시간, 상위 campaign 한도 중 작은 값을 사용한다.
   대기·backoff도 budget에 포함하며 monotonic clock으로 잔여 시간을 계산한다.
   Native timeout hint 반올림으로 0이 unlimited가 되는 경로를 허용하지 않는다.
3. Goal deadline 후 신규 호출을 막되 기존 process cleanup과 usage settlement는 별도의 bounded grace로 끝낸다.
   시간 종료를 이유로 physical receipt나 이미 발생한 비용의 관측을 버리지 않는다.
4. Trusted capacity에 한해 선택적 cooldown admission을 제공한다. 모두 대기 중이면
   `blocked_until` 또는 unknown 상태를 caller에게 반환한다. 무한 sleep·busy retry·숨은 scheduler를 만들지 않는다.
   Explicit account는 계속 고정한다. 관측되지 않은 reset 시각·전역 quota를 추정하지 않는다.
   REL-02의 분류 변경 전에는 새 cooldown admission을 활성화하지 않는다.

### 실패 사례와 acceptance

- Fake clock으로 각 retry의 잔여 시간이 줄고 만료 후 physical launch가 없음을 확인한다.
- 정책 없는 기존 최대 4회 fixture와 정책 있는 조기 종료 fixture를 모두 유지한다.
- 모든 seat가 cooldown일 때 선택적 정책은 차단을 반환하며 explicit account를 바꾸지 않는다.
- Campaign의 더 짧은 한도와 pinned account·receipt fence를 우회하지 않는다.
- Deadline 직후 cleanup/settlement와 unknown usage가 보존되고 다른 repository를 전역 차단하지 않는다.

**의존성:** 총 deadline 전달과 cooldown admission은 caller 범위가 달라 두 코드 PR로 나눌 수 있다.
전자는 기존 operation/campaign 계약을 재사용하고 후자는 REL-02 뒤에 연결한다.
REL-04는 이 task 실행 정책과 독립적인 auth/catalog transport 작업이다.

## 5. REL-04 — Codex Broker를 기존 bounded Reactor에 연결한다

### 근거와 재사용

[broker.rs](../crates/oms-command-providers-skills/src/broker.rs)의 `Broker::start`는 Codex app-server를
직접 spawn하고 reader thread와 `sync_channel(128)`을 만든다. 한 frame의 1 MiB 상한도 이미 있다.
`call`은 blocking `writeln!`과 `flush`가 끝난 뒤 20초 response deadline을 시작한다.
따라서 stdin을 소비하지 않는 child에 대한 write, initialize의 후속 notification, 전체 pagination,
`Drop`의 kill/wait까지 같은 deadline과 취소가 덮는 구조는 아니다.
Reader의 malformed frame·oversize·queue full·EOF는 구분 없이 종료된다.
`id`가 있는 row만 통과시키며 응답 envelope와 역방향 request를 구분하지 않는다.

[models.rs](../crates/oms-command-providers-skills/src/models.rs)의 refresh와 `Broker::model`은
각각 최대 20 page를 개별 `call`로 읽는다. Page 상한은 전체 elapsed-time 한도가 아니다.
이 Broker는 auth/catalog 경로다. Native Team의 managed execution bridge가 구현되었다는 증거가 아니다.

[oms-supervisor/process.rs](../crates/oms-supervisor/src/process.rs)의 `DuplexOptions`,
`Reactor::send/receive/poll/close_input/stop`, owned cleanup을 재사용한다.
RF-01의 duplex 기반은 이미 있으므로 별도 process supervisor나 새 비동기 framework를 만들지 않는다.
현재 crate 의존성에 연결이 필요하므로 기존 workspace dependency 방향도 코드 PR에서 확인한다.

### 구현 범위

1. Start/call/shutdown에 caller cancellation과 절대 deadline을 전달한다.
   Enqueue·write·read·handshake·pagination을 같은 제한에 넣고 cleanup grace를 별도로 제한한다.
   Reactor의 send 성공은 queue admission이며 child acknowledgement가 아님을 유지한다.
2. Frame bytes·input/output queue bytes·event count를 모두 제한한다. Queue 포화는 유실 없는
   성공으로 처리하지 않고 명시적인 오류·backpressure로 보고한다.
   Timeout, disconnect, malformed frame, saturation, protocol error를 bounded redacted metadata로 구분한다.
3. Request ID와 connection generation 및 response envelope를 함께 검사한다.
   역방향 request를 같은 ID의 응답으로 소비하지 않는다. 지원하지 않는 request는 명시적으로 거절한다.
   Auth/catalog 범위에 permission responder나 자동 승인 기능을 추가하지 않는다.
4. 기존 subscription env, credential zeroing/redaction, 401 refresh 범위를 보존한다.
   Pagination 상한·deadline에 막힌 목록은 완전한 최신 catalog로 게시하지 않는다.
   종료 증거는 실제 supervisor scope만 보고하며 지원되지 않는 whole-tree 종료를 주장하지 않는다.

### 실패 사례와 acceptance

- Fake app-server가 stdin을 읽지 않아도 caller deadline/interrupt로 빠져나온다.
- Fragmented/oversized frame, malformed JSON, 128개 초과 burst, 같은 ID의 역방향 request를 구분한다.
- Pagination 중 만료·반복 cursor·half-close 후 final response에서 성공/부분/실패가 섞이지 않는다.
- Credential이 fixture·오류·log에 출력되지 않으며 provider login이나 유료 호출 없이 검증한다.
- 기존 [duplex_whole_tree.rs](../crates/oms-supervisor/tests/duplex_whole_tree.rs)의 cleanup 범위를 유지한다.
  Broker fixture는 provider command의 기존 test 경계에 추가하고 새 광범위 CI matrix를 만들지 않는다.

**의존성:** REL-01~03과 독립적으로 진행할 수 있다. Remote artifact collect의 blocking I/O는 별도 작업이다.
Catalog schema 전체 개편이나 built-in Team bridge 활성화는 이 PR 범위가 아니다.

## 6. 공식 문서에서 확인한 경계

- [Claude agent teams](https://code.claude.com/docs/en/agent-teams): in-process teammate는 resume으로
  복구되지 않고 task status가 늦을 수 있다. Transcript row만으로 실제 팀 복구·완료를 확정하지 않는 근거다.
- [Antigravity Swarm](https://antigravity.google/docs/swarm/): quota 소진 member는 일시 정지하며
  quota가 갱신되어도 저절로 재시작하지 않는다. Provider 상태와 OMS의 retry 정책을 별도로 정의해야 한다.
- [Codex App Server](https://learn.chatgpt.com/docs/app-server): response는 ID와 result/error로 식별하며
  설치 버전별 schema를 생성할 수 있다. Model/effort 목록은 client·account에 따라 달라진다.
  문서의 `-32001` ingress overload 설명은 WebSocket 경로다. 현재 stdio Broker에 그대로 일반화하지 않는다.

## 7. 실행 순서와 검증 범위

REL-01 → PERF-01, REL-02 → REL-03 cooldown 정책 순으로 진행한다. REL-04는 별도 코드 PR로 검토한다.
현재 문서는 source와 기존 fixture를 대조한 설계이며 Rust test·live native session 검증 결과가 아니다.
각 코드 PR은 구현한 한 경로, 재사용한 계약, 실제 실행한 fixture, 남은 provider 제한을 명시한다.
관측 fixture가 통과한 것을 native Team 생성·제어·acceptance 검증으로 보고하지 않는다.
