# Overall Main / provider orchestrator: 남은 작업과 채택 조건

작성일: 2026-11-18. 소스 기준:
[`f9a695ea2de90a160754252c1ee9cbaa4dd7a488`](https://github.com/eightmm/oh-my-setting/commit/f9a695ea2de90a160754252c1ee9cbaa4dd7a488).
이 문서는 코드 변경과 아직 구현하지 않은 작업 명세를 연결하는 index다.
최신 source와 PR 상태는 구현 착수 시 다시 확인한다. 문서 병합은 해당 runtime 기능 완료가 아니다.
개념 점검 기준은 main `189bfdaa28b6b628f0d4d50a4ca3d0df28396b88`이다.
#259~#265는 문서 PR로 병합되었고 runtime source는 위 기준과 동일하다. #258은 draft다.
제품 범위와 개념의 구분은 [Feature scope](FEATURE-SCOPE.md), 실제 착수 상태는 이 큐가 소유한다.
이전 CP/TM/MR/UX/NF 번호는 acceptance의 출처이며 별도 전체 구현 지시가 아니다.

2026-11-20 공유 제안의 코드 대조와 후속 구현 범위는
[현재 감사 §10](ORCHESTRATOR-COMPLETION-AUDIT.md#10-2026-11-20-공유-제안의-현재-코드-대조와-구현)에
기록한다. 공개 handoff prompt 연결, 소유권과 최신성의 진단 구분, 기존 skill-manifest
검증 강화이며, 아래 lifecycle/Main 인계/실제 모델 평가 과제의 완료를 뜻하지 않는다.

## 1. PR 지도

| PR / 작업 | 실제 변경 | 구현 상태 / 다음 행동 |
|---|---|---|
| [#258](https://github.com/eightmm/oh-my-setting/pull/258) | 새 native orchestrator의 model/effort 선택 코드·fixture·문서 | Draft. HEAD `4af980f`의 기존 CI 성공 확인. connected session 거절의 run-level fixture와 review는 남음 |
| [#259](https://github.com/eightmm/oh-my-setting/pull/259) | [현재 감사](ORCHESTRATOR-COMPLETION-AUDIT.md), [LC-01~04](ORCHESTRATOR-LIFECYCLE-IMPLEMENTATION.md), 이 index | 병합된 명세. 기존 Rust 구성요소와 빠진 연결, 우선순위의 기준 |
| [#260](https://github.com/eightmm/oh-my-setting/pull/260) | [PERF-01~04](ORCHESTRATOR-PERFORMANCE-WORK-PACKETS.md) | 병합된 명세. 반복 I/O·Plan 읽기·usage persistence·lexical 전처리 |
| [#261](https://github.com/eightmm/oh-my-setting/pull/261) | [REL-01~04](ORCHESTRATOR-RELIABILITY-WORK-PACKETS.md) | 병합된 명세. native 관측·실패 분류·총 deadline·Broker supervision |
| [#262](https://github.com/eightmm/oh-my-setting/pull/262) | [VW-01~05](ORCHESTRATOR-VERIFICATION-WORK-PACKETS.md) | 병합된 명세. 범위 판정·검증 중복·실패 진단·fixture 효율 |
| [#263](https://github.com/eightmm/oh-my-setting/pull/263) | [AR-01~04](REMOTE-ARTIFACT-WORK-PACKETS.md) | 병합된 명세. 선택형 원격 수집·bundle 재개·cache 공간 관리 |
| [#264](https://github.com/eightmm/oh-my-setting/pull/264) | [Graph retrieval 비교](GRAPH-RETRIEVAL-COMPARISON.md)와 [GR-01~13](GRAPH-RETRIEVAL-WORK-PACKETS.md), native README 교정 | 병합된 비교·명세. Rust/Python/shell/notebook 정확성, worker 전달, 평가, 조건부 semantic/hybrid 실험 |
| [#265](https://github.com/eightmm/oh-my-setting/pull/265) | [호출 경로·중복 감사](CALLABLE-CONSOLIDATION-AUDIT.md), [CC-01~10](CALLABLE-CONSOLIDATION-WORK-PACKETS.md), MCP/Main swap 문서 교정 | 병합된 명세. 실제 caller, 재사용 대상과 제거 조건 |
| [#281](https://github.com/eightmm/oh-my-setting/pull/281) | Native Claude xhigh·subagent effort·Fable 관측 호환성 | Draft 구현. 전체 capability 통합은 기존 MR-A/TM owner의 후속 |
| [#282](https://github.com/eightmm/oh-my-setting/pull/282) | Native compaction 우선 안내, Main/Lead/leaf와 source 읽기 범위 명료화 | Draft 구현·지침. 자동 compaction·runtime lead 승격은 포함하지 않음 |

2026-11-19 [모델 업그레이드 후속](MODEL-UPGRADE-ADAPTATION.md)은 공식 지침과 현재 caller를
대조해 capability, native 관측, 공개 웹 조사 권한, Responses bridge의 완료 조건을
기존 MR/TM/REL owner에 연결한다. #258 launch와 #276 full catalog 수정, 병합된 #280
Dreaming 명세를 복제하지 않는다. Prompt audit는 스킬 참조·출력 스타일까지 점검하되
runtime prompt·실제 주입·중복·의미 충돌 미검사를 명시한다.

기존 40개 ID는 근거를 추적하기 위해 보존한다. **유지 16개, 기존 owner 작업에 흡수 9개,
조건부 15개**로 재분류했으며 40개 독립 프로젝트나 모두 완료해야 할 출시 조건이 아니다.
LC-01~04는 별도 핵심 연결 작업이다. “흡수”는 동등한 기능의 검증·연결을 한 owner가 관리한다는
뜻이며 모든 caller를 하나의 큰 PR에 합치거나 domain 계약을 제거하라는 뜻이 아니다.

- **유지:** 현재 caller에서 확인한 정확성·연결·일관성 문제. 우선순위에 따라 작은 PR로 진행한다.
- **흡수:** 해당 기능의 producer/consumer 또는 canonical fixture 변경에 포함한다. 독립 엔진·대장을 추가하지 않는다.
- **조건부:** 아래 필요·측정·API 조건이 충족되면 착수한다. 기존 구현을 삭제하거나 미지원으로 바꾸는 뜻이 아니다.

## 2. 먼저 이어갈 기존 핵심 연결 작업

[기존 감사](ORCHESTRATOR-COMPLETION-AUDIT.md)의 핵심 연결 작업은 계속 우선한다.

- **LC-01 → LC-02 → LC-03 → LC-04:** guard/claim 읽기, Team/write 예약 반환, active cancel·실제 cleanup,
  held domain/epoch 복구. 예약을 늘려 회수 누락을 덮지 않는다.
- **Native assignment:** task·owner generation·worktree·durable send intent·실제 결과를 연결한다.
  Native pane이 열린 것을 managed Team 완료·hard budget·격리의 증거로 사용하지 않는다.
- **Reviewed proposal:** 기존 `apply-proposal`로 task graph에 반영한 뒤 기존 dispatch를 사용한다.
- **Composite acceptance:** member별 결과를 합친 정확한 tree에서 기존 review/verifier를 실행한다.
- **Task 결과의 현재 경계:** 일반 native 대화에는 task binding이 필수가 아니다.
  Durable assignment가 필요한 작업은 현재 Plan claim과 Room participant/attempt를 확인하고,
  전송 intent, 실제 결과 artifact와 verifier evidence를 연결해야 한다. Room 수신 ack나
  native 대화의 완료 보고만으로 task를 수락하지 않는다. 전송 결과가 불명확하면 기존
  evidence를 확인하기 전 자동 재전송하지 않는다.
  현재 panel acceptance는 `--panel-accept` fence를 landing에 전달하지만,
  `patch-land` parser는 이 인자를 받지 않는다. 따라서 이 경로의 end-to-end 수락은
  미완료이며, 기존 acceptance 연결이 이미 동작한다고 가정하지 않는다.
- **Managed built-in adapter:** lifecycle과 요청 계약을 먼저 갖추고 provider 하나의 지원 경로부터 연결한다.
- **Catalog와 task 중심 board:** 기존 model/effort catalog·route evidence·Reader를 사용한다.
  #258의 launch 옵션 이후에도 account별 capability 최신성, 실제 적용 설정, task→Team→member 증거 연결은 남는다.

### Overall Main 아래의 작업과 native 세션 연결

2026-11-19, main `7d9b291592a926ce389693141d11ef8269eccfcd`에서 확인한 후속 범위다.
[패널 계약](TEAM-ORCHESTRATION-PANEL.md)은 하나의 작업 흐름 안에서 Main 직접 작업과
필요한 Provider Lead를 구분한다. Provider별 분류는 세션 이동·활동 분류이며 배정 관계가 아니다.
새 작업 큐를 만들지 않고 기존 TM-06과 P0-B/native assignment owner가 아래 순서로 연결한다.

| 기존 owner / 순서 | 이번 변경과 후속 범위 | 완료 조건 / 선행 관계 |
|---|---|---|
| TM-06A: 현재 Auto overview | Room membership과 lifecycle parent attempt가 현재 room의 live owner를 가리키는 Worker·Reviewer·Researcher·Advisor를 Main 직접 작업 영역에 표시 | 빈/유실/모호한 parent와 native 관측은 provider 활동으로 표시; 선택·검색·페이지·mouse target·기존 session shortcut 보존 |
| P0-B: 명시적 native 배정 | 기존 task/lease/attempt·Room reference와 native target/turn·결과를 연결 | 일반 native 대화에는 task 생성 강제 없음; 모호한 전송 뒤 direct 재배정·자동 재전송 금지; 작업과 실행 관계가 확인될 때만 공통 카드에 연결 |
| TM-05 / 기존 landing owner: 결과 수락 | 현재 panel acceptance → native landing parser 연결 오류를 별도 correctness diff로 수정하고 결과 근거를 기존 수락 경로로 소비 | `--panel-accept`를 무시하는 호환 처리 금지; exact owner/task/base/candidate와 검토 근거 재검증; 실제 native parser를 지나는 기존 entry fixture에서 성공·stale·복구 검사 |
| TM-06A/B: 같은 작업 inspector | 기존 Results/Events/Diff/Transcript reader에 작업·배정·결과 연결과 지원되는 action을 표시 | P0-B의 실제 binding과 해당 action capability를 소비; native 보고·verification·acceptance·publication 구분; 같은 작업을 실행 방식별로 중복 표시하지 않음 |
| TM-06C / PERF-02 / REL-01 | 연결된 projection의 bounded refresh와 입력·선택 보존 | 동일 snapshot 재사용; 새 모델 호출·전체 transcript 재파싱·state write 없이 열람; cold/warm I/O와 실제 terminal 동작은 별도 측정 |

첫 행은 이 변경의 read projection과 기존 render fixture 범위다. Task-first 통합과 native
assignment/result/control의 완료를 뜻하지 않는다. Native 배정 설계는
[#285](https://github.com/eightmm/oh-my-setting/pull/285), Main 선택은
[#286](https://github.com/eightmm/oh-my-setting/pull/286), 실제 역할 프롬프트는
[#284](https://github.com/eightmm/oh-my-setting/pull/284)를 함께 읽는다.

결과 수락 문제의 확인 경로는 `oms-panel-core/src/acceptance.rs`의 `--panel-accept`
생성 → `oms/src/services_collab.rs`의 인자 전달 → `oms-command-landing/src/cli.rs`의
해당 option 누락이다. 이는 소스 추적으로 확인한 거절 경로이며 이번 UI diff에서 수정하거나
live로 재현한 기능이 아니다. Read-only 분류 수정은 이 후속과 독립적으로 검토할 수 있지만,
수락·취소·복구 조작을 새로 활성화하는 데 필요한 증거를 대신하지 않는다.

Main이 역할과 실행 조건을 판단하며 사용자가 매번 native/direct 모드를 선택하지 않는다.
기존 Lead의 맥락을 이어 쓰는 작은 작업도 유효하고, 큰 작업의 독립 검토를 direct reviewer로
맡길 수도 있다. Lead는 native loop 안에서 직접 수행·분업하고 OMS-managed 실행이 필요하면
Main에 요청한다. Lead의 child marker를 제거하거나 parent-only guard를 완화하지 않는다.

새 성능/CI 개선은 이 P0의 예약·사용량·소유권·검증 의미를 유지해야 한다.
시간이 오래 걸린다는 이유로 checked API, fsync, exact-SHA proof를 생략하지 않는다.

## 3. 패킷별 채택 상태

상세 source·fixture·완료 조건은 위 연결된 패킷에 보존한다. 이 표의 채택 조건을 먼저 적용한다.
시작점은 LC-01/02, CC-02/04, VW-01이다. Graph의 명확한 parser/전달 오류는 해당 작은 회귀
fixture와 함께 고치며, 대규모 retrieval 평가를 기다리지 않는다. 우선순위는 측정된 병목 순위가 아니다.

| ID | 채택 | 현재 범위 / 흡수할 owner | 착수·보존 조건 |
|---|---|---|---|
| PERF-01 | 조건부 | REL-01 observer의 증분 읽기 후속 | 관측 상태를 먼저 고정하고 실제 반복 read 비용을 측정 |
| PERF-02 | 유지 | 한 refresh의 Plan read set 공유 | 새 global cache/atomic multi-store snapshot/권한 없음 |
| PERF-03 | 조건부 | LC 작업의 필요한 계측; 별도 저장 최적화 | 실제 write amplification 확인 후 batch/형식 판단; pending/history 보존 |
| PERF-04 | 조건부 | GR-05 이후 기존 lexical 전처리 재사용 | 반복 query 비용과 동일 순위·source validation oracle |
| REL-01 | 유지 | native 관측의 zero/unknown/partial 분리 | bounded source·redaction·identity·coverage 유지 |
| REL-02 | 유지 | typed failure와 재실행 적격성 | pin·호출 상한·physical receipt·권한 거절 유지 |
| REL-03 | 조건부 | 선택적 총 deadline / hard cooldown 정책 | 실제 caller 요구; 기존 per-call timeout과 정렬용 cooldown을 묵시 변경하지 않음 |
| REL-04 | 유지 | 기존 Reactor에 Broker I/O 감독 연결 | auth/catalog framing·credential 정책 유지; CC-06과 공통 API owner 조정 |
| VW-01 | 유지 | docs/affected planning과 native 준비 분리 | crate README 포함 두 classifier 일치; critical/unknown과 required status 유지 |
| VW-02 | 조건부 | noncritical leaf affected closure | 반복되는 실제 leaf 변경과 Cargo/feature/target 근거; 불명은 기존 coverage |
| VW-03 | 흡수 | VW-01과 같은 CI 정리 owner | 동일 환경·입력의 중복이 입증된 실행만 축소; full-proof classifier 함께 유지 |
| VW-04 | 흡수 | VW-01/03의 기존 timing·실패 진단 | 필요한 계측만; VW-01을 막는 별도 telemetry 프로젝트 없음 |
| VW-05 | 흡수 | LC와 변경 중인 각 canonical fixture | clock wait 축소; 실제 process/platform 경계 시험 유지 |
| AR-01 | 조건부 | 원격 collect를 사용하는 경로의 신뢰성 수정 | local cleanup과 remote job cancel 구분; 일반 Main의 선행 아님 |
| AR-02 | 조건부 | AR-03과 한 선택형 bundle 기능 | 여러 필수 artifact 실수요; 기존 단일 immutable bundle 수집부터 비교 |
| AR-03 | 조건부 | AR-02 manifest의 실제 consumer | AR-01/02 및 모든 필수 artifact 완료 receipt; manifest-only 기능 추가 금지 |
| AR-04 | 조건부 | cache 공간 inventory와 필요 시 정리 | 공간 문제 관측; 삭제는 durable completion과 명시적 적용 조건, 자동 GC 기본 제외 |
| GR-01 | 유지 | Rust scope별 symbol ID·span·signature | 동명 impl method 손실 방지; 기존 parser/query 사용 |
| GR-02 | 유지 | Rust nested module·path 관계 | GR-01과 같은 parser owner; 잘못된 EXTRACTED edge 거절 |
| GR-03 | 유지 | Worker의 no-match/partial/error 전달 | GR-04와 한 producer→consumer 계약; 정상 no-match로 Python 제외 rebuild 금지 |
| GR-04 | 유지 | Parse·resolve·query coverage | 기존 syntax_valid/omission 재사용; 새 coverage store 없음 |
| GR-05 | 유지 | Source freshness와 engine identity 근거 | stat cache hit와 byte verification 구분; PERF-04와 독립적인 정확성 |
| GR-06 | 흡수 | 각 parser/consumer 수정의 회귀 검증 | 작은 adversarial fixture 필수; held-out 한영/latency 평가는 ranking·GR-12/13 판단 때 |
| GR-07 | 흡수 | GR-03의 source 전달 품질 | GR-01 identity 이후 관련 선언 우선; truncation/omission을 consumer까지 전달 |
| GR-08 | 흡수 | GR-01/04 parser-index의 test metadata | 검색 분류만; Cargo/test 실행 생략의 권한으로 사용 금지 |
| GR-09 | 유지 | 기존 notebook provenance·좌표 정합성 | 새 code-only body 검색은 수요/평가 조건부; output을 code로 취급하지 않음 |
| GR-10 | 유지 | Python lexical binding·nested identity | 기존 fixture 재사용; 의도적 semantic correction 명시 |
| GR-11 | 유지 | Shell command 위치와 data argument 구분 | lexical/heredoc 재사용; false call 방지 |
| GR-12 | 조건부 | 필요한 경우 Rust LSP/SCIP enrichment | syntax fix 뒤에도 남는 type-resolution 실패 corpus와 설치된 bounded producer |
| GR-13 | 조건부 | 필요한 경우 한국어·본문 검색 확장 | 실제 no-match 평가; 기존 body/query expansion부터, 의존성은 measured gain 이후 |
| CC-01 | 유지 | Provider identity/alias/discovery 공유 | 현재 ID validation 차이 교정; target·권한·account 정책은 adapter에 유지 |
| CC-02 | 유지 | 전체 catalog의 routing consumer | display 20개와 allocation 분리; legacy/empty/unknown 구분 |
| CC-03 | 흡수 | CC-01의 provider 계약 정리 owner | registry decode만 공유; preview/execute 오류·선택·Claude default 의미 유지 |
| CC-04 | 유지 | [이 tree에서 MCP reader·canonical reference 연결](TOOL-CONTEXT-REVIEW.md) | 기존 capture core; SHA·root·권한·전체 저장본 withhold·응답 cap 보존. 설치 revision/CI 증거는 별도 확인 |
| CC-05 | 조건부 | Actual installer의 ownership/convergence 정리 | 설치 경로 수정이나 실제 drift가 있을 때; bare/absolute corpus와 rollback 선행 |
| CC-06 | 흡수 | REL-04와 공통 supervisor API owner | caller별 작은 PR: Plan → graph → task; task API 동등성 전 기존 verify 유지 |
| CC-07 | 조건부 | Lock cancellation watcher 제거 | 실제 contention/thread 비용 또는 관련 lock 변경; 의미 없는 API 확대 금지 |
| CC-08 | 조건부 | Current-source verification 집계 공유 | 실제 결과 drift 또는 두 production consumer의 필요; 단순 line 감소만으로 착수하지 않음 |
| CC-09 | 조건부 | Unsupported 표시 교정; host swap은 별도 | 구체적 handoff 수요와 LC/identity/cleanup 증거; quota만으로 자동 교체하지 않음 |
| CC-10 | 흡수 | CC-01/04와 실제 CLI/MCP 변경의 정합성 | 기존 등록/argv/schema fixture 재사용; 모든 CLI의 MCP 복제·범용 router 제외 |

MCP symbol slice는 GR-01의 identity/span을 소비하는 adapter로 연결한다. CLI와 MCP의
노출 범위를 같게 만드는 것 자체를 완료 목표로 삼지 않는다. 같은 supervisor를 쓰더라도
Broker protocol, SSH 파일 게시, task stdin/live output은 별도의 caller 계약이다.

## 4. 병렬 작업과 직렬 연결

| 작업 그룹 | 병렬화할 수 있는 부분 | 직렬로 다룰 경계 |
|---|---|---|
| Lifecycle | LC source/fixture 조사와 다른 모듈의 문서·계측 | LC-01 → 02 → 03 → 04; telemetry lock 변경과 PERF-03 |
| Panel | PERF-02의 Plan snapshot과 REL-01의 관측 결과 설계 | REL-01 → PERF-01; 최종 panel consumer/type 통합은 한 owner |
| Provider execution | REL-02 분류, CC-01/03 corpus와 REL-04 Broker의 독립 fixture | 같은 `providers.rs`의 REL-02/03/CC-01/03은 한 writer; account 정책은 별도 |
| Transport | Broker·collect·Plan Git 각 caller의 재현·계약 조사 | REL-04/AR-01/CC-06의 공통 Reactor API는 한 선행 PR; CC-06은 Plan → graph → task |
| Verification | VW-01 planning과 VW-05의 다른 fixture | `check.sh`/workflow/CI classifier를 바꾸는 VW-01/04/03/02는 순차 반영 |
| Graph | GR-06 평가 corpus와 GR-03/04 interface 설계, 다른 언어 fixture 조사 | GR-01 → GR-02; GR-05/06 → PERF-04; resolver/index/context의 공통 writer는 한 owner |
| Remote artifacts | AR-02 manifest와 AR-04 inventory 설계 | AR-01/02 → AR-03; 삭제 apply는 완료 증거 producer 이후 |
| Callable consolidation | CC-02/05/07/08의 다른 파일과 caller fixture | CC-04/09/10은 MCP/native 등록·host owner와 순차 연결; CC-09는 LC와 조정 |

한 작업에서 발견한 새 필요를 다른 worker의 owned file에 바로 섞지 않는다.
공통 API는 좁은 선행 PR로 정하고 caller를 순서대로 연결한다. 새 scheduler·DB·daemon으로
충돌을 피하려고 하지 않는다. 병렬로 작성한 diff도 최종 합성 tree를 다시 검증한다.

## 5. 구현 에이전트 전달문

> 최신 main의 실제 caller와 구현 PR을 확인하고 위 채택 표에서 가장 작은 유지 작업을 선택하세요.
> LC-01/02의 예약 반환, CC-02의 catalog consumer, CC-04의 output reader, VW-01의 문서 판정을
> 우선할 수 있습니다. 같은 파일의 writer는 한 명으로 정하고 작업별 critical gate를 지키세요.
> GR-01/02/03/04의 정확성 수정에는 GR-06의 작은 회귀를 함께 넣으세요. 더 넓은 평가·최적화는
> 필요한 알고리즘/비용 판단이 생길 때 수행하며 별도 benchmark 완료를 모든 fix의 선행으로 만들지 마세요.
> 흡수 항목은 해당 owner의 acceptance에 포함하세요. 조건부 항목은 실제 trigger를 기록한 뒤 착수하세요.
> 기존 Plan/Room/peer/supervisor/usage/landing과 canonical fixture를 재사용하세요.
> 기능 존재, production wiring, fixture, live provider 증거를 구분하고 unknown을 성공/0으로 채우지 마세요.
> 새 계층·DB·router·기본 telemetry·hosted CI job/matrix/schedule을 패킷 수 때문에 만들지 마세요.
> 성능 변경은 같은 workload/환경의 관측으로 판단하고 유료 모델/GPU/원격 실행을 기본 검증에 넣지 마세요.
> 실제 변경·재사용·검증·남은 제한을 PR에 적고 검토를 위해 열어 두세요.

이 전달문은 구현 범위의 지시이며 merge·설치·배포·계정 변경·데이터 삭제를 승인하는 문서가 아니다.
AR-04는 정리 기능을 구현하는 작업이고 실제 사용자 cache 삭제는 별도 명시된 요청이 있어야 한다.

## 6. 제외한 중복 확장과 검토 범위

- Reader rendered-line cache, graph parser cache, Room append reader는 이미 있다.
- Account cooldown 기록·pinned account·한정된 fallback·campaign admission·physical receipt도 이미 있다.
- Feature branch push와 PR의 이중 CI trigger는 현재 workflow에 없다.
- SSH 단일 파일 resume, snapshot, remote operation ledger를 새로 만들지 않는다.
- Panel dispatch → peer, graph/peer → context planner, task/plan/runtime/panel → work-inspect는 이미 연결되어 있다.
- Task/Plan, capture/peer-result reference, preview/execute account 선택, 두 JSON 표현은 각 계약을 보존한다.
- Python gate helper, 별도 검색 서비스, provider별 task DB나 상주 fleet scheduler를 복원·추가하지 않는다.

이번 재분류는 위 40개 패킷 전체, Team 문서 7개와 주요 native owner/caller를 대조했다.
과거 외부 비교 문서는 당시의 근거로 보존하며 이번에는 vendor 기능의 최신성을 재검증하지 않았다.
Runtime API 삭제·config/default·CI classifier 변경은 없다. 문서 검증 결과는 PR 본문에 기록한다.
Production 성능·실제 청구·실 vendor·SSH/GPU·최소 지원 OS 실행은 측정하지 않았다.

## 7. Memory consolidation 후속 작업

2026-11-19 추가. [증분 Dreaming과 경험 재사용 명세](MEMORY-CONSOLIDATION.md)는
main `9a37a6ab8e39e3d4f8e11b77961d6c547b2d630b`의 journal/memory와 외부 1차 자료를
대조한 설계다. 위 기존 40개 작업과 LC의 채택 상태는 유지한다. 아래 DR ID는 그에 추가된
memory 후속 범위이며 신규 runtime 구현, 자동화 활성화 또는 여섯 독립 프로젝트를 뜻하지 않는다.

| ID | 채택 | 현재 범위 / owner | 착수·보존 조건 |
|---|---|---|---|
| DR-01 | 유지 | journal/memory의 증분 입력 보존과 batch 파생 갱신 | cap/marker·날짜 반복을 먼저 재현; retry·crash·동시 writer·legacy 보존. 성능은 직접 측정 |
| DR-02 | 유지·선택형 | 기존 peer 경로의 semantic distill producer | DR-01 뒤 연결; source/scope/revision 검사와 #250 review eligibility 유지; 가용 model×effort 선택 사용 |
| DR-03 | 유지 | failure→action→verification 경험과 skill 후보 | DR-02 출처 재사용; #238/#239 repair 및 기존 skill-forge와 연결. 새 repair/CI 정책 없음 |
| DR-04 | 유지 | 관련 변경에 따른 memory 재검토·대체·폐기 | 기존 hygiene/citation/supersede/retire 사용; 사용자 결정과 임시 가정 구분 |
| DR-05 | 조건부 | 기존 context 위의 linked wiki/index | 반복된 다중 source 질의의 실제 부족과 비용 비교; GR-03/04/05/07 owner 연결, 파생 뷰로 재생성 |
| DR-06 | 흡수 | RF-05/06의 memory 귀속·성과 관측 | #240 usage와 task/result 연결, 필요 시 #241; 신규 benchmark/ledger/panel store 없음 |

첫 구현 단위는 DR-01이며, 그 정확성 수정은 semantic 모델 평가나 전체 native Team 완성을
기다리지 않는다. DR-02는 DR-01 다음으로 연결한다. DR-03/04의 자료와 fixture 준비는 병렬화할
수 있지만 journal/memory의 공통 writer와 최종 반영은 한 owner가 조정한다. DR-05는 조건부다.
DR-06의 최소 관측 연결은 producer와 함께 준비하되 별도 평가 플랫폼 구축을 선행시키지 않는다.

Native 기능 연결은 기존 LC/native assignment/composite acceptance, REL-01/RF-03/04에
남긴다. Catalog 최신성은 CC-01/02/03 및 #258/#276을 따른다. 조사 시점 #258은 draft,
#276은 open이며 아직 main 구현으로 간주하지 않는다. memory batch는 PERF-03의 usage
persistence와 별개 owner이고, 검증 선택은 기존 VW owner와 조정한다.

구현 에이전트는 새 명세의 DR별 완료 조건과 공통 전달문을 적용하고 실제 source/병합 상태를
다시 확인한다. 이 문서 PR은 provider 호출·새 유료 평가·배포·사용자 데이터 삭제를 실행하지 않는다.
