# 기능 통합 27개 항목의 PR 지도

작성일: 2026-11-25. 기준 source는 main
[`4c7db603ecf33952598406532c9213cf3aa18293`](https://github.com/eightmm/oh-my-setting/commit/4c7db603ecf33952598406532c9213cf3aa18293)이다.
이 문서는 기능 통합 조사에서 나온 **1–27번의 소유자, PR 범위와 다음 검증**을 연결한다.
과거 CC/AH/LC 번호를 재정의하지 않으며, 구현 상태는 아래 PR의 정확한 revision에서 확인한다.

이번 묶음은 **새 구현 Draft 3개, 새 명세 PR 4개, 이 추적 문서 PR 1개,
기존 관련 PR 4개**로 구성된다. 각 항목에 구현 또는 구체적인 후속 명세의 소유자를 배정한
것이다. 명세 PR의 게시·병합은 해당 runtime 기능의 구현 완료를 뜻하지 않는다.

## 읽는 기준

| 유형 | 이 문서에서의 의미 | 다음 완료 증거 |
|---|---|---|
| 구현 Draft | 코드와 기존 테스트 변경이 있는 검토 대상 | 해당 PR HEAD의 필요한 CI·계약 검증 및 별도 병합 결정 |
| 명세 | 재사용할 producer, adapter 경계와 검증 조건을 정한 문서 | 후속 구현 PR과 실제 검증 결과 |
| 기존 PR | 이미 같은 문제를 소유하는 작업에 연결 | 기존 diff·stack base·현재 CI를 재대조하고 필요한 delta만 추가 |

한 항목의 주 소유자는 하나다. 24번은 기존 #318이 기본값 구현을 소유하고, surface 명세는
명령 선택 안내를 보완한다. 이를 별도 council 실행기 두 개의 구현으로 집계하지 않는다.
이 추적 문서 자체에는 runtime 변경이 없다.

## 1–7. 판정과 routing 계약

| 번호 | 확인한 통합 대상 | 유형 | Canonical owner / 재사용 대상 | 연결 PR | 의존성과 다음 검증 |
|---|---|---|---|---|---|
| 1 | artifact-resolution 판정의 공통 replay | 구현 Draft | `oms-command-telemetry::store::resolution`; hook·state·report·runtime adapter | [#529](https://github.com/eightmm/oh-my-setting/pull/529) | 중복 ID, 선행 순서, lineage, source, invalid count; telemetry legacy 의미, hook의 강화된 거부와 runtime 입력 순서 계약을 구분 |
| 2 | provider identity / discovery | 기존 PR | #334의 공유 provider producer; peer와 providers-skills consumer | [#334](https://github.com/eightmm/oh-my-setting/pull/334), Draft | alias·discovery 순서·provider family 의미와 caller별 실패 처리를 보존 |
| 3 | account registry 검증 | 기존 PR | #335의 account registry validator | [#335](https://github.com/eightmm/oh-my-setting/pull/335), Draft | **#334 위의 stack**; decode·검증 공유와 계정 선택·credential custody를 분리 |
| 4 | provider / model / effort 입력 검증 | 명세 | model/effort: 기존 `oms-command-agent-role::native_model`; provider: #334와 peer `Target` adapter | [#530](https://github.com/eightmm/oh-my-setting/pull/530) | #334/#335와 중복 범위를 먼저 대조; backend 선택 owner를 유지하고 Graph·Panel 입력·오류 계약 검증 |
| 5 | model-doctor / doctor의 probe·cache 조정 | 명세 | `oms-command-providers-skills::model_probe`와 기존 probe producer | [#530](https://github.com/eightmm/oh-my-setting/pull/530) | 이미 공유하는 probe를 유지; cache key·freshness·force refresh·timeout·credential 경계 검증 |
| 6 | scope overlap의 소비자별 재구현 | 명세 | **기존 `oms-contract-markdown::scope::scopes_overlap`** | [#530](https://github.com/eightmm/oh-my-setting/pull/530) | 새 overlap API를 만들지 않고 기존 함수 적용성 검토; empty/root/glob/invalid 입력과 scheduler별 보수성 검증 |
| 7 | provider 실패 관측 분류 | 명세 | `oms-failure` taxonomy와 peer transport 관측 adapter | [#530](https://github.com/eightmm/oh-my-setting/pull/530) | capacity/auth/incomplete 분류 공유; Plan-bound fallback 금지, 미확정 실행의 재전송 금지와 budget 결정은 각 owner 유지 |

## 8–12. 실행과 소유권

| 번호 | 확인한 통합 대상 | 유형 | Canonical owner / 재사용 대상 | 연결 PR | 의존성과 다음 검증 |
|---|---|---|---|---|---|
| 8 | Goal / Panel의 정확한 tree commit 처리 | 명세 | `autonomy::goal`와 `panel-core::acceptance`가 공유할 좁은 Git primitive | [#531](https://github.com/eightmm/oh-my-setting/pull/531) | index identity·parent/tree·ref CAS·미확정 publication 검증; durable intent와 Plan 완료 순서는 각 domain 유지 |
| 9 | 남은 bounded process 수명 관리 | 명세 | 기존 `oms-supervisor::Reactor`; graph/task/runs adapter | [#531](https://github.com/eightmm/oh-my-setting/pull/531) | Plan Git의 이전은 이미 반영됨; signal·stdin·live output·kill/reap/drain·출력 해시 계약별 migration 검증 |
| 10 | Broker / recipe companion 수명 관리 | 명세 | 기존 supervisor의 duplex / owned-process 계약 | [#531](https://github.com/eightmm/oh-my-setting/pull/531) | 9번의 공통 기초와 연결; Broker framing·backpressure·scrubbing 및 companion 종료 정책 유지 |
| 11 | Task / Room의 lock 취소 전달 watcher | 구현 Draft | `oms-lock::Lock::acquire_with_cancel` | [#527](https://github.com/eightmm/oh-my-setting/pull/527) | owned `Cancellation` API 유지; 획득 전·contention 중 취소, live owner, deadline와 caller signal exit 검증 |
| 12 | 같은 Plan task를 시작하는 Panel / Lead / CLI 진입점 | 기존 PR | `oms-command-autonomy::runner`와 Panel의 exact task binding adapter | [#522](https://github.com/eightmm/oh-my-setting/pull/522), Draft | pinned task snapshot·Claim CAS·ready-only·provider 호출 전 검사; native assignment와 managed worker custody 유지 |

## 13–20. Context, 관측과 기록

| 번호 | 확인한 통합 대상 | 유형 | Canonical owner / 재사용 대상 | 연결 PR | 의존성과 다음 검증 |
|---|---|---|---|---|---|
| 13 | Graph / peer의 context pack 검증 | 명세 | `oms-context` manifest 계약과 기존 Graph / peer adapter | [#532](https://github.com/eightmm/oh-my-setting/pull/532) | schema·digest·경로·크기 한도를 보존; pack 유효성과 실제 요청 전달 증거를 구분 |
| 14 | CLI / runtime의 current-source 집계 | 명세 | `oms-context-source`의 관측과 공유할 집계 함수 | [#532](https://github.com/eightmm/oh-my-setting/pull/532) | unchanged/changed/missing/unreadable 판정, 오류 전파와 bounded 출력 검증 |
| 15 | AGY hook / session handoff 관측 | 명세 | `oms-context-source`의 AGY history acquisition; hook / peer rendering adapter | [#532](https://github.com/eightmm/oh-my-setting/pull/532) | session identity·transcript 출처·재사용/소비 증거를 보존; handoff를 실행·수락 권한으로 승격하지 않음 |
| 16 | Slurm 관측 정규화 | 명세 | 기존 `oms-command-runs::reconcile`의 순수 scheduler-state 정규화; submit adapter | [#532](https://github.com/eightmm/oh-my-setting/pull/532) | raw scheduler 상태·exit·불확실성 정규화; **submit 성공과 run 성공의 정책은 별도 유지** |
| 17 | Runtime / Runs의 동일한 수치 통계 구현 | 구현 Draft | `oms-command-runs::statistics::{mean, stdev}` | [#528](https://github.com/eightmm/oh-my-setting/pull/528) | 두 visibility 선언 외 6,361 bytes 동일 확인; 기존 Runs와 runtime experiment 테스트로 연결 검증 |
| 18 | 사용처 없는 state job_metrics와 job-digest의 중복 | 명세 | `oms-command-job-digest` producer | [#532](https://github.com/eightmm/oh-my-setting/pull/532) | `state::job_metrics`의 공개 API·실제 caller 재확인 후 제거/재사용 결정; 데이터 읽기·표시 fixture 보존 |
| 19 | retention의 reference inventory | 명세 | telemetry retention 및 기존 domain reference reader | [#532](https://github.com/eightmm/oh-my-setting/pull/532) | 참조 종류·missing/unknown·snapshot freshness를 공통 관측; 삭제·보존 권한과 lock은 각 domain 유지 |
| 20 | 테스트용 공개 hook 표면과 실제 설치 wiring의 구분 | 명세 | `oms-command-hooks::surfaces`의 순수 ownership/convergence; lifecycle installer writer | [#532](https://github.com/eightmm/oh-my-setting/pull/532) | API/test 노출과 실제 installed invocation을 각각 추적; 등록·설치·실행 증거를 대조 |

## 21–27. 선택 안내와 읽기 전용 표면

| 번호 | 확인한 통합 대상 | 유형 | Canonical owner / 재사용 대상 | 연결 PR | 의존성과 다음 검증 |
|---|---|---|---|---|---|
| 21 | callable catalog / owner / MCP metadata | 명세 | root registry, owning parser, MCP descriptor의 기존 소유자 | [#533](https://github.com/eightmm/oh-my-setting/pull/533) | 등록·문법·discovery 대응 검증; MCP 목록 노출을 authorization으로 사용하지 않음 |
| 22 | Main capability descriptor | 명세 | `oms-command-panel::native_panel`의 Main descriptor | [#533](https://github.com/eightmm/oh-my-setting/pull/533) | 명령별 실제 지원·unsupported 이유·현재 세션 증거 검증; role 이름만으로 capability를 만들지 않음 |
| 23 | Desktop view descriptor | 명세 | `oms-command-panel::desktop_*` 및 기존 view handler | [#533](https://github.com/eightmm/oh-my-setting/pull/533) | view ID·표시·handler·지원 상태 대응; UI metadata가 state writer를 대체하지 않음 |
| 24 | consult / advise / agent-call / peer-ask 선택과 기본값 | 기존 PR + 명세 | 이미 공유된 `peer::council`; #318이 defaults 구현 소유 | [#318](https://github.com/eightmm/oh-my-setting/pull/318), Draft; [#533](https://github.com/eightmm/oh-my-setting/pull/533) | **#334 위의 stack**; thread·strategy·명시 target·고정 planning quorum 보존, 선택 안내만 보완 |
| 25 | plan-run / goal-drive / autopilot / graph / recipe 선택 UX | 명세 | 기존 각 실행 계층의 공개 계약 | [#533](https://github.com/eightmm/oh-my-setting/pull/533) | 작업 1개·목표 acceptance·승인 receipt·workflow·명시 host recipe의 선택 기준 검증; 새 실행기 추가 없음 |
| 26 | memory / journal / archive의 기록 검색 입구 | 명세 | Journal의 recorded-search 계약과 root host read adapter | [#533](https://github.com/eightmm/oh-my-setting/pull/533) | 출처·시간·현재성·제한/누락 표시 공유; 저장소·보존·private source 접근 정책 유지 |
| 27 | waiting / recovery의 읽기 전용 상태 설명 | 명세 | root read projection; autonomy / Panel / Landing 실행 owner 유지 | [#533](https://github.com/eightmm/oh-my-setting/pull/533) | 대기 이유·증거 현재성·지원되는 다음 동작 표시; 조회를 retry·재전송·권한 부여로 바꾸지 않음 |

## 코드 PR의 고정된 게시 근거

아래는 확인된 게시 revision이다. 후속 수정이 생기면 현재 PR HEAD와 그 HEAD의 checks를
별도로 확인한다. 최초 receipt를 새 HEAD의 검증 결과로 재사용하지 않는다.

| 항목 | 코드 PR / 현재 checks | 고정된 게시 근거 |
|---|---|---|
| 1 | [#529](https://github.com/eightmm/oh-my-setting/pull/529) · [checks](https://github.com/eightmm/oh-my-setting/pull/529/checks) | 최초 [`9b8163ff17f7e493a775f752f7204f82f51f9438`](https://github.com/eightmm/oh-my-setting/commit/9b8163ff17f7e493a775f752f7204f82f51f9438); 형식 수정 [`5d0d437a11504f3308cffdc6d61be5f4c1bba869`](https://github.com/eightmm/oh-my-setting/commit/5d0d437a11504f3308cffdc6d61be5f4c1bba869) |
| 11 | [#527](https://github.com/eightmm/oh-my-setting/pull/527) · [checks](https://github.com/eightmm/oh-my-setting/pull/527/checks) | 최초 [`98b5a5e49d0f74fe659b1f864086ca14d62f7d17`](https://github.com/eightmm/oh-my-setting/commit/98b5a5e49d0f74fe659b1f864086ca14d62f7d17); 형식 수정 [`a9c02686461ce4d7234722340a6ea0e6c6e59482`](https://github.com/eightmm/oh-my-setting/commit/a9c02686461ce4d7234722340a6ea0e6c6e59482) |
| 17 | [#528](https://github.com/eightmm/oh-my-setting/pull/528) · [checks](https://github.com/eightmm/oh-my-setting/pull/528/checks) | 최초 [`e8c1dc90f430e6ce966a5565f5b0b615388690e7`](https://github.com/eightmm/oh-my-setting/commit/e8c1dc90f430e6ce966a5565f5b0b615388690e7) |

## 이미 공유하는 구현과 유지할 경계

| 기준선에서 확인한 재사용 | 이번 후속의 범위와 유지할 경계 |
|---|---|
| Graph agent node는 [`plan-run`](https://github.com/eightmm/oh-my-setting/blob/4c7db603ecf33952598406532c9213cf3aa18293/crates/oms-command-graph/src/execution_runner.rs), bound commit은 [`goal-drive`](https://github.com/eightmm/oh-my-setting/blob/4c7db603ecf33952598406532c9213cf3aa18293/crates/oms-command-graph/src/execution_commit.rs) 사용 | Graph routing/gates, Plan lease, Goal acceptance/commit, Autopilot receipt/budget를 유지. 25번은 선택 안내 |
| consult / advise / agent-call / peer-ask는 [`peer::council`](https://github.com/eightmm/oh-my-setting/blob/4c7db603ecf33952598406532c9213cf3aa18293/crates/oms-command-peer/src/peer.rs) 사용 | 24번은 기본값·선택 UX. independent review gate와 planning quorum 유지 |
| [`Plan Git`](https://github.com/eightmm/oh-my-setting/blob/4c7db603ecf33952598406532c9213cf3aa18293/crates/oms-command-plan/src/repository.rs)은 supervisor, [`recipe step`](https://github.com/eightmm/oh-my-setting/blob/4c7db603ecf33952598406532c9213cf3aa18293/crates/oms-command-state/src/recipe.rs)은 Runs process 사용 | 9·10번은 남은 host와 companion 계약. 이미 이전한 caller를 다시 구현하지 않음 |
| [`DispatchContext`](https://github.com/eightmm/oh-my-setting/blob/4c7db603ecf33952598406532c9213cf3aa18293/crates/oms-command-peer/src/dispatch_context.rs)와 [`native assignment contract`](https://github.com/eightmm/oh-my-setting/blob/4c7db603ecf33952598406532c9213cf3aa18293/crates/oms-panel-core/src/native_contract.rs)가 존재 | 12번은 task-facing 연결. native send/adopt/return과 managed lease/worktree custody 유지 |
| [`patch-land`](https://github.com/eightmm/oh-my-setting/blob/4c7db603ecf33952598406532c9213cf3aa18293/crates/oms-command-landing/src/landing.rs)는 admission 호출; land와 draft-pr는 remote-tip helper 공유 | admission, local apply, exact commit, create-only draft publication, 선택적 merge 권한을 구분 |
| [`scopes_overlap`](https://github.com/eightmm/oh-my-setting/blob/4c7db603ecf33952598406532c9213cf3aa18293/crates/oms-contract-markdown/src/scope.rs)는 이미 공개되어 있음 | 6번은 기존 primitive 재사용. Graph gate·Plan resources/WIP·Room membership 정책은 각 scheduler 유지 |

과거 [호출 경로 감사](CALLABLE-CONSOLIDATION-AUDIT.md)와
[작업 큐](ORCHESTRATOR-WORK-QUEUE.md)의 날짜별 미연결 목록은 당시 source의 기록이다.
위 표의 현재 연결을 먼저 확인하고 후속 범위를 정한다.

## 권장 진행 순서와 기존 PR 관계

1. **작은 코드 변경의 정확한 HEAD 검증:** 1·11·17번의 regression과 기존 caller 계약을
   확인하고 각 PR의 CI를 판정한다. #522의 task binding은 별도 소유권 경계로 검토한다.
2. **Provider 기초 계약:** #334를 기준으로 #335와 #318의 stack diff를 확인한다. 4–7번의
   routing 명세는 이미 공유된 검증·probe를 연결하고 retry 권한을 caller에게 남긴다.
3. **실행 primitive:** 8번은 exact tree/ref/index 불변식과 domain별 durable intent 순서를
   먼저 고정한다. 9번의 remaining caller 이전과 10번의 duplex/companion 계약은 출력·신호·
   종료 확인 조건별로 나눈다. 락 취소 변경만으로 이 이전 전체가 끝난 것으로 보지 않는다.
4. **Context와 관측:** 13–16·18–20번을 현재 producer의 개별 delta로 구현한다. 외부 관측의
   정규화가 실행 성공, 삭제 허가 또는 installed hook 지원의 증거가 되지 않도록 검증한다.
5. **선택과 설명:** 21–27번은 위 실제 capability·source·receipt를 읽는 표면으로 연결한다.
   command/role/view 이름을 합치는 일보다 올바른 owner와 지원되는 다음 동작의 표시를 우선한다.

[#518](https://github.com/eightmm/oh-my-setting/pull/518),
[#520](https://github.com/eightmm/oh-my-setting/pull/520),
[#525](https://github.com/eightmm/oh-my-setting/pull/525)는 관련 접점의 검토 자료이며 위 27개
항목의 주 coverage로 추가 집계하지 않는다.
[#395](https://github.com/eightmm/oh-my-setting/pull/395)와
[#387](https://github.com/eightmm/oh-my-setting/pull/387)에 연관된 adaptive decision·external result·
recovery evidence는 일부가 이미 기준선에 연결되어 있다. PR이 열려 있다는 이유로 해당 기능
전체를 미구현으로 분류하지 않고 현재 source 대비 남은 diff를 7·27번에서 확인한다.
GC 관련 [#521](https://github.com/eightmm/oh-my-setting/pull/521)와 CI 관련
[#526](https://github.com/eightmm/oh-my-setting/pull/526)는 위 기준선에 반영된 변경이다.

## 검증 기록의 구분

이 작업 환경에는 Rust/cargo toolchain이 없다. 코드 변경의 로컬 `./verify`는 첫 `type`
단계에서 `cargo: command not found`, exit 127로 멈췄다. 이는 컴파일·테스트 통과 증거가
아니며, 소스 검토·바이트 비교·`git diff --check` 결과와 구분한다. CI의 일부 shard 성공도
현재 PR HEAD의 전체 필수 검증 성공으로 확대하지 않는다.

이 index와 네 명세 PR의 로컬 검증은 문서 참조, 1–27 번호 coverage, 실제 PR 링크 및
diff 형식 확인이다. 문서 검증으로 runtime 테스트를 대신하지 않는다. 후속 구현의 검증은
각 명세의 기존 canonical fixture와 caller 계약을 기준으로 하고, provider 호출이나 실제
원격 실행·게시를 문서 검증에 포함하지 않는다.

새 명세 문서는 해당 PR 링크에서 읽는다. 이 branch에 없는 문서의 상대경로를 현재 파일처럼
연결하지 않는다. 이 지도는 실행 권한, 자동 병합 승인 또는 CI 면제를 추가하지 않는다.
