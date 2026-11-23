# Overall Main / provider orchestrator: 남은 작업과 채택 조건

작성일: 2026-11-18. 소스 기준:
[`f9a695ea2de90a160754252c1ee9cbaa4dd7a488`](https://github.com/eightmm/oh-my-setting/commit/f9a695ea2de90a160754252c1ee9cbaa4dd7a488).
이 문서는 코드 변경과 아직 구현하지 않은 작업 명세를 연결하는 index다.
최신 source와 PR 상태는 구현 착수 시 다시 확인한다. 문서 병합은 해당 runtime 기능 완료가 아니다.
개념 점검 기준은 main `189bfdaa28b6b628f0d4d50a4ca3d0df28396b88`이다.
#259~#265는 문서 PR로 병합되었고 runtime source는 위 기준과 동일했다. 당시 #258은 draft였다.
제품 범위와 개념의 구분은 [Feature scope](FEATURE-SCOPE.md), 실제 착수 상태는 이 큐가 소유한다.
이전 CP/TM/MR/UX/NF 번호는 acceptance의 출처이며 별도 전체 구현 지시가 아니다.

2026-11-22 재대조 기준은 main `8527cd07d87b16efbc6ac7f4041b7ebe40e0adc8`이다.
[§8](#8-2026-11-22-재대조와-작은-통합-단위)에 확인한 구현 상태와 다음 작은 변경을 기록한다.
이전 날짜·SHA의 조사 기록과 채택 분류는 보존하되 현재 미구현 목록으로 다시 사용하지 않는다.
PR의 병합, source 연결, fixture 존재와 이번 실행 결과는 각각 구분한다.
2026-11-23 main `051fd3ea`에서 §8의 PR 상태와 구현 경계를 다시 확인했다.

2026-11-20 공유 제안의 코드 대조와 후속 구현 범위는
[현재 감사 §10](ORCHESTRATOR-COMPLETION-AUDIT.md#10-2026-11-20-공유-제안의-현재-코드-대조와-구현)에
기록한다. 공개 handoff prompt 연결, 소유권과 최신성의 진단 구분, 기존 skill-manifest
검증 강화이며, 아래 lifecycle/Main 인계/실제 모델 평가 과제의 완료를 뜻하지 않는다.

2026-11-21 [Agent harness 패킷](AGENT-HARNESS-WORK-PACKETS.md)은 임의 에이전트 연결과
협업 표면의 공백을 AH-01~13으로 기록한다: 유지 6개, 기존 owner에 흡수 2개, 조건부 5개.
명세 전용이며 아래 40개 ID의 분류와 LC 우선순위를 바꾸지 않는다. AH-04는 RF-02,
AH-02는 TM-07/08/09와 REL-01, AH-13은 CP-15의 owner를 따른다. AH-08과 AH-10의
다음 변경은 §8의 conformance 경계와 council 좌석 항목이 같은 작업으로 소유한다.

## 1. PR 지도

| PR / 작업 | 실제 변경 | 조사 시점 상태 / 후속 |
|---|---|---|
| [#258](https://github.com/eightmm/oh-my-setting/pull/258) | 새 native orchestrator의 model/effort 선택 코드·fixture·문서 | 초기 조사 당시 Draft; 2026-11-22에는 병합 확인. 기존 세션의 재설정이나 실제 served 설정까지 구현한 것으로 확대하지 않음 |
| [#259](https://github.com/eightmm/oh-my-setting/pull/259) | [현재 감사](ORCHESTRATOR-COMPLETION-AUDIT.md), [LC-01~04](ORCHESTRATOR-LIFECYCLE-IMPLEMENTATION.md), 이 index | 병합된 명세. 기존 Rust 구성요소와 빠진 연결, 우선순위의 기준 |
| [#260](https://github.com/eightmm/oh-my-setting/pull/260) | [PERF-01~04](ORCHESTRATOR-PERFORMANCE-WORK-PACKETS.md) | 병합된 명세. 반복 I/O·Plan 읽기·usage persistence·lexical 전처리 |
| [#261](https://github.com/eightmm/oh-my-setting/pull/261) | [REL-01~04](ORCHESTRATOR-RELIABILITY-WORK-PACKETS.md) | 병합된 명세. native 관측·실패 분류·총 deadline·Broker supervision |
| [#262](https://github.com/eightmm/oh-my-setting/pull/262) | [VW-01~05](ORCHESTRATOR-VERIFICATION-WORK-PACKETS.md) | 병합된 명세. 범위 판정·검증 중복·실패 진단·fixture 효율 |
| [#263](https://github.com/eightmm/oh-my-setting/pull/263) | [AR-01~04](REMOTE-ARTIFACT-WORK-PACKETS.md) | 병합된 명세. 선택형 원격 수집·bundle 재개·cache 공간 관리 |
| [#264](https://github.com/eightmm/oh-my-setting/pull/264) | [Graph retrieval 비교](GRAPH-RETRIEVAL-COMPARISON.md)와 [GR-01~13](GRAPH-RETRIEVAL-WORK-PACKETS.md), native README 교정 | 병합된 비교·명세. Rust/Python/shell/notebook 정확성, worker 전달, 평가, 조건부 semantic/hybrid 실험 |
| [#265](https://github.com/eightmm/oh-my-setting/pull/265) | [호출 경로·중복 감사](CALLABLE-CONSOLIDATION-AUDIT.md), [CC-01~10](CALLABLE-CONSOLIDATION-WORK-PACKETS.md), MCP/Main swap 문서 교정 | 병합된 명세. 실제 caller, 재사용 대상과 제거 조건 |
| [#281](https://github.com/eightmm/oh-my-setting/pull/281) | Native Claude xhigh·subagent effort·Fable 관측 호환성 | 2026-11-19 조사 당시 Draft; 2026-11-20 병합(`d261fd27`). 전체 capability 통합은 기존 MR-A/TM owner의 후속 |
| [#282](https://github.com/eightmm/oh-my-setting/pull/282) | Native compaction 우선 안내, Main/Lead/leaf와 source 읽기 범위 명료화 | 2026-11-19 조사 당시 Draft; 2026-11-20 병합(`d261fd27`). 자동 compaction·runtime lead 승격은 포함하지 않음 |

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
  초기 감사의 `--panel-accept` parser 누락은 현재 source에서 수정되어 있다.
  기존 fence의 owner/task/base/candidate 재검증을 재사용하며, parser 연결만으로
  native assignment나 여러 member의 통합 수락까지 완료했다고 간주하지 않는다.
- **Managed built-in adapter:** lifecycle과 요청 계약을 먼저 갖추고 provider 하나의 지원 경로부터 연결한다.
- **Catalog와 task 중심 board:** 기존 model/effort catalog·route evidence·Reader를 사용한다.
  #258의 launch 옵션 이후에도 account별 capability 최신성, 실제 적용 설정, task→Team→member 증거 연결은 남는다.

### Overall Main 아래의 작업과 native 세션 연결

2026-11-19, main `7d9b291592a926ce389693141d11ef8269eccfcd`에서 확인한 후속 범위다.
[패널 계약](TEAM-ORCHESTRATION-PANEL.md)은 하나의 작업 흐름 안에서 Main이 배정한 direct
worker와 필요한 Provider Lead를 구분한다. 팀 Main의 직접 편집을 뜻하지 않는다.
Provider별 분류는 세션 이동·활동 분류이며 배정 관계가 아니다.
새 작업 큐를 만들지 않고 기존 TM-06과 P0-B/native assignment owner가 아래 순서로 연결한다.

| 기존 owner / 순서 | 이번 변경과 후속 범위 | 완료 조건 / 선행 관계 |
|---|---|---|
| TM-06A: 현재 Auto overview | Room membership과 lifecycle parent attempt가 현재 room의 live owner를 가리키는 Worker·Reviewer·Researcher·Advisor를 Main 직접 작업 영역에 표시 | 빈/유실/모호한 parent와 native 관측은 provider 활동으로 표시; 선택·검색·페이지·mouse target·기존 session shortcut 보존 |
| P0-B: 명시적 native 배정 | 기존 task/lease/attempt·Room reference와 native target/turn·결과를 연결 | 일반 native 대화에는 task 생성 강제 없음; 모호한 전송 뒤 direct 재배정·자동 재전송 금지; 작업과 실행 관계가 확인될 때만 공통 카드에 연결 |
| TM-05 / 기존 landing owner: 결과 수락 | 초기 parser 누락은 현재 수정됨; 기존 panel acceptance fence와 결과 수락 경로 재사용 | `--panel-accept`를 무시하는 호환 처리 금지; exact owner/task/base/candidate와 검토 근거 재검증. Native assignment와 composite acceptance는 별도 증거 필요 |
| TM-06A/B: 같은 작업 inspector | 기존 Results/Events/Diff/Transcript reader에 작업·배정·결과 연결과 지원되는 action을 표시 | P0-B의 실제 binding과 해당 action capability를 소비; native 보고·verification·acceptance·publication 구분; 같은 작업을 실행 방식별로 중복 표시하지 않음 |
| TM-06C / PERF-02 / REL-01 | [#267](https://github.com/eightmm/oh-my-setting/issues/267) 관리형 Board의 provider 관측을 기존 갱신 워커에 통합 | 격리 PTY에서 AGY 캡처가 대기 중일 때 Help 다시 그리기와 종료를 검증하고, 캡처 해제 뒤 갱신까지 별도 측정했다. PERF-02의 전체 Plan 읽기 계약과 REL-01의 zero/unknown/partial 표시, 실제 프로바이더의 cold/warm p95는 후속 검증 범위다. |

첫 행은 이 변경의 read projection과 기존 render fixture 범위다. Task-first 통합과 native
assignment/result/control의 완료를 뜻하지 않는다. Native 배정 설계는
[#285](https://github.com/eightmm/oh-my-setting/pull/285), Main 선택은
[#286](https://github.com/eightmm/oh-my-setting/pull/286), 실제 역할 프롬프트는
[#284](https://github.com/eightmm/oh-my-setting/pull/284)를 함께 읽는다.

초기 결과 수락 문제는 `oms-panel-core/src/acceptance.rs`의 `--panel-accept`
생성 → `oms/src/services_collab.rs`의 인자 전달 → landing parser의 option 누락이었다.
2026-11-22 재대조에서는 [현재 parser](../crates/oms-command-landing/src/cli.rs)가 이 인자를
받고 크기 및 조합을 검증하며, [기존 landing fixture](../crates/oms-command-landing/tests/landing_contract.rs)에
해당 경로가 있음을 확인했다. 이번 문서 변경에서 runtime fixture나 live 수락을 실행한 것은 아니다.
Read-only 분류는 수락·취소·복구 조작을 새로 활성화하는 데 필요한 증거를 대신하지 않는다.

Main이 역할과 실행 조건을 판단하며 사용자가 매번 native/direct 모드를 선택하지 않는다.
기존 Lead의 맥락을 이어 쓰는 작은 작업도 유효하고, 큰 작업의 독립 검토를 direct reviewer로
맡길 수도 있다. Lead는 native loop 안에서 직접 수행·분업하고 OMS-managed 실행이 필요하면
Main에 요청한다. 현재 [계층 계약](../custom-skills/oms-agent-harness/references/coordination-layers.md)에
따라 Lead는 substantive work를 native children에게 맡기고 자신의 편집은 scoped integration과
unblocking으로 제한한다. Native children이 없으면 직접 수행하고 그 제한을 보고한다.
Lead의 child marker를 제거하거나 parent-only guard를 완화하지 않는다.

새 성능/CI 개선은 이 P0의 예약·사용량·소유권·검증 의미를 유지해야 한다.
시간이 오래 걸린다는 이유로 checked API, fsync, exact-SHA proof를 생략하지 않는다.

## 3. 패킷별 채택 상태

상세 source·fixture·완료 조건은 위 연결된 패킷에 보존한다. 이 표의 채택 조건을 먼저 적용한다.
초기 시작점은 LC-01/02, CC-02/04, VW-01이었다. CC-02/04와 VW-01의 구현된 부분은
§8에서 제외하고 실제 남은 caller부터 선택한다. Graph의 명확한 parser/전달 오류는 해당 작은 회귀
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
| CC-02 | 유지·구현됨 | #276의 전체 catalog routing consumer 재사용 | display 20개와 allocation 분리; legacy/empty/unknown 계약을 회귀로 보존 |
| CC-03 | 흡수 | CC-01의 provider 계약 정리 owner | registry decode만 공유; preview/execute 오류·선택·Claude default 의미 유지 |
| CC-04 | 유지·구현됨 | [MCP reader·canonical reference 연결 재사용](TOOL-CONTEXT-REVIEW.md) | 기존 capture core; SHA·root·권한·전체 저장본 withhold·응답 cap 보존. 설치 revision/CI 증거는 별도 확인 |
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
> §8의 확인 상태로 완료된 slice를 제외하고 LC/REL/CC의 실제 미연결 caller를 선택하세요.
> CC-02의 catalog consumer와 CC-04의 output reader는 재사용하세요.
> 같은 파일의 writer는 한 명으로 정하고 작업별 critical gate를 지키세요.
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
남긴다. Catalog 최신성은 CC-01/02/03 및 #258/#276을 따른다. 이 절의 2026-11-19 조사 당시
#258은 draft, #276은 open이었으며, 2026-11-22의 병합·구현 확인은 §8을 따른다. memory batch는 PERF-03의 usage
persistence와 별개 owner이고, 검증 선택은 기존 VW owner와 조정한다.

구현 에이전트는 새 명세의 DR별 완료 조건과 공통 전달문을 적용하고 실제 source/병합 상태를
다시 확인한다. 이 문서 PR은 provider 호출·새 유료 평가·배포·사용자 데이터 삭제를 실행하지 않는다.

## 8. 2026-11-22 재대조와 작은 통합 단위

기준은 main
[`8527cd07`](https://github.com/eightmm/oh-my-setting/commit/8527cd07d87b16efbc6ac7f4041b7ebe40e0adc8)과
당일 확인한 PR 상태다. 아래는 실제 중복·의미 차이를 기존 owner에 배정하는 index다.
새 작업 번호, 별도 전체 명세, 새 registry/DB/daemon을 만들지 않는다. Source와 fixture를
읽어 연결을 확인한 것이며, 이 절의 추가로 runtime 테스트나 live provider 실행을 했다는 뜻은 아니다.
2026-11-23 main `051fd3ea`에서 PR 상태와 아래 경계를 다시 확인했고, 바뀐 내용에는 그 날짜를 적었다.

### 구현된 부분과 기존 열린 변경

| 확인한 slice | 현재 근거와 재사용할 경계 | 계속 남는 범위 |
|---|---|---|
| 새 Lead의 model/effort 선택 | 병합된 [#258](https://github.com/eightmm/oh-my-setting/pull/258), [native orchestrator](../crates/oms-command-panel/src/orchestrator.rs) | Connected session 재설정·실제 served 설정·account-scoped capability는 별도 |
| Full catalog와 update 환경 전달 | 병합된 [#276](https://github.com/eightmm/oh-my-setting/pull/276), [routing inputs](../crates/oms-command-panel/src/commands.rs), [update](../crates/oms-command-lifecycle/src/update.rs) | CC-02 재구현 제외. Post-refresh 연동 점검과 정제된 실패 진단은 아래 lifecycle slice |
| Canonical output reader | CC-04의 [기존 연결](TOOL-CONTEXT-REVIEW.md) | Capture reference와 peer answer reference의 owner 차이 유지 |
| Main 선택과 역할 계약 | 병합된 [#286](https://github.com/eightmm/oh-my-setting/pull/286), [#321](https://github.com/eightmm/oh-my-setting/pull/321), [현재 Main registry](../crates/oms-command-panel/src/native_panel.rs) | 현재 registry는 Codex/Claude/AGY와 Pi CodexGateway. Main manage-only는 대부분 prompt 계약이다. 2026-11-23 main은 Claude Code panel Main의 `Edit`·`Write`·`MultiEdit`·`NotebookEdit`만 관리 저장소와 linked worktree 안에서 거절한다(`f214abad`; panel 시작 시 `OMS_MAIN_WRITE_GUARD=0`이면 꺼짐). Bash 변경과 Claude Code가 아닌 Main의 편집은 막지 않음 |
| Landing parser·docs planning | [Landing parser](../crates/oms-command-landing/src/cli.rs)는 `--panel-accept`를 소비하고 [check.sh](../scripts/check.sh)는 docs-only reference 경로 전에 native payload를 준비하지 않음 | Native assignment/composite acceptance, 일반 runtime·hosted CI 성공까지 확대하지 않음 |
| Runtime state envelope | [Native adapter](../crates/oms/src/services_control.rs)의 callback이 [state projection](../crates/oms-command-state/src/state_projection.rs)에 이미 연결됨 | [Feature scope](FEATURE-SCOPE.md)의 설명 정정(원래 [#316](https://github.com/eightmm/oh-my-setting/pull/316))을 재사용; 새 callback 연결 불필요 |

[#314](https://github.com/eightmm/oh-my-setting/pull/314)의 unknown benchmark 비교와
[#315](https://github.com/eightmm/oh-my-setting/pull/315)의 accepted verification 뒤 failure resolve는
열린 구현 변경이다. 2026-11-23에도 둘 다 open이며 main에 없다.
[후속 연구 §10](RESEARCH-FOLLOWUPS.md#10-2026-11-21-하네스-강화의-현재-경계)(원래 #316)은
handoff section별 손실 표시(REL-01)와 failure→action→accepted receipt의 정확한 연결(DR-03/04)을
이미 명세한다. 해당 작업을 복제하지 않는다. #315의 command 단위 resolve와 exact recovery lineage도 구별한다.
#316의 Main 직접 수행 문구는 PR head `d2b4da0e`에서 팀 Main/solo session을 구분하도록 정정되었고,
그 문구가 후속 연구 §10에 있다.

### 여덟 통합 경계와 다음 변경의 합격 조건

아래 runtime 변경은 이 문서에서 구현하지 않았다. 기존 명세 중에서 **probe producer의 일치,
typed dispatch context의 projection, model-family/unknown의 의미 교정**을 새로 좁힌다.
Provider identity 정리와 family 의미 변경은 별도 diff로 검토한다. 순서는 의존 관계이며
전체 기능을 한 PR이나 한 engine으로 합치라는 뜻이 아니다.

| 통합 경계 / 기존 owner | 다음 작은 변경 | 기존 fixture에서 확인할 합격 조건 |
|---|---|---|
| Provider probe·capability cache / MR-A, TM-01·07~09 | [model-doctor](../crates/oms-command-providers-skills/src/models.rs)와 [doctor](../crates/oms-command-lifecycle/src/provider_check.rs)의 CLI probe·cache producer를 공유하고 `exact_model_override`를 provider capability 근거로 표시 | 같은 관측 근거의 usability·cache identity·freshness 의미가 두 caller에서 일치. 서로 다른 시각에 읽은 age 숫자의 동일성은 요구하지 않음. Version/help 부분 실패, binary 교체, unknown, probe 미실행을 구별. Exact-model flag 지원을 model entitlement로 확대하지 않음 |
| DispatchContext / 기존 panel·peer dispatch owner, LC·REL-02 | [peer delegate](../crates/oms-command-peer/src/delegate.rs)와 [panel dispatch](../crates/oms-panel-core/src/dispatch.rs)의 task·parent attempt·Room/member·role/access·기존 route provenance를 typed value에서 env와 attempt refs로 변환 | 현재 단계별 subprocess 경계와 protected binding을 보존하고 두 projection의 동일한 metadata·누락 의미를 확인. Env metadata 자체를 권한으로 사용하지 않음. 일반 context manifest/compiler·graph·raw context 전달로 범위를 확대하지 않음 |
| Main/Lead/Worker 정책 / 기존 [계층 계약](../custom-skills/oms-agent-harness/references/coordination-layers.md)과 native prompt owner | 이번 문서의 역할 충돌을 정정. 후속은 compact/full prompt가 공유할 책임과 caller별 명령 안내를 구분해 기존 renderer/fixture에 연결 | 팀 Main은 관리, solo session은 직접 작업, Lead의 scoped integration·native-child 부재 예외, leaf의 무위임을 모든 실제 projection에서 보존. Prompt 통일을 runtime guard 구현으로 표시하지 않음 |
| Provider identity / CC-01; model family / MR-A·MR-C | Identity/alias/discovery는 [CC-01](CALLABLE-CONSOLIDATION-WORK-PACKETS.md#cc-01--provider-identity와-discovery) 범위만 공유. 별도 semantic diff로 [routing family](../crates/oms-panel-core/src/routing.rs)와 producer의 unknown·composite evidence 차이를 정정 | Alias·custom ID·owner cwd discovery를 유지. 같은 model ID의 family 해석을 대조하고 identifier-derived 진단을 verified served-family나 reviewer independence 증거로 승격하지 않음 |
| 남은 subprocess caller / CC-06·REL-04 | 기존 [Reactor](../crates/oms-supervisor/src/process.rs)를 재사용. Plan Git의 남은 caller부터 연결하고 Broker·Graph·Task는 각 별도 계약으로 진행 | Deadline·cancel·stdin EOF·output cap·exit/signal·소유한 자식 회수를 보존. Task의 inherited stdin/live raw output, Broker framing/backpressure가 동등하기 전 해당 loop를 제거하지 않음 |
| Account registry / CC-03 → CC-01 | [기존 CC-03](CALLABLE-CONSOLIDATION-WORK-PACKETS.md#cc-03--account-readvalidation-계약-공유)의 bounded decode·schema·ID·home validation만 공통화 | 같은 registry corpus에서 같은 validation. Preview를 실행 account pin으로 바꾸지 않으며 cooldown/retry 재선택·credential env·Claude implicit default·ownership writer 보존 |
| Tool 갱신 후 연동 / 기존 lifecycle·doctor owner, #276 후속 | [update](../crates/oms-command-lifecycle/src/update.rs)의 Verify → core commit → RefreshTools 뒤 bounded integration probe를 재사용하고 phase/tool/exit 진단을 정제 | Refresh 뒤 hook/plugin/MCP가 깨진 fixture 탐지. Success/failure/explicit skip과 확정된 core receipt를 보존. Vendor binary를 core rollback으로 복구했다고 표시하거나 raw provider 출력·secret을 노출하지 않음 |
| Native adapter conformance / [AH-08](AGENT-HARNESS-WORK-PACKETS.md#10-ah-08--adapter-conformance-시험), [#319](https://github.com/eightmm/oh-my-setting/pull/319) | Native `oms doctor --providers`에 #319의 adapter conformance를 연결. 기본 probe는 inference를 실행하지 않고 실제 실행은 명시적 opt-in으로 구분 | Public doctor 진입에서 fake adapter의 marker·target·stdin/payload·`-` prompt·output overflow·deadline/cancel·stderr/exit·malformed/unknown을 검사. 기존 [peer contract](../crates/oms-command-peer/tests/peer_contract.rs)를 재사용하되 public doctor 연결 검증을 대체하지 않음. Shell example 성공을 native/live provider 적합성으로 표시하지 않음 |

첫 코드 단위는 probe producer와 Plan Git caller로 나누어 독립 검토할 수 있다.
Post-refresh 점검은 같은 probe 의미를 재사용하고 lifecycle writer와 순차 반영한다.
CC-01과 CC-03은 provider 파일의 한 writer가 연결하되, model-family 의미 변경은
별도 회귀와 diff로 검토한다. Dispatch context도 공통 value를 먼저 정하고 실제 caller를
순서대로 옮긴다. Compact/full prompt와 native conformance는 각 기존 fixture owner에서
독립 진행할 수 있다. Broker·Graph·Task의 supervisor 이전은 Plan 변경만으로 완료되지 않으며
각 caller의 위 계약이 보존된 뒤 old loop를 제거한다. 위 순서가 #314/#315의 correctness 수정이나
#316의 handoff·recovery 후속을 새 공통 체계 완료까지 미루는 조건은 아니다.

Council 기본 좌석은 [AH-10](AGENT-HARNESS-WORK-PACKETS.md#12-ah-10--council-기본-좌석의-설정화)(원래 #313)과
[#318](https://github.com/eightmm/oh-my-setting/pull/318)의 기존 설정 owner에서 이어간다.
권장 후속은 명시 `--providers` → `OMS_PEER_PROVIDERS` → optional 설정 기본값 → legacy 기본값의
우선순위를 해당 consumer에 연결하는 것이다. 최초 재대조 시점에는 설정 구현 전이었다.
같은 날 구현 PR의 상태는 아래 표를 따른다.
Panel의 기존 `council.extra` 의미와 strict schema를 보존하며 새 council 명세나 별도 router는 만들지 않는다.

### 같은 날 게시된 구현 slice와 검토 순서

아래는 최초 재대조 이후 게시한 검토 단위를 위 owner에 연결한 지도다. 위의 다음 변경과
합격 조건을 대체하지 않으며, PR 게시를 병합·전체 roadmap 완료·hosted CI 또는 live provider
검증 완료로 읽지 않는다. 실제 실행 증거와 남은 범위는 각 PR에서 확인한다.
2026-11-23 기준 #330은 open이고 나머지 표의 PR은 모두 open draft이며, main `051fd3ea`에는
어느 slice도 병합되지 않았다. 통합 branch에 옮긴 사본도 main 병합 전에는 미완료로 본다.

| 기존 owner | 게시된 PR | 이번 slice와 계속 남는 범위 |
|---|---|---|
| CC-06·REL-04 | [#328](https://github.com/eightmm/oh-my-setting/pull/328) | Plan Git caller를 기존 supervisor에 연결. Broker·Graph·Task의 서로 다른 I/O 계약 이전은 별도 |
| MR-A, TM-01·07~09 | [#329](https://github.com/eightmm/oh-my-setting/pull/329) | Probe 결과 해석·cache metadata·exact-model override 표시 공유. Process/cache I/O와 실패 시 effort/cache provenance는 caller별 계약 유지 |
| 계층 계약·native prompt owner | [#330](https://github.com/eightmm/oh-my-setting/pull/330) | Main/Lead/Worker 문서 정합성과 이 owner 지도(이 절과 [Feature scope](FEATURE-SCOPE.md)의 역할 문구, 원래 #330). Compact/full prompt의 공통 책임 projection은 후속 |
| Panel·peer dispatch, LC·REL-02 | [#331](https://github.com/eightmm/oh-my-setting/pull/331) | Typed dispatch metadata의 env/attempt refs projection. 기존 protected binding을 보존하며 env를 권한으로 승격하지 않음 |
| AH-08, 기존 #319 | [#333](https://github.com/eightmm/oh-my-setting/pull/333) | Native doctor의 non-inference adapter probe 연결. 명시적 실행·write·`-` prompt·output isolation 및 live coverage는 #319 후속 |
| CC-01 | [#334](https://github.com/eightmm/oh-my-setting/pull/334) | Provider identity·alias·owner cwd discovery 공유. Model-family/unknown evidence의 의미 교정은 별도 diff |
| CC-03 → CC-01 | [#335](https://github.com/eightmm/oh-my-setting/pull/335) | Account registry decode·validation 공유. Caller별 선택·pin·retry·credential 정책을 합치는 변경은 아님 |
| Lifecycle·doctor, #276 후속 | [#336](https://github.com/eightmm/oh-my-setting/pull/336) | Post-refresh integration 검증과 정제된 phase/tool/exit 진단. Vendor binary를 core rollback으로 복구한다는 보장은 추가하지 않음 |
| AH-10, 기존 council 설정 owner | [#318](https://github.com/eightmm/oh-my-setting/pull/318) | 확인한 local ref(`a67c125c`)는 `OMS_PEER_PROVIDERS` 기본 좌석을 추가한다. CLI → nonempty env → optional `council.peer_default` → legacy 우선순위와 strict reader 연결은 PR 설명 기준, 미확인. `council.extra`·panel 배정·Team 권한은 별도 |

겹치는 provider 변경은 **#329 → #334 → #318 / #335** 순서로 검토한다. 2026-11-23 `gh` 기준
#318과 #335의 base branch는 #334의 head branch이며, 어느 한쪽의 검토를 다른 쪽의 합격 증거로 사용하지 않는다.
#336은 main 기준 독립 slice다. #328/#330/#331/#333은 각 PR의 실제 base와 변경 범위를
확인해 별도로 검토하며, 이 지도가 일괄 병합 순서나 전체 gate 통과를 선언하지 않는다.

#316의 팀 Main/solo 문구는 [후속 연구 §10](RESEARCH-FOLLOWUPS.md#10-2026-11-21-하네스-강화의-현재-경계)에
반영되었으며, handoff section 손실 표시와 exact recovery lineage는 그 기존 REL-01·DR-03/04
owner에 남긴다. [#319](https://github.com/eightmm/oh-my-setting/pull/319)는 #333의 native probe
slice와 남은 실행 conformance를 연결하는 기존 owner다. 새 중복 명세나 완료 표시를 만들지 않는다.
