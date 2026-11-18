# Overall Main / provider orchestrator: 구현 대기 작업과 실행 순서

작성일: 2026-11-18. 소스 기준:
[`f9a695ea2de90a160754252c1ee9cbaa4dd7a488`](https://github.com/eightmm/oh-my-setting/commit/f9a695ea2de90a160754252c1ee9cbaa4dd7a488).
이 문서는 코드 변경과 아직 구현하지 않은 작업 명세를 연결하는 index다.
최신 source와 PR 상태는 구현 착수 시 다시 확인한다. 문서 병합은 해당 runtime 기능 완료가 아니다.
호출 통폐합 조사 시 main `283ab6c1`까지 다시 확인했다. #259~#264는 문서 PR로 병합되었고
runtime source는 위 기준과 동일하다. #258은 여전히 draft다.

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
| [CC-01~10](CALLABLE-CONSOLIDATION-WORK-PACKETS.md) | [호출 경로·중복 감사](CALLABLE-CONSOLIDATION-AUDIT.md), 통폐합 명세, MCP/Main swap 문서 교정 | 명세. 실제 caller와 existing core를 연결하고 중복 규칙의 제거 조건 지정 |

추가 조사 명세는 성능·신뢰성·검증·원격 산출물 **17개**, graph retrieval **13개**,
호출 통폐합 **10개**, 총 **40개 패킷**이다. 기존 LC-01~04는 별도 핵심 연결 작업이다.
패킷 하나는 하나 이상의 작은 코드 PR로 나눌 수 있다.
예를 들어 REL-03의 총 deadline과 cooldown admission, PERF-03의 계측과 저장 형식 변경은
같은 코드 PR에 강제로 묶지 않는다. #260–#264의 명세는 main에서 함께 참조할 수 있다.

## 2. 먼저 이어갈 기존 핵심 연결 작업

[기존 감사](ORCHESTRATOR-COMPLETION-AUDIT.md)의 핵심 연결 작업은 계속 우선한다.

- **LC-01 → LC-02 → LC-03 → LC-04:** guard/claim 읽기, Team/write 예약 반환, active cancel·실제 cleanup,
  held domain/epoch 복구. 예약을 늘려 회수 누락을 덮지 않는다.
- **Native assignment:** task·owner generation·worktree·durable send intent·실제 결과를 연결한다.
  Native pane이 열린 것을 managed Team 완료·hard budget·격리의 증거로 사용하지 않는다.
- **Reviewed proposal:** 기존 `apply-proposal`로 task graph에 반영한 뒤 기존 dispatch를 사용한다.
- **Composite acceptance:** member별 결과를 합친 정확한 tree에서 기존 review/verifier를 실행한다.
- **Managed built-in adapter:** lifecycle과 요청 계약을 먼저 갖추고 provider 하나의 지원 경로부터 연결한다.
- **Catalog와 task 중심 board:** 기존 model/effort catalog·route evidence·Reader를 사용한다.
  #258의 launch 옵션 이후에도 account별 capability 최신성, 실제 적용 설정, task→Team→member 증거 연결은 남는다.

새 성능/CI 개선은 이 P0의 예약·사용량·소유권·검증 의미를 유지해야 한다.
시간이 오래 걸린다는 이유로 checked API, fsync, exact-SHA proof를 생략하지 않는다.

## 3. 추가 구현 큐

아래 우선순위는 작업 간 제안 순서다. 실제 서버에서 측정한 병목 순위가 아니다.

| ID | 우선순위 | 구체적 결과 | 선행 / 보존할 조건 |
|---|---|---|---|
| VW-01 | P0 | docs/mode 판정에서 native build와 state 준비 분리 | 기존 path classifier 사용; mixed/critical/unknown은 필요한 coverage 유지 |
| PERF-02 | P1 | 한 panel refresh에서 Plan을 한 번 취득해 projection 공유 | UI read set은 mutation 권한이 아님 |
| REL-01 | P1 | 실제 0개와 unavailable/partial/unsupported/omission 구분 | 기존 bounded observer·redaction·PID identity 유지 |
| PERF-01 | P1 | native transcript 증분 읽기 및 bounded 재검증 | REL-01 이후; metadata hit는 cached observation |
| REL-02 | P1 | typed failure와 재실행 적격성 | account pin·기존 최대 호출 수·physical receipt 보존 |
| REL-03 | P1 | 선택적 총 operation deadline과 cooldown admission | cooldown 부분은 REL-02 이후; 기존 per-call timeout 유지 |
| REL-04 | P1 | auth/catalog Broker의 write/read/pagination/cleanup 감독 | 기존 Reactor duplex; 새 provider 권한 없음 |
| VW-04 | P1 | 기존 Rust timing parser와 bounded 실패 증거 연결 | 기본 업로드 없음; 원래 exit와 scrub 경계 보존 |
| VW-03 | P1 | 측정된 build/install 중복 제거와 coverage label 정렬 | VW-04 측정; exact-SHA/full-proof classifier와 동시 변경 |
| VW-05 | P1 | 실제 clock 대기 축소와 deterministic native fixture | 실제 child ownership/cleanup 시험은 유지 |
| AR-01 | P1, 원격 경로 사용 시 | collect의 취소·총 deadline·게시 불명 처리 | local client cleanup과 remote job 취소 구분 |
| PERF-04 | P2 | graph lexical 전처리 재사용 | GR-05/06의 identity·품질 기준과 연결; 기존 순위/점수/source validation oracle 보존 |
| PERF-03 | P2 | usage write amplification 계측과 필요한 작은 개선 | LC lock 순서·durable pending 보존; 저장 형식 변경은 별도 근거 |
| VW-02 | P2 | noncritical leaf의 안전한 affected test closure | VW-01 이후; advisory 비교 후 executor 연결, 불명은 full |
| AR-02 | P2, 선택형 | run/snapshot에 묶인 bounded artifact manifest | 실제 묶음 수집 필요; 파일·총량·경로 상한 |
| AR-03 | P2, 선택형 | partial/resume과 묶음 완료 receipt | AR-01 + AR-02; 모든 필수 artifact 확인 전 완료 금지 |
| AR-04 | P2, 선택형 | cache 공간 inventory·정리 계획 | inventory 독립; 삭제 apply는 durable completion producer 선행 |

### Graph retrieval 후속 큐

[GR 상세 패킷](GRAPH-RETRIEVAL-WORK-PACKETS.md)은
owner, source, canonical fixture와 완료 조건을 지정한다. 비교 분석은 새 DB보다 기존
SQLite/graph/context 경계의 정확성 보완을 우선하며 검색 성능 수치는 아직 측정하지 않았다.

| ID | 우선순위 | 구체적 결과 | 선행 / 보존할 조건 |
|---|---|---|---|
| GR-01 | P0 | Rust scope별 symbol ID·본문 span·signature | 동명 impl method 손실 방지; 기존 API/query 재사용 |
| GR-02 | P0 | Rust nested module·`#[path]` 관계 | GR-01과 같은 parser 파일은 순차; 잘못된 EXTRACTED edge 금지 |
| GR-03 | P0 | Worker에 no-match/partial/오류와 snapshot 근거 전달 | 정상 no-match로 Python 제외 rebuild 금지; 기존 bundle/pack 사용 |
| GR-04 | P0 | Parse·resolve·query coverage | 이미 있는 syntax_valid 재사용; invalid/unsupported/unknown 구분 |
| GR-05 | P1 | Freshness 근거·engine fingerprint | stat reuse와 byte verification 구분; PERF-04 cache identity 선행 |
| GR-06 | P0 | 동일 corpus/budget의 retrieval 평가 | 기존 history/parity 보존; 한영 seed와 holdout, entry/file/shown-symbol 분리 |
| GR-07 | P1 | 관련 선언을 먼저 싣는 byte budget | GR-01/03/06; truncation·omission을 consumer까지 전달 |
| GR-08 | P1 | Rust test metadata의 탐색 분류 | production file 전체를 test-only로 만들지 않음; 실행 범위 축소는 별도 |
| GR-09 | P1 | Notebook code-only 검색·좌표 정합성 | 기존 cell 추출 재사용; output은 검색 제외 |
| GR-10 | P1 | Python lexical binding·nested identity | legacy golden의 의도적 semantic correction 명시 |
| GR-11 | P1 | Shell command 위치와 data argument 구분 | 기존 lexical/heredoc 처리 재사용; false call 방지 |
| GR-12 | P2, 조건부 | Rust LSP/SCIP enrichment | identity/coverage/평가 이후; 설치된 도구·bounded supervisor·fallback |
| GR-13 | P2, 조건부 | 한국어·본문 hybrid 검색 실험 | GR-06/07과 PERF-04; measured gain 전 새 의존성 채택 보류 |

### 호출 가능한 기능 통폐합 큐

[CC 상세 패킷](CALLABLE-CONSOLIDATION-WORK-PACKETS.md)은 기존 함수의 실제 caller,
제거 가능한 중복과 보존할 adapter 계약을 지정한다. CC-02/04의 P0는 즉시 손볼
consumer/callable 결함을 뜻하며 LC 작업의 예약·cleanup 우선순위를 대체하지 않는다.

| ID | 우선순위 | 구체적 결과 | 선행 / 보존할 조건 |
|---|---|---|---|
| CC-02 | P0 | 전체 catalog를 routing에 전달 | 표시용 20개 limit 유지 가능; legacy/empty/unknown 구분 |
| CC-04 | P0 | 광고된 output reader 등록과 canonical reference 연결 | 기존 capture core; SHA·root·권한·withhold·응답 cap 보존 |
| CC-01 | P1 | provider identity/alias/discovery 공유 | target 문법·account·권한은 caller에 유지; REL-02/03 writer 조정 |
| CC-05 | P1 | hook ownership/convergence의 실제 owner 하나 | absolute native/legacy corpus 먼저; lifecycle transaction 유지 |
| CC-06 | P1, 단계별 | Plan Git → graph → task supervisor 재사용 | stdin·live output·signal·tail·cleanup 동등성; REL-04/AR-01 조정 |
| CC-03 | P1 | account decode/validation 공유 | preview/execute의 error·선택·Claude default 정책 유지 |
| CC-10 | P1 | CLI/MCP/skill 설명·owner·parser 정합성 | discovery와 권한 분리; CC-04/09 지원 상태 반영 |
| CC-07 | P2 | lock cancellation watcher thread 제거 | 기존 lock/release/timeout 의미 유지 |
| CC-08 | P2 | current-source verification 집계 공유 | raw path·state/work root·출력·cancel adapter 유지 |
| CC-09 | P1, host 연결 별도 | native Main swap 지원 계약과 host 연결 | unsupported 표시 우선; LC cleanup·lineage·기존 core 재사용 |

MCP symbol slice와 graph tool 확장은 GR-01/03/06의 기존 producer에 연결한다.
새 parser나 provider별 실행기를 별도 패킷으로 중복 추가하지 않는다.

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

> 최신 main과 관련 코드 PR을 확인하고, 문서가 아닌 실제 구현이 완료된 패킷은 건너뛰세요.
> 이 조사 시점 #259~#264는 문서로 병합되었고 #258은 draft입니다.
> 우선 #259의 LC-01/02와 native assignment 연결을 유지하면서 VW-01, PERF-02, REL-01,
> REL-02를 각기 좁은 코드 PR로 진행하세요. 같은 파일의 writer는 한 명으로 고정합니다.
> 기존 Plan/Room/peer/supervisor/usage/landing과 canonical fixture를 재사용하세요.
> REL-01 다음에 PERF-01, REL-02 다음에 cooldown 정책, AR-01/02 다음에 AR-03을 연결하세요.
> Graph는 GR-06의 기준선을 고정하며 GR-03/04 전달 계약, GR-01/02 Rust 정확성을 먼저 보완하세요.
> GR-05의 source/engine identity를 PERF-04에 연결하고 GR-12/13은 평가 후 선택하세요.
> 호출 경로는 CC-02의 catalog consumer와 CC-04의 output reader부터 작은 코드 PR로 연결하세요.
> CC-01/03은 REL-02/03과 같은 provider writer를 조정하고 CC-06은 Plan → graph → task로 나누세요.
> CC-05는 actual installer corpus, CC-09는 unsupported 표시와 실제 host handshake를 먼저 다루세요.
> CC-07/08의 순수 중복 제거와 CC-10의 callable 정합성은 해당 adapter 계약을 보존하세요.
> 기능의 존재, 실제 public caller 연결, fixture 통과, live provider 증거를 구분해 보고하세요.
> 빠진 usage·cleanup·관측을 성공이나 0으로 채우지 마세요. 성능은 동일 workload의 bytes,
> metadata/subprocess/fsync, lock hold, RSS, p50/p95를 cold/warm으로 나눠 비교하세요.
> 새 hosted CI job/matrix/schedule, 유료 모델/GPU 실행, storage upload를 기본으로 추가하지 마세요.
> Scope가 작은 기존 검사를 실행하고, runtime/permission/state 경계를 바꾸면 저장소의 required gate를 지키세요.
> 각 PR에 실제 수정·재사용·실행한 검사·남은 제한을 적고 검토를 위해 열어 두세요.

이 전달문은 구현 범위의 지시이며 merge·설치·배포·계정 변경·데이터 삭제를 승인하는 문서가 아니다.
AR-04는 정리 기능을 구현하는 작업이고 실제 사용자 cache 삭제는 별도 명시된 요청이 있어야 한다.

## 6. 이번 조사에서 추가하지 않은 중복 작업

- Reader rendered-line cache, graph parser cache, Room append reader는 이미 있다.
- Account cooldown 기록·pinned account·한정된 fallback·campaign admission·physical receipt도 이미 있다.
- Feature branch push와 PR의 이중 CI trigger는 현재 workflow에 없다.
- SSH 단일 파일 resume, snapshot, remote operation ledger를 새로 만들지 않는다.
- Panel dispatch → peer, graph/peer → context planner, task/plan/runtime/panel → work-inspect는 이미 연결되어 있다.
- Task/Plan, capture/peer-result reference, preview/execute account 선택, 두 JSON 표현은 각 계약을 보존한다.
- Python gate helper, 별도 검색 서비스, provider별 task DB나 상주 fleet scheduler를 복원·추가하지 않는다.

새 명세는 source와 nearest fixture, 공식 provider/GitHub/OpenSSH 문서를 대조해 작성했다.
새 문서가 baseline에서 독립적으로 링크되는지와 문서 참조·diff 공백을 검사했다.
Production 성능·실제 청구·실 vendor·SSH/GPU·최소 지원 OS 실행은 측정하지 않았다.
