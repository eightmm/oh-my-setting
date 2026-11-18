# Team runtime: ownership, admission, execution and evidence

작성: 2026-11-17. **DESIGN ONLY**. 기준 소스:
`32923cd388e581a6d6d24525b200fca746cda4d0`.
상위 설계: [architecture PR #245](https://github.com/eightmm/oh-my-setting/pull/245).
이 문서는 TM-01~TM-05의 구현 패킷이다. 타입/동작/상태 이름은 제안이며 현재 CLI가
아니다. 기존 Rust parent runtime, room, plan, supervisor, usage, admission writer를
확장한다. 별도 scheduler/DB/daemon, 직접 provider CLI 실행, Python runtime은 추가하지 않는다.

## 1. 보존해야 할 현재 경계

[safety.rs](../crates/oms-command-peer/src/safety.rs)의 `parent()`는 harness child의
peer 호출과 owner mutation을 거절한다. [dispatch.rs](../crates/oms-panel-core/src/dispatch.rs)는
scope, main/attempt 관계, carried patch와 WIP를 다룬다.
[room](../crates/oms-command-room/README.md)은 메시지와 소비를 기록하지만 승인 권한을
만들지 않는다. [supervisor](../crates/oms-supervisor/README.md)가 process 생명주기와
cleanup 증거를 소유하고 [panel core](../crates/oms-panel-core/README.md)가 결과·admission을 연결한다.

Team Lead에게 기존 mutation을 허용하는 대신 **요청과 실행을 분리**한다.
Lead는 계획/질문/후속 요청을 만들고 부모가 재검증하여 기존 실행기로 전달한다.
부모가 정한 범위 안의 deterministic dispatch마다 LLM Main의 새 응답을 요구하지
않는다. 범위 확대나 충돌 판단만 Main에 올린다.

## 2. TM-01 — 계약, identity와 canonical state

### 최소 계약

| 구조 | 내용 |
|---|---|
| TeamSpec | schema/version, immutable ID/generation, root owner generation, task/plan generation, objective/criterion digest, base identity, mode, member bounds, scope/resource/policy/budget refs, expiry |
| MemberBinding | logical lead와 root owner를 각각 기록, role/access, existing attempt/room/worktree refs, provider/transport/account/host namespace, native identity provenance |
| TeamRequest | stable request ID, request digest, authenticated sender binding, expected team/task/policy generation, action/target, bounded payload or artifact ref, expiry |
| TeamResult | base/tree/patch digest, component provenance, verification/review refs, unresolved findings, usage/cleanup/observation coverage |

부모가 작성한 immutable TeamSpec artifact를 기존 canonical plan/operation record가
참조한다. 활성화/세대 변경/취소 결정의 유일한 writer는 기존 부모 경로다.
UI를 위해 별도 mutable team 상태 DB를 만들지 않는다. Snapshot/cache는 지워도
재구성 가능한 projection이며 권한 근거로 읽지 않는다.

Identity는 표시 이름, cwd, PID, 최근 파일 mtime으로 추론하지 않는다. 동일 task title,
재사용 PID, 같은 native thread ID라도 repo/account/host/connection generation이 다르면
다른 실행이다. Logical parent는 트리 표현용이고 root owner는 권한·자원 집계용이다.
Legacy `parent_attempt_id`의 의미를 조용히 바꾸지 않는다. 필요하면 additive typed
lineage를 도입하고 direct-parent 기반 기존 caller/test를 함께 점검한다.

팀 계약 변경은 새 generation이다. 범위/criterion/base/정책 변경 시 영향 member를
held/stale로 투영하고 재승인·재검증 없이 이전 evidence를 이월하지 않는다.
Legacy record는 읽히되 없는 team/coverage를 null로 표현한다. 알 수 없는 schema,
cycle, 복수 owner, 깨진 predecessor는 실행을 거절한다.

### Acceptance

기존 canonical fixture에 create/read/restart, old/new version, stale spec, owner swap,
동명이인 member, cross-account ID collision, root-owner cycle을 추가한다. 이 단계는
계약·projection만 제공하고 provider를 시작하지 않는다.

### Native contract work in progress

`agent-plan team-register --team-spec REPO_REL` captures a schema-1
`oms-team-spec` into private content-addressed `.oms/plan/team-specs/` storage.
The existing canonical plan stores its references and phase; no second team
database is introduced. `team-show --id ID` and `team-list` read this state.
`team-cancel --id ID --expected-team-sha256 SHA --reason TEXT` withdraws a
planned, unlaunched contract. These entrypoints do not launch providers.

Registration requires the enrolled Main and room, its actual creation event
as `root_owner_generation`, the current plan/task/criterion/base and policy
digests, normalized scope within the parent task, and a future expiry. Each
member has a separate identity, logical parent, root owner and route snapshot.
Display names may repeat. Planned routes retain absent resolved/served model,
applied effort, native execution and usage evidence as null.

Scope inclusion compares complete pattern languages and literal descendants;
matching only the declaration text is insufficient. An empty allow list keeps
the existing unrestricted meaning. Inclusion work/state limits refuse an
unproven scope rather than granting it.

The first generation has an empty `previous_sha256`. A later generation must
name the preceding immutable artifact digest and supply the same digest as
`--expected-team-sha256`; it starts held for new admission. Historical artifacts
remain readable. Changed bindings project held without modifying stored state;
settled means withdrawn execution, without verified/admitted/landed/published
evidence. Readers reject unknown schema and broken artifact/predecessor chains.

Each command validates every retained artifact once and reuses its lifecycle,
room, policy and HEAD observations across the teams in that command. The next
command reads current producer bytes again; this cache grants no persistent
authority. Canonical fixtures count shared producer projections and check that
listing teams leaves the plan unchanged.

Parser bounds are 64 KiB per contract, 128 members per contract, and 256
retained contract generations across one plan. These are storage bounds, not
default team/concurrency budgets. Registration does not grant TM-02 admission,
TM-03 lead-request authority, TM-04 execution, or TM-05 integration. Native
enrollment fixtures cover the actual Main/room producers, owner termination
and artifact tampering. The remaining TM-01/MR-A fault matrix still needs
matching evidence before declaring TM-01 complete.

The native Plan API now exposes a contract-only `team_admission_before` guard.
It reuses the current generation/owner/task/policy checks under the existing
writer order, shares a deadline with native lifecycle and Git proofs, and
revalidates transient native group evidence before returning. Cancellation with
an unchanged spec digest and expiry during owner projection refuse. This API
does not itself reserve resources, enroll an executing member or grant launch
authority; the shared physical consumer still needs those integrations.

## 3. TM-02 — 하나의 admission과 명시적인 자원 범위

### 자원 종류를 분리한다

| 예산/한도 | 집계와 해제 조건 |
|---|---|
| Team 수 | 같은 root owner/goal의 활성 team 계약. Member 수와 별도 |
| Running invocation 수 | lead, worker, verifier, review, repair 등 실행 중 호출 전부. Native 후손도 검증된 upper bound 또는 실제 예약으로 포함 |
| Open session/process 수 | 대기 중 세션/프로세스의 메모리·FD도 제한. 추론 idle과 구분 |
| Write WIP | 실행 중 writer + 후보 검토/통합 대기. Team 하나를 writer 하나로 축소하지 않음 |
| Provider/account capacity | 정확한 account/host namespace, 기존 permit/reservation. 다른 repo/host와 공유하지 않는 cap은 해당 scope라고 표시 |
| Calls/elapsed/retries | 기존 실행 예산. 메시지 wakeup과 물리 fallback도 포함 |
| Token/cost admission | #240 receipt와 #241 reservation의 measured/active/unresolved 투영. 실제 provider-side hard cap은 아님 |

Global은 현재 지정한 admission domain에서의 전체를 뜻한다. 여러 프로젝트가 같은
account를 쓰면 같은 예약 producer에 참여해야 hard global concurrency라고 부를 수 있다.
독립 호스트의 동기화가 없으면 host-local로 표시하고 strict cross-host bound는 거절한다.
새 분산 scheduler나 조용한 전역 파일 lock을 이 PR의 부수 기능으로 만들지 않는다.

### Atomic reservation과 launch intent

기존 writer/lock 순서를 조사한 뒤 하나의 검토된 순서로 lease, resource, call budget을
검사·예약한다. 새 팀 경로와 기존 single/delegate 경로가 같은 occupancy에 참여해야
팀이 기존 worker를 무시하고 cap을 초과하지 않는다. 단순 snapshot check를 atomic
reservation이라고 표시하지 않는다. 서로 다른 저장소 writer에 걸친 단계는 하나의
ACID transaction인 척하지 않고 intent/CAS/보상/복구로 명시한다.

1. Parent는 request/spec/plan/owner와 dependency readiness를 현재 generation으로 검증한다.
2. 고정 lock 순서와 bounded acquisition으로 자원을 예약한다. 일부 자원을 잡은 채
   무기한 기다리지 않는다. Reviewer/cleanup에 필요한 capacity를 남기고 writer backlog에
   backpressure를 적용한다. 기존 continuation 예외도 총 실행 예산을 우회하지 않는다.
3. Process 생성이나 provider 전송 전에 durable launch intent와 usage liability를 기록한다.
4. 기존 supervisor/peer 경로로 정확한 argv/cwd/env/access를 실행한다. 설정이 허용하는
   범위 안에서만 native subagent 생성을 끄거나 제한한다. Prompt-only 제한은 enforcement가 아니다.
5. 확인된 결과·usage revision·cleanup 증거에 따라 예약을 정산한다. 확실한 pre-launch
   거절은 반환할 수 있지만 spawn/send 불명은 reservation/liability를 보존한다.

응답 손실 후 같은 요청을 다시 받으면 기존 intent/receipt를 조회한다. 같은 ID와 같은
payload는 같은 결과를 돌려주며, 같은 ID와 다른 payload는 거절한다. Launch가 일어났는지
모르면 자동 재실행하지 않는다. External effect의 exactly-once 보장은 주장하지 않는다.

### 비용와 native 후손

[#240](https://github.com/eightmm/oh-my-setting/pull/240)의 operation/call/revision을
재사용하고 [#241](https://github.com/eightmm/oh-my-setting/pull/241)의 soft admission을
공통 consumer 경계로 연결한다. 가격표·구독 quota 비율을 추정해서 채우지 않는다.
Parent cumulative total이 descendants를 포함하면 descendant total을 다시 더하지 않는다.
Scope가 불명확하면 총량을 unknown/partial로 남기며 알려진 부분과 불명 liability를 같이 표시한다.

취소·timeout·모델 전환·account retry가 과거 spend를 지우지 않는다. Native team이 내부
spawn 수를 제어할 수 없으면 알려진 worst-case를 예약하거나 strict bounded mode에서
거절한다. 평균 관측치나 관측되지 않은 자식 수 0으로 hard cap을 계산하지 않는다.

### Acceptance

동시 team+legacy dispatch, 경계값, double reservation, owner swap, delayed receipt,
late downward correction, inclusive parent usage, missing usage, lead/reviewer 비용,
unknown launch, registry full/disk failure를 결정적 fixture로 시험한다. Assertions는
자원 cap, 기존 state 보존, 중복 부작용 없음, 불명 책임 보존이다. 유료 호출은 없다.

## 4. TM-03 — 부모가 소비하는 제한된 TeamRequest

### 허용 요청

Lead는 배정된 task의 분해 제안, 이미 검토된 leaf dispatch, 같은 범위의 continuation,
대상 member에 대한 메시지, blocker 제출, 자기 팀 취소 요청, 결과 제출을 할 수 있다.
권한·범위·모델/계정·예산·verifier 변경은 제안만 가능하고 부모 결정이 필요하다.
Worker가 lead 역할을 사칭하거나 worker를 또 생성하는 요청은 거절한다.

제안 request 종류는 `propose_tasks`, `request_dispatch`, `request_continue`,
`request_message`, `report_blocker`, `request_cancel`, `submit_result`다. 새 공개 CLI
철자는 구현 시 기존 panel/MCP front door와 중복 여부를 확인한 뒤 확정한다.
이 목록을 raw shell 실행 API로 구현하지 않는다.

### 인증·검증

Room에 적힌 actor/role, JSON의 team ID, 환경변수만으로 요청을 인증하지 않는다.
부모가 관리하는 dispatch enrollment와 private operation binding에 요청을 묶고,
정확한 member generation·allowed action·target·scope·budget·expiry를 검증한다.
기존 child guard를 해제하지 않는다. 일반 room 메시지는 계속 untrusted data다.
프로세스가 host와 동일 UID/credential을 가진 trusted-local 실행인 경우 그 한계를
명시한다. 적대적 worker에 대한 보장은 별도 검증된 OS isolation 없이는 주장하지 않는다.

Request가 bounded artifact를 가리킬 때 경로·소유자·정규 파일·크기·digest를 확인한다.
Prompt injection으로 메시지 본문에 적힌 command/approval을 실행하지 않는다.
Decision은 기존 canonical writer가 남기며 UI event나 Lead의 서술은 receipt가 아니다.

### 메시징과 컨텍스트

[Room의 기존 4,000-byte 메시지·cursor·ack 경계](../crates/oms-command-room/src/cli.rs)를
재사용한다. 큰 brief/diff는 기존 artifact reference로 전달한다. All-to-all 전체 transcript
복제를 하지 않고 target/team 간 필요한 contract/evidence만 공유한다. Cross-team 요청은
부모가 중계하거나 정확한 opt-in audience로 제한한다.

전달 제출, transport 수락, 모델 입력 포함, 소비 ack, task 완료를 다른 상태로 기록한다.
Replay가 새 wakeup을 만들지 않도록 request/message identity를 결합한다. 통신 loop에는
fan-out, message bytes, 모델 wakeup 횟수와 discussion rounds 상한을 둔다. 대기 중
모델을 계속 polling하여 깨우지 않는다. Query/ack 자체는 inference 권한이 아니다.

Lead가 await하는 동안 실행 permit을 놓을 수 있는지는 실제 idle/turn-terminal 증거로
판정한다. Quiescent lead session을 유지해도 session cap은 남는다. 증거가 없으면
running slot을 유지하고 다음 작업을 보류한다. 단순 quiet를 무료 idle로 가정하지 않는다.

### Acceptance

Forged sender, cross-team target, expired scope, duplicate/reordered message, room rollover,
ack-before-consume, lost wakeup receipt, message amplification, worker recursive request를
시험한다. Parent offline이면 요청이 남거나 안전하게 거절되어야 하며 child가 fallback
controller가 되면 안 된다. Main 재시작 후 미확정 wakeup을 자동 재전송하지 않는다.

### TM-03A native report channel: bounded implementation

An explicit `peer-ask --team-requests --providers ADAPTER` uses the existing
four Team locator flags and `--task-id`. Its default supports only an actual
read-only lead and `report_blocker`. Registered custom adapters require a strict schema-1
`ADAPTER.oms-team-request-v1.json` declaration with `protocol` equal to
`oms-team-request-v1` and the observed `binary_sha256`. The declaration and
binary are owned, no-follow, single-link regular files, bounded at 8 KiB and
16 MiB respectively, and checked again before launch. These are selection
observations, not an immutable executing-image pin or live vendor capability.
Codex, Claude and Antigravity built-ins refuse this protocol opt-in. The normal
single-call path keeps its existing behavior.

A borrowed opaque launch binding comes only from the existing physical New
producer. The actual native lifecycle attempt, Room enrollment, current Team
contract and owned Supervisor duplex child are required before Root issues a
private session. Controller image/native process generation, connection and
private operation file are retained. A public locator, PhysicalCall, environment
marker or copied JSON cannot create that session. The process is trusted-local;
this does not isolate hostile processes with the same UID.

The parent sends a hello containing schema/kind, session, random nonce,
connection_generation, binding_sha256 and expires_at. The actor echoes the
hello identity, then sends request frames with those identity fields, sequence,
binding_sha256 and body. Body is exactly schema 1, request_id, action
`report_blocker`, text and expires_at. The existing private `--prompt-file`
continues to carry the caller's assignment; a channel handshake is not evidence
that a model consumed it. Responses contain an unpredictable receipt_challenge;
ACK echoes it along with exact request_id/sequence and channel identity. Queue
admission is not consumption. Raw protocol captures and challenge/nonce bytes
are withheld from outward artifacts; private metadata retains only nonce hash.

Frames/partial frames are capped at 8 KiB, text at 4,000 bytes, input at four
frames/32 KiB, output queue at eight events/64 KiB, capture per stream at 512 KiB
and requests at 32 per connection. Deadline/cancel/reap use the same Reactor.
Session/request expiry cannot exceed the selected session/Team budget. Known
cleanup is separate from protocol completeness and unknown usage.

The canonical Team record opts into request_schema 2 only when recording its
first request. Legacy `{versions,phase,reason}` remains readable; older readers
refuse the extra fields. Request receipts have Plan-wide bounds of 64 and
512 KiB including final array framing. Same semantic ID/body returns the stored
outcome; changed body conflicts. Generation changes/cancellation retain dedup
history. Post-CAS guard failure reports possible publication and retains the
receipt for reconciliation. Recording a blocker changes no task state, scope,
route, budget, acceptance or publication authority. Other request actions remain
unsupported; this is not completion of TM-03/TM-04 or provider-native control.

The canonical native Root fixture exercises public opt-in, actual local adapter,
private issuance, report CAS, response ACK, replay/conflict, lost ACK, forged
channel data, early ACK, actual deadline/cancel and exact enrollment cleanup.
Plan fixtures reject altered receipt keys and an exact 512-KiB comma overflow.
Only sealed owned cleanup for the exact call/scope allows checked enrollment
leave; missing or stale authority preserves the enrollment for reconciliation.
No paid model or live provider capability is claimed by these fixtures.

### TM-03B scoped task proposals: explicit opt-in

The same native channel additionally accepts `propose_tasks` only when its parent
passes `--team-request-actions propose_tasks` or an explicit comma list including
`report_blocker`. The default remains report-only. The typed selection is retained
by the opaque New launch binding and private session metadata; it is policy data,
not a controller credential. A non-default hello lists `allowed_actions` and the
guard-checked `task_id`/`task_sha256`; the actor obtains its exact parent-task
digest from that owned channel rather than guessing it from local metadata. The
existing custom-adapter declaration describes the wire transport; no built-in
provider capability, model call or new transport is inferred from this action.

A proposal body has schema 2, request_id, action `propose_tasks`, summary,
expected_task_sha256, tasks and expires_at. It contains 1..8 proposed tasks, each
exactly id, title, allowed and depends. Summary is at most 1,000 UTF-8 bytes, title
300 bytes, and the existing 8-KiB body/frame limits still apply. IDs and dependency
edges are unique; dependencies name only other proposed tasks and must be acyclic.
Each allowed list contains 1..32 normalized unique paths of at most 240 bytes.
The current parent-task digest must match, IDs must be absent from the canonical
task graph, and scope must be provably within the Team's allowed envelope and
disjoint from forbidden scope. Unproven overlap refuses the proposal.

The canonical Team request record opts into version 3 upon its first proposal.
Version 2 remains report-only; older readers refuse version 3. The existing 64-row /
512-KiB Plan-wide history cap, stable semantic identity, changed-body conflict,
generation/cancel retention, challenged ACK and post-CAS reconciliation apply.
An exact replay returns the recorded proposal outcome rather than reapplying new
task eligibility checks or creating another effect. `agent-plan team-show --id ID
--requests` explicitly reads the bounded receipt history; default Team views keep
their existing fields. A missing request ledger is shown as null by this opt-in
reader, and reading it does not write state or call a provider.

The response status `proposed` means only that parent-review data was recorded.
It does not add tasks, change their state/scope/criterion/verifier, select a route,
dispatch, wake a model, cancel a member or accept a result. Applying any reviewed
decomposition remains a separate existing parent operation with its own digest
CAS and acceptance rules. Other TeamRequest actions and full TM-03 completion
remain unsupported. Local fixtures cover proposal/replay/lost ACK, explicit
action refusal, stale digest, scope/forbidden boundaries and cyclic/existing IDs;
they are not live-provider support evidence.

## 5. TM-04 — OMS-managed 실행과 복구

첫 E2E는 동일한 fake fixture에서 Codex 역할과 Claude 역할의 read-only workers를
기존 peer transport로 실행한다. 실제 모델은 사용하지 않는다. Native fan-out을 통제할
수 없는 transport는 strict bounded team route로 선택하지 않는다. 기존 provider의
read-only/plan 설정이 OS write 차단을 보장하는지와 단순 instruction인지 구분한다.

각 write worker는 기존 격리 worktree와 scoped brief를 받는다. Team 전체가 하나의
writable checkout을 공유하는 모드를 기본으로 제공하지 않는다. Build output, ports,
Cargo target slots, test DB 등 경로 밖 공유 자원도 resource claim에 넣는다. 같은 파일의
경쟁 후보는 독립 snapshot에서만 허용하며 합치는 결정은 별도다.

Parent runtime은 dependency-ready member를 시작한다. 전체 wave의 가장 느린 member를
기다리는 global barrier는 만들지 않는다. 다만 canonical plan의 기존 scheduling 계약을
몰래 바꾸지 말고 caller 통합과 회귀를 같은 구현 PR에 담는다. Unrelated failure는 다른
team의 후보를 폐기하지 않으며 required dependency 실패는 해당 후속 작업만 막는다.

| 사건 | 필수 복구 |
|---|---|
| Main crash / replacement | Canonical owner generation 재검증. Existing intent/attempt를 연결하고 권한이 불명인 새 dispatch 보류 |
| Lead crash | Lead의 논리적 실패와 sibling process 생존 분리. Dirty work/receipts 보존; 명시된 recovery policy 없이는 새 lead 자동 생성 금지 |
| Member timeout / quota exhausted | Unknown usage와 partial patch 보존. Fresh admission 없이 fallback/재시도 금지 |
| Cancel request | 새 launch 먼저 막고 exact owned descendants에 기존 cancellation 전달; requested와 confirmed cleanup 분리 |
| Native child identity 누락 | `unknown` coverage. 조용하다는 이유로 종료 또는 budget 반환 금지 |
| PID reuse / host loss | Generation/boot/ownership 증거 없는 kill, lease reclaim, cleanup 금지 |
| Disk full / partial append | 기존 writer의 불명 상태 보존. 두 번째 장부나 성공 응답으로 우회 금지 |

`hold`는 다음 admission 차단이며 실행 중인 agent의 OS pause가 아니다. `cancel`은
협력적 중단·grace·supervisor escalation을 따르고 실제 종료 증거가 있어야 settled다.
팀 parent가 완료를 보고해도 native children cleanup이 불명이면 완료로 덮지 않는다.

## 6. TM-05 — 후보, 독립 검증, 통합

MVP에서 Lead는 read-only coordinator로 둔다. 코드 변경이 필요하면 동일한 admission을
받는 별도 writer 역할/attempt로 실행한다. Coordinator가 몰래 integration checkout을
수정하고 자신의 patch를 검증하는 우회로를 만들지 않는다.

각 member 결과는 immutable patch/tree artifact, 고정 base, task/criterion digest,
실행·test·review refs, 실패와 생략 범위를 제출한다. 그룹 result는 그 참조를 묶는
manifest이며 자연어 요약만 제출하면 missing evidence다. Required member 실패는
partial result로 남긴다. Partial success를 허용하는지 여부는 사전에 검토된 task contract가 결정한다.

서로 다른 후보를 합칠 때 기존 부모 integration 경로에서 disposable integration
worktree를 사용한다. 정해진 순서로 exact patches를 적용하고 공유 파일/API 충돌은
명시적으로 해결한다. 구성원별 테스트 통과를 합친 tree의 테스트 통과로 재사용하지 않는다.
조합된 exact tree에 고정 verifier와 독립 review를 다시 수행한 뒤 기존
`patch-admit`/`patch-land`를 호출한다.

Reviewer는 작성자로부터 분리된 attempt/context를 사용한다. Policy가 cross-family
review를 요구하면 실제 served model family 증거로 판정한다. CLI/provider 이름만
다르거나 모델이 unreported이면 독립성 충족으로 표시하지 않는다. Native verifier
sign-off는 보조 evidence이지 OMS acceptance를 대체하지 않는다.

| 변경/실패 | 금지되는 처리 |
|---|---|
| Review 후 candidate bytes/base/verifier 변경 | 옛 green을 현재 결과에 사용 |
| 통합 도중 process crash | Patch 무조건 재적용 또는 중간 worktree 삭제 |
| 일부 member 결과 유실 | Lead summary로 missing evidence 보충 |
| Remote main 이동 | Base/tree를 재검증하지 않은 자동 push |
| Writer가 verifier/policy 수정 | 자기 완화된 기준으로 성공 선언 |
| 사용자 publication 승인 없음 | 검증 통과를 commit/push/merge 권한으로 전환 |

복구는 기존 acceptance intent, 실제 index/tree/HEAD, patch digest와 publication receipt를
대조한다. 이미 적용된 것을 다시 적용하지 않는다. Shared integration ref의 writer는
한 명이며 concurrent team acceptance도 같은 기존 직렬화 경로에 참여한다.

## 7. 구현 범위, 시험과 제출

| PR | 주 검토 경계 | 반드시 같이 넣을 시험 |
|---|---|---|
| TM-01 | 기존 plan/operations identity와 panel projection | schema/legacy/generation/cycle |
| TM-02 | dispatch, supervisor admission, telemetry, #241 budget consumer | concurrent reservation, unknown liability, root-owner accounting |
| TM-03 | room/request producer, parent consumer, context selection | spoof/replay/expiry/wakeup/message loops |
| TM-04 | peer executor와 native supervisor, root service wiring | mixed-provider fake E2E, crash/cancel/reconcile, single 회귀 |
| TM-05 | 기존 result/admission/landing 및 canonical tests | exact integration tree, stale verifier, conflicting patches, interrupted acceptance |

구체적 신규 파일명이나 public schema를 이 문서에서 확정하지 않는다. 최신 source의
최소한의 기존 crate 경계를 재사용하며 같은 공통 파일은 순차 병합한다. 각 PR은 당시
`AGENTS.md`가 요구하는 affected/critical 검사를 사용하고 public entrypoint가 연결된
fixture를 포함한다. 전체 test-only 후속 PR에 correctness를 미루지 않는다.

본 문서가 정의한 acceptance는 모두 미래 구현 조건이다. 이번 문서 작성에서 실제
native team, 모델 사용량, OS enforcement 또는 성능 개선을 검증했다는 뜻은 아니다.
