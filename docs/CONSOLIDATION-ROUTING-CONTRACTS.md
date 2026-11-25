# Routing 통합 계약: 항목 4–7

**상태: DESIGN / NOT IMPLEMENTED.** 이 문서는 구현 순서와 합격 조건을 정하는
독립 제안이다. 아래의 새 API 이름은 제안이며, 이 문서 추가로 runtime 동작은 바뀌지 않는다.
소스 기준은 `4c7db603ecf33952598406532c9213cf3aa18293` (2026-11-25)이다.
소스 링크의 줄 번호도 이 revision을 기준으로 한다. 원래 감사의 1–27 번호를 유지한다.

항목 2의 provider identity는 [#334](https://github.com/eightmm/oh-my-setting/pull/334),
항목 3의 account registry 검증은 [#335](https://github.com/eightmm/oh-my-setting/pull/335),
항목 24의 council 기본값은 [#318](https://github.com/eightmm/oh-my-setting/pull/318)의
기존 작업으로 이어간다. 열린 PR의 제안을 현재 main 구현으로 간주하지 않는다.
이 문서는 그 구현을 복제하거나 새 범용 router를 만들지 않는다.
기존 [CC 패킷](CALLABLE-CONSOLIDATION-WORK-PACKETS.md)과
[work queue의 통합 경계](ORCHESTRATOR-WORK-QUEUE.md#8-2026-11-22-재대조와-작은-통합-단위)를 구체화한다.

## 4. Provider/model/effort 입력 검증

**현재와 중복.** [Graph `build_invocation`](../crates/oms-command-graph/src/execution_runner.rs#L307)은
provider를 별도 40자 regex로, model을 별도 81자 regex로 검사한다.
[Peer `model_name`](../crates/oms-command-peer/src/providers.rs#L127)은 256자 상한과 다른
문자 규칙을 사용한다. 같은 값을 실행 경로에 따라 다르게 거절할 수 있다.
반면 [native model 계약](../crates/oms-command-agent-role/src/native_model.rs#L1)의
effort 정규화·지원 판단은 role/Panel/Peer가 이미 공유한다.

**선정 owner/API.** Provider 이름·alias·discovery는 #334 owner를 소비한다.
Model/effort의 순수 token 검증은 기존 `oms-command-agent-role::native_model`에
작은 함수로 모으고, 결과에 transport 지원과 catalog 관측을 섞지 않는다.
Graph가 Peer를 직접 의존하게 만들면 기존 Peer → Graph 의존과 순환하므로 금지한다.

**유지할 경계.** Token 수용은 model entitlement나 실제 served model의 증거가 아니다.
Graph는 concrete Plan task만 실행하고, frozen assignment와 issued lease의 검사는
Plan/Peer owner에 남는다. CLI별 길이 제한이 꼭 필요하면 adapter 제한으로 명시하고
공통 validator의 결과를 몰래 바꾸지 않는다. 기존 target 문법·argv 경계도 유지한다.

**구현 slice.**

1. `providers.rs::model_name`, `native_model`과 Graph regex의 차이를 기존 fixture에
   표로 고정한다. 공통 token 규칙과 의도적인 caller 제한을 먼저 결정한다.
2. `native_model`에 순수 validator를 추가하고 Peer의 기존 public 함수는 호환 wrapper로
   유지한다. #334의 provider 계약이 병합된 뒤 Graph의 provider 검증을 연결한다.
3. `execution_runner::build_invocation`의 model/effort regex를 연결하고 중복 regex만
   제거한다. `oms-command-graph/Cargo.toml`의 의존 방향을 함께 확인한다.

**테스트·합격 조건.** [Graph host adapter](../crates/oms-command-graph/tests/host_adapter.rs#L211)의
`dry_run_emits_native_plan_run_invocation_without_scripts`,
[Peer contract](../crates/oms-command-peer/tests/peer_contract.rs#L1382)의 native transport fixture,
[role native tests](../crates/oms-command-agent-role/tests/native.rs)를 확장한다.
Alias/custom provider, 선행 `-`, 공백·제어문자, model 길이 경계, unknown effort가
같은 corpus에서 설명 가능한 결과를 내야 한다. 잘못된 입력은 claim/provider 호출 전에
거절하며 dry-run argv는 frozen Plan 경로를 보존한다.

**의존성.** Provider 부분은 #334 이후. 계정 registry 검증은 #335를 재사용하고
선택·pinning·cooldown 정책은 각 실행 owner에 유지한다.
AGY exact selector 변경 [#523](https://github.com/eightmm/oh-my-setting/pull/523)과 겹치는
model 해석은 해당 head를 재대조한 뒤 진행한다.

## 5. Provider probe와 capability cache 갱신

**현재와 중복.** [models/model-doctor](../crates/oms-command-providers-skills/src/models.rs#L252)와
[doctor provider check](../crates/oms-command-lifecycle/src/provider_check.rs#L291)가
version/help → catalog → effort → cache 쓰기를 각각 수행한다. `model_probe::inspect`,
catalog parsing, cache 직렬화는 이미 공유한다. Lifecycle은
[binary identity 재검증](../crates/oms-command-lifecycle/src/provider_check.rs#L430)을 수행하지만
models는 [usable 분기 안에서 cache를 갱신](../crates/oms-command-providers-skills/src/models.rs#L263)한다.
공통 파서를 다시 만드는 작업이 아니라 남은 producer 흐름을 연결하는 작업이다.

**선정 owner/API.** 기존 `oms-command-providers-skills::model_probe`에
bounded probe 입력·관측 결과 계약을 두고, host 실행 adapter가 version/help/catalog를
수행하게 한다. 결과는 binary identity, 시각, 각 probe의 실행/실패/미실행,
catalog 보존 여부를 구분한다. Cache 저장도 같은 결과를 소비한다.

**유지할 경계.** Doctor의 health 집계와 models의 display/routability 출력은 유지한다.
`--live-models`/refresh opt-in, account scope, timeout/cancellation, 이전 catalog 보존,
unknown과 false의 차이를 보존한다. Exact-model flag 지원은 model 사용 권한의 증거가 아니다.
단순 상태 조회가 probe나 inference를 시작하게 만들지 않는다.

**구현 slice.**

1. 두 caller의 현재 cache fields·실패 처리를 비교하는 corpus를 기존 probe fixture에
   추가한다. Binary 변경과 version/help 부분 실패의 결과를 먼저 고정한다.
2. Version/help 및 cache identity 생성부터 공통 producer로 옮긴다. 각 caller는 기존
   command/timeout adapter를 전달하고 error/exit 표시를 유지한다.
3. Codex catalog와 AGY/OpenCode catalog 수집을 순서대로 연결한다. Broker transport
   이전을 기다리지 않고 현재 adapter를 사용하며, 두 번째 catalog 실행은 만들지 않는다.
4. `models.rs`와 `provider_check.rs`의 중복 갱신 분기를 제거한 뒤 기존 post-refresh
   consumer가 같은 증거를 사용하는지 확인한다.

**테스트·합격 조건.** [native lifecycle](../crates/oms-command-lifecycle/tests/native_lifecycle.rs#L2233)의
`doctor_and_model_doctor_share_bounded_probe_evidence`와
[models parity](../crates/oms-command-providers-skills/tests/project_parity.rs#L326)의
`models_preview_preserves_full_catalog_routability`를 확장한다.
같은 fake binary와 관측 입력이면 usability·binary identity·미실행/실패 의미가 일치하고,
변경된 binary는 이전 성공 근거를 차용하지 않아야 한다. 전체 catalog는 유지하며
display cap 때문에 routing 후보가 줄지 않아야 한다. 취소·timeout 후 재실행은 자동으로 하지 않는다.

**의존성.** #334의 binary discovery 계약을 우선 사용한다. 항목 10의 Broker 정리는
별도 transport 변경이며 이 producer 통합의 완료로 간주하지 않는다.

## 6. Scope overlap 판단

**현재와 중복.** [parallel Plan runner](../crates/oms-command-autonomy/src/parallel.rs#L33)는
glob을 parent root로 줄인 뒤 겹침을 계산하고,
[Graph scheduler](../crates/oms-command-graph/src/execution_scheduler.rs#L13)는 별도 glob/literal
규칙을 쓴다. [Room `overlaps`](../crates/oms-command-room/src/model.rs#L115)는 정규화된
literal scope의 prefix 관계를 다룬다. Panel의 Room 배정 경로는 이미 Room owner를 사용한다.
또한 [공통 `scope::scopes_overlap`](../crates/oms-contract-markdown/src/scope.rs#L973)이
존재하며, unknown/empty/invalid 입력을 모두 충돌로 취급한다.

**선정 owner/API.** 새 함수 대신 기존
`oms_contract_markdown::scope::scopes_overlap(Option<&[&str]>, Option<&[&str]>)`를 사용한다.
이 함수는 보수적 MAY-overlap이며 glob 언어의 정확한 교집합 계산기가 아니다.

**유지할 경계.** Read-only 작업과 writer slot, resource collision, Plan claim CAS,
Room scope 소유권은 각 scheduler/Room에 남는다. Unknown 배열에서 잘못된 원소를
버리고 빈 배열로 바꾸지 않고 `None`으로 전달한다. Room의 literal-only 입력 계약을
glob 허용으로 넓히지 않는다.

**구현 slice.**

1. Root/empty/invalid/glob corpus를 공통 scope tests에 고정하고 parallel·Graph의 기존
   결과 차이를 보수성 관점에서 검토한다.
2. `parallel.rs::overlaps`와 `execution_scheduler.rs::scopes_overlap`를 순서대로
   공통 함수의 adapter로 바꾼다. 각 caller의 JSON 형식 검증은 유지한다.
3. Room은 정규화된 literal 정책이 동등함을 확인한 뒤만 wrapper로 연결한다.
   승인되지 않은 scope를 늘리거나 Panel에 별도 overlap owner를 추가하지 않는다.

**테스트·합격 조건.** [scope contract](../crates/oms-contract-markdown/tests/scope_contract.rs#L1118)의
`reference_overlap`, [parallel run](../crates/oms-command-autonomy/tests/parallel_run.rs#L487)의
`overlapping_scopes_never_run_together`,
[Graph host adapter](../crates/oms-command-graph/tests/host_adapter.rs),
[Room contract](../crates/oms-command-room/tests/room_contract.rs)를 확장한다.
같거나 상하위인 경로·root glob·잘못된 선언은 동시에 writer로 선택되지 않아야 한다.
서로 다른 literal subtree는 기존 동시성을 유지하고, claim이 변하면 기존 CAS가 거절해야 한다.

**의존성.** 독립 구현 가능. Graph 파일은 항목 4의 writer와 순차 반영한다.

## 7. Provider capacity/fault와 adaptive failure 분류

**현재와 중복.** [Peer `capacity`/`provider_fault`](../crates/oms-command-peer/src/providers.rs#L2773)는
exit/complete와 출력 keyword로 provider 실패를 분류한다.
[Adaptive `classification`](../crates/oms-command-autonomy/src/adaptive.rs#L72)은
[ControlServices](../crates/oms/src/services_control.rs#L756)를 통해 기존 `oms-failure`를 소비한다.
Failure 분류를 하나 더 만들 필요는 없다. 다만 분류를 적용하는 fallback/recovery 결정은
서로 다른 권한 계약이다.

**선정 owner/API.** 기존 `oms-failure`의 순수 분류를 producer 사실
(exit, output completeness, timeout, provider 진단)로 호출하는 adapter를 Peer에 연결한다.
분류 결과와 `retry_allowed`는 분리한다. 재시도 허가는 각 실행 owner만 판단한다.

**유지할 경계.** [Peer fallback](../crates/oms-command-peer/src/providers.rs#L2744)의
`!plan_bound`, 다른 fallback model, clean write snapshot, account pin 조건을 유지한다.
종료·cleanup·request 전송 여부가 불확실한 호출을 다시 보내지 않는다. Adaptive의 frozen
authority, budget, repeated-failure 차단, exact operation/lease는 그대로 둔다.
진단 excerpt는 scrub한 bounded text이며 fingerprint는 실행 권한이 아니다.

**구현 slice.**

1. Peer keyword 분류와 `oms-failure` code/recovery의 대응 corpus를 작성한다.
   알려지지 않은 transport 결과는 unknown으로 남기고 의미 변경을 별도 표시한다.
2. Peer에 read-only 분류 adapter를 연결한다. 기존 fallback predicate는 그대로 두고
   분류 결과만 소비하도록 변경한다.
3. 동일한 실패가 Adaptive에 전달될 때 code/source digest가 유지되는지 확인한 뒤
   중복 keyword 목록을 제거한다. 재시도 orchestration이나 ledger는 합치지 않는다.

**테스트·합격 조건.** [failure contract](../crates/oms-failure/tests/failure_contract.rs),
[Peer contract](../crates/oms-command-peer/tests/peer_contract.rs),
[Adaptive feedback tests](../crates/oms-command-autonomy/tests/autonomy_contract.rs#L525)를 확장한다.
Quota/429/auth/ordinary task failure/timeout/partial output을 구분하고, Plan-bound 또는
uncertain 결과에서 provider 호출 횟수가 늘지 않아야 한다. Frozen task·account·operation이
다르면 재시도하지 않고, clean unbound retry의 기존 동작만 유지한다.

**의존성.** #334와 Peer writer를 조정한다. 분류 통합은 항목 27의 표시 통합과 별도로
검증하며, 표시된 recovery 제안만으로 실행을 허가하지 않는다.

## 검증과 완료 판정

이 제안 PR의 검증은 상대 링크·번호·소스 근거 확인이다. 위 runtime 합격 조건은 후속
구현 PR에서 실행할 조건이며 여기서 통과했다고 주장하지 않는다. 각 구현 slice는 해당
기존 fixture를 먼저 확장하고, caller별 adapter가 실제 연결된 뒤 중복 분기를 제거한다.
최종 변경의 root `./verify`와 CI 결과는 구현 PR의 정확한 head에 따로 기록한다.
