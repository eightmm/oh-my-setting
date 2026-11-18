# Graph retrieval 구현 작업 패킷

기준: 2026-11-18, main `f9a695ea2de90a160754252c1ee9cbaa4dd7a488`.
[비교 분석](GRAPH-RETRIEVAL-COMPARISON.md)의 소스 근거를 구현 가능한 단위로 나눈다.
**이 문서의 GR-01–13은 미구현 제안이다.** P0는 현재 사용 경로의 정확성·평가를 먼저
확보할 작업, P1은 이어서 보완할 범위, P2는 측정 후 선택할 실험이다.

## 작업 순서와 소유권

| ID | 우선순위 | 결과 | 주 변경 지점 | 선행·연계 |
|---|---|---|---|---|
| GR-01 | P0 | Rust scope별 ID, body span, signature | `oms-graph::polyglot`, graph API fixture | GR-06 fixture와 함께 |
| GR-02 | P0 | Rust module 경로와 `#[path]` | `polyglot`, resolver | GR-01과 같은 파일이므로 순차 |
| GR-03 | P0 | Worker까지 typed retrieval 상태·snapshot 근거 전달 | peer delegate/context pack, panel caller | GR-04와 interface 합의 |
| GR-04 | P0 | Parse·resolve·query coverage 보존 | parser result, builder, context projection | GR-03 consumer와 동시 설계 |
| GR-05 | P1 | Freshness 등급과 engine identity | graph build/cache/projection | #260 PERF-04의 cache key보다 먼저 |
| GR-06 | P0 | 재현 가능한 품질 평가 | 기존 benchmark·engine·peer fixtures | 다른 작업과 병렬로 baseline 고정 |
| GR-07 | P1 | 필요한 symbol을 먼저 싣는 budget projection | signature map, peer renderer | GR-01/03/06 |
| GR-08 | P1 | Rust test metadata의 retrieval 재사용 | graph index/context | GR-01/04; test-run narrowing은 별도 |
| GR-09 | P1 | Notebook code-only 검색과 일관된 provenance | notebook adapter, source view | GR-04/06 |
| GR-10 | P1 | Python lexical binding·nested identity 교정 | Python extract, resolver | GR-04/06; legacy 의미 변경 명시 |
| GR-11 | P1 | Shell command 위치에 근거한 path-call | shell parser, resolver fixture | GR-04/06 |
| GR-12 | P2 | 선택적 Rust semantic enrichment | 기존 parser/graph seam, supervisor | GR-01–06의 identity·coverage 이후 |
| GR-13 | P2 | 한국어·본문 질문의 hybrid 비교 실험 | 기존 retrieval/ranking seam | GR-06/07, #260 PERF-04 |

현재 [#260 PERF-04](https://github.com/eightmm/oh-my-setting/pull/260)는 graph lexical
전처리 재사용 작업을 이미 정의한다. 새 cache/store 작업을 중복 생성하지 않는다.
GR-05는 invalidation/validation 계약, GR-06은 평가, PERF-04는 결과를 보존하는 최적화다.
Provider lifecycle 작업은 [#259](https://github.com/eightmm/oh-my-setting/pull/259)에
유지한다. Graph 개선은 Team 예약·취소·정산·acceptance 누락을 해결한 것으로 세지 않는다.

## GR-01 — Rust symbol identity와 source span

**문제와 재사용:** [polyglot][polyglot]의 `symbol:{path}::{name}`과
[resolver][resolve]의 last-write overwrite로 다른 impl의 동명 method가 합쳐진다.
`end_line`은 이름 토큰 줄이며 signature가 없다. 기존 node/qualname, `file_api`,
search block ownership과 source evidence 형식을 재사용한다.

**구현:** module/impl/trait scope와 symbol kind를 포함한 결정적 identity, 선언 signature,
본문 끝 span을 공급한다. Bounded syntax parser 선택은 이 경계 안에서 검토하며 새로운
type-checker를 직접 만들지 않는다. 불완전 source·macro/조건부 항목은 해석 범위를 표시한다.
ID 의미가 바뀌면 parser/build identity를 함께 올려 이전 cache와 섞이지 않게 한다.

**완료 조건:** 다른 impl의 `new`, nested module의 동명 함수, trait/inherent impl,
generic/async/lifetime/raw string과 불완전 source를 구분한다. 기존
[parser_contract][parser-tests]에 fixture를 추가하고 [project_engine][engine-tests]에서
JSON/SQLite 조회 parity, Rust signature 표시, 함수 안 body hit의 owner를 확인한다.
`process.rs`의 네 `new`가 사라지지 않는 case를 포함한다. Exact lookup과 ambiguous name
조회는 별개이며 임의의 한 정의를 정답으로 선택하지 않는다.

## GR-02 — Rust module 관계

**문제와 재사용:** [polyglot][polyglot]의 outlined module 후보는 현재 파일 디렉터리에만
의존한다. `#[path]`, inline scope, non-`mod.rs` 하위 module을 놓친다. 기존 safe path
정규화와 ref evidence, resolver confidence를 재사용한다.

**구현:** [Rust reference](https://doc.rust-lang.org/reference/items/modules.html)의
root/`mod.rs`/일반 파일·inline module·`#[path]` 규칙을 반영한다. 지원하지 않는
`cfg_attr`, macro-generated path, custom Cargo target/feature 해석은 unknown으로 남긴다.
Cargo path/workspace 추출을 새로 만들지 않는다. Cargo의 `file:` root는 builder가 이미 만든다.

**완료 조건:** 실제 target과 잘못된 sibling 파일이 모두 존재해도 false `EXTRACTED` edge가
없다. OMS `project.rs`의 attribute 기반 module 선언을 fixture로 재현한다.
Parser/resolver canonical fixture를 확장하고 full rebuild와 incremental 결과를 비교한다.
Module 관계 개선을 Rust call graph 또는 Cargo test dependency closure 완성으로 표시하지 않는다.

## GR-03 — Worker retrieval bridge

**문제와 재사용:** [delegate][delegate]의 `build_graph_context`는 빈 성공 결과와 오류를
합쳐 Python 제외 rebuild를 시도한다. JSON `retrieval/truncated/omitted`는 버리고
일반 overview도 task signatures로 출력한다. [context][context]의 기존 JSON과
[explicit pack adapter][pack]를 하나의 typed projection 계약으로 연결한다.

**구현:** match/no-match/partial, parser unavailable, malformed/error, cancellation,
generic fallback을 구분한다. 정상 no-match만으로 discovery policy를 바꾸지 않는다.
진단용 stderr를 통째로 prompt에 붙이는 대신 bounded 상태·이유를 전달한다.
명시적 pack 우선순위와 일반 delegate의 opt-in을 보존하고 panel Delegate caller를 검증한다.
Graph exec의 `ready`도 retrieval 성공 여부와 별도 상태로 설명한다.

현재 graph 준비는 parent repo에서 worker worktree 생성 전에 실행된다. Source revision과
선택 파일 digest를 detached snapshot에 맞춰 검증하거나 그 snapshot에서 준비한다.
기존 [compiled context][compiled]의 bundle/manifest identity를 재사용하며 별도 state store는 만들지 않는다.

**완료 조건:** `peer_contract::peer_dry_runs_and_ml_context`를 확장해 no-match 전후
Python coverage·manifest policy 보존, helper 오류, default off, explicit pack 우선,
dirty parent/clean worker 차이를 확인한다. 기존
`delegate_context_manifest_binds_snapshot_bundle_across_repair`와
`native_delegate_context_manifest_reaches_the_physical_provider_boundary`의 fake provider
capture를 확장해 실제 prompt의 상태·source identity·digest를 검증한다. 유료 호출은 필요 없다.

## GR-04 — Parse·resolve·query coverage

**문제와 재사용:** [Python model][py-model]은 `syntax_valid`를 이미 생산한다.
[generic conversion][registry]은 이를 버리고 [build coverage][build]는 지원 파일 수를 센다.
Unresolved ref는 [resolver][resolve]에서 결과 edge 없이 사라질 수 있다. Context rank는
source를 읽지 못해도 quoted field만 빼고 이름·summary ranking을 계속한다.

**구현:** 기존 result와 manifest projection을 확장해 valid empty/invalid/unsupported/
partial/infrastructure failure를 구분한다. 언어별로 declaration, body span, import,
call/reference의 지원 수준을 드러낸다. Unresolved external/internal/unsupported 구분은
증거가 있을 때만 하고, 불명확한 항목은 unknown으로 남긴다.
Source read/stale/skipped 관측과 omission을 context/signature/explicit pack까지 전달한다.
새 field와 schema version은 producer·consumer·legacy reader를 함께 변경한다.

**완료 조건:** 잘못된 Python syntax, 유효한 빈 파일, helper 중단, unknown extension,
partial notebook, stale source, zero match를 서로 다르게 보고한다. `parsed=1`을 semantic
resolution 완료로 읽지 않는다. 기존 helper direct/wire fixture, parser, engine, peer/MCP
fixture를 확장하고 오래된 manifest는 coverage unknown으로 처리한다.

## GR-05 — Freshness와 engine fingerprint

**문제와 재사용:** [build][build]는 size/inode/mtime이 같으면 digest를 재사용한다.
기존 [engine fixture][engine-tests]가 same-stat 변경의 `fresh=true`를 고정한다.
Per-file cache에는 parser version이 있지만 자동 freshness와 graph revision은 global
version/schema에 의존한다. 개별 parser만 바꾸는 향후 upgrade에 대한 무효화 계약이 약하다.

**구현:** stat-reused와 byte-verified의 근거를 구분한다. 필요한 consumer에 bounded
재검증 또는 기존 force 경로를 연결한다. Parser registry, resolver, projection의 engine
identity를 manifest/revision/cache key와 결합한다. 이 field 이름과 schema는 구현 PR에서
확정한다. 기존 safe reads, build lock, atomic graph publication과 immutable reader를 유지한다.
서로 다른 worktree/dirty generation은 HEAD가 같다는 이유만으로 cache를 공유하지 않는다.

**완료 조건:** 같은 stat로 bytes만 바뀜, metadata 누락, parser만 upgrade, rename/delete,
중단된 publication, concurrent reader가 정확한 freshness basis를 받는다.
기존 shortcut 정책을 바꾸는 경우 그 의미 변경을 fixture와 문서에 명시한다.
Corrupt cache row의 miss와 DB 자체의 open/schema 실패를 구분한다. Regenerable DB 복구를
추가한다면 이전 완전한 graph를 보존하고 실패·rebuild 상태를 숨기지 않는다.

## GR-06 — retrieval evaluation

**재사용:** [history benchmark][benchmark], [analytics][analytics],
[frozen lexical reference][lexical-reference], [engine fixture][engine-tests]를 확장한다.
현재 parity fixture는 작은 node 집합과 lexical 결과를 고정하며 graph 확장의 품질 corpus가 아니다.
기존 history recall 정의와 snapshot은 유지한다.

**Corpus 계약:** repo SHA, query/gold digest, relevance role, budget, language, source availability를
고정한다. Parser/ranking/renderer version은 baseline/candidate별로 기록하며 미리 선언한
treatment의 변경은 허용한다. Gold는 primary definition, 필요한
caller/test, 유용한 맥락, decoy를 구분한다. Added/deleted/unsupported 경로의 수와
`min(k, parent에서 찾을 수 있는 gold 수) / 전체 gold 수` 상한은 보조 정보로 보고한다.
지원이 약한 언어를 분모에서 지워 높은 점수를 만드는 방식은 쓰지 않는다.

| 묶음 | 질문·경계 | 목적 |
|---|---|---|
| 기존 parity | 현재 find/context golden과 history snapshots | 의도하지 않은 결과 변화 감지 |
| Static adversarial | 동명 Rust impl, module path, Python shadowing, shell inert argument, notebook output-only marker | False fact·identity·anchor 오류 방지 |
| 작업 질문 | exact identifier, 영어/한국어 동등 질문, paraphrase, body-only clue, caller/test, negative/no-match | 실제 retrieval 품질 |
| 전달·갱신 | late symbol/작은 budget, dirty worktree, rename/delete, stale/no-refresh, unavailable helper | 최종 출력·freshness·fallback |

아래 여섯 쌍은 source를 확인한 **개발용 seed**다. 현재 답을 찾았다는 검색 실행 결과나
독립 held-out corpus가 아니다. 구현 전에 label을 검토·고정하고 별도 holdout을 추가한다.

| ID | 한국어 / 영어 query | Primary source |
|---|---|---|
| Q1 | 같은 usage receipt의 중복 집계와 no-op write를 어디서 막는가? / Where are duplicate usage receipts deduplicated and no-op writes skipped? | [usage::record](../crates/oms-command-telemetry/src/store/usage.rs); write 경계는 [atomic_bytes](../crates/oms-safe-fs/src/atomic_bytes.rs) |
| Q2 | Panel worker의 빈 graph 검색은 왜 Python 제외 rebuild를 하는가? / Why does an empty graph-context lookup rebuild with Python excluded? | [build_graph_context][delegate]; caller는 [panel dispatch](../crates/oms-panel-core/src/dispatch.rs) |
| Q3 | 동명 Rust method의 정의와 caller/test를 어떻게 구분하는가? / How are same-named Rust methods and their caller/test links returned? | [ReadGraph::symbol][index]; extractor 오류 fixture는 별도 |
| Q4 | Direct target의 관련 source/test를 찾고 digest를 재검증하는 경로는? / How does runtime context discover related sources/tests and revalidate their digests? | [context_dependencies_with_groups](../crates/oms-command-graph/src/project.rs); caller는 runtime context |
| Q5 | Parent에 없는 신규 파일도 history gold에 포함되는가? / Does history recall count added files absent from the parent tree? | [history_cases/run][benchmark] |
| Q6 | 선택 파일 뒤쪽의 관련 함수가 signature budget 밖으로 빠질 수 있는가? / Can a relevant function near a selected file's end be omitted by the signature budget? | [signature_map][context]; 선언 순서는 [file_api][index] |

**측정:** entry/file recall@8, hit@1, MRR, 필요 시 graded nDCG, shown-symbol recall,
edge precision/recall, valid source anchors, coverage/fallback/omission을 분리한다.
`rg`는 literal coverage baseline으로 사용하고 semantic rank를 제공하는 것처럼 비교하지 않는다.
Exact query와 자연어 query의 결과를 같은 성공 정의로 합치지 않는다.

**비용:** 고정 host/filesystem에서 cold/warm/one-file edit를 나누고 baseline/candidate를
교차 실행한다. Query latency와 build latency, p50/p95, peak RSS, metadata/read/hash/parse
수는 PERF-04와 연계한다. Timeout/error를 빠른 성공 sample로 세거나 분포에서 삭제하지 않는다.

**완료 조건:** corpus·통제 조건이 다르면 snapshot 비교를 거부한다. 의도한 treatment 차이는
실행별 version과 함께 보고한다. 동일 budget과 case로 paired 결과·regression을 보고하며
correctness fixture의 false authoritative edge와 잘못된 anchor는
허용하지 않는다. 숫자 성능 목표는 baseline 측정 후 tuning 전에 고정한다.
CI에는 작은 결정적 fixture만 추가하고 반복 성능·외부 제품 비교는 opt-in이다.

## GR-07 — 질문에 맞는 signature budget

**문제와 재사용:** [signature_map][context]은 선택 파일의 [file_api][index]를 source 순서로
출력한다. 첫 파일에서 잘리면 이후 파일을 생략한다. 기존 `entries`와 `evidence.reason`에
남아 있는 winning symbol, `truncated/omitted`와 byte counter를 재사용한다.

**구현:** 매칭된 선언과 직접 필요한 증거를 먼저 배치하고 file별 공정한 budget을 검토한다.
관련도 변경과 renderer 순서 변경의 효과를 분리한다. 큰 단일 signature도 명시적으로
생략할 수 있어야 하며 bytes/token을 혼동하지 않는다. 본문은 필요할 때 별도 bounded 조회한다.

**완료 조건:** 긴 파일 마지막의 hit, 다음 선택 파일, UTF-8, budget보다 큰 선언을 canonical
engine/peer fixture로 검증한다. 선택한 파일의 hit와 실제 표시된 symbol의 hit를 따로 기록한다.
기존 정확한 `--entry-path` 선택 의미, deterministic tie-break, total byte cap을 보존한다.

## GR-08 — Rust test 분류

**문제와 재사용:** [polyglot][polyglot]의 `metadata.test`는 있지만 [index][index]의
`is_test`와 `blast.test_cases`는 file kind/경로 및 `test_` 이름에 의존한다.
기존 test metadata와 path/node index를 연결한다.

**완료 조건:** `#[test] fn check()`, `#[tokio::test]`, `src/lib.rs`의 unit test,
일반 함수 `test_adapter`, 동명 test를 구분한다. Unit test가 있는 production file 전체를
test-only로 만들지 않는다. `rust_workspace_cache_and_impact`를 확장한다.
Cargo target/feature별 test 실행과 [changed-hunk narrowing](../crates/oms-command-graph/src/project_cases.rs)은
별도 계약이다. Retrieval에서 test를 찾았다는 이유로 기존 full verification fallback을 축소하지 않는다.

## GR-09 — Notebook code-only view와 좌표

**문제와 재사용:** [notebook][notebook]에는 code-cell 추출·syntax validity·skip count와
top-level physical line remap이 있다. Raw notebook은 body search에서 제외되며 nested
data/callable contract의 evidence/field/parameter line은 joined-cell 좌표에 남는다.

**구현:** 기존 code-cell extractor를 source view로 공유하고 raw output/Markdown을 검색에
섞지 않는다. Cell ID/index, cell-local line, physical JSON line의 뜻을 구분해 모든 anchor를
같은 계약으로 변환한다. Source view와 cache key에는 notebook digest와 extractor version을 넣는다.

**완료 조건:** 여러 cell, source 배열/문자열, pretty JSON, magic·비 Python cell,
annotated class/function의 nested provenance를 확인한다. Output-only marker는 검색되지 않아야 한다.
현재 tree에서 전용 native notebook fixture를 찾지 못했으므로 기존 graph test harness 안에
해당 경로의 canonical fixture를 추가하고, retired Python suite의 항목을 실행 증거로 인용하지 않는다.

## GR-10 — Python lexical scope 교정

**문제와 재사용:** [extract][py-extract]는 subtree import binding을 모으고 import 이름을
parameter/local보다 먼저 처리한다. [resolver][resolve] import map은 파일 단위이다.
Ruff AST와 기존 declaration/ref 형식을 이용하되 lexical scope를 ref까지 유지한다.

**완료 조건:** `from a import foo` 뒤 `def f(foo): foo()`가 외부 `a.foo` call로 연결되지 않는다.
함수별 동명 local import, nested function/class 동일명, method 내부 nested function,
`global/nonlocal`을 [Python scope 규칙](https://docs.python.org/3.12/reference/executionmodel.html#resolution-of-names)에
맞추거나 unsupported로 명시한다. 기존 [Python contract fixture][py-tests]와
[resolve fixture][resolve-tests]를 확장한다. Legacy golden의 일부는 같은 부정확한 동작을
고정하므로 의도한 semantic correction과 cache/ID 변경을 PR에 명시한다.

## GR-11 — Shell path-call 정확성

**문제와 재사용:** [shell parser][shell-parser]는 raw scan의 path regex hit를 call ref로
만든다. `echo "scripts/helper.sh"` 같은 data argument가 실행 관계로 연결될 수 있다.
기존 `shell_code`, `command_names`, wrapper와 heredoc 처리를 재사용한다.

**완료 조건:** quoted executable, `bash path`, ordinary argument, comment,
command substitution, heredoc, case pattern을 구분한다. Quotes를 모두 지워 정상 command도
놓치는 방식은 피한다. 모호한 경로를 실행 사실로 승격하지 않는다.
[shell contract](../crates/oms-graph-shell/tests/graph_contract.rs)와 lexical fixture를 확장하고,
긴 줄·지원하지 않는 구문의 생략은 GR-04 coverage로 전달한다.

## GR-12 — 선택적 Rust semantic enrichment

Serena의 LSP 탐색과 Sourcegraph의 SCIP occurrence/snapshot 방식을 참고한다.
**조건부 실험**이며 mandatory daemon/새 DB 도입은 범위 밖이다.
기존 graph parser seam, source evidence와 [supervisor](../crates/oms-supervisor/src/process.rs)를 재사용한다.

설치된 rust-analyzer 또는 검토한 SCIP producer 하나부터 query/snapshot adapter를 비교한다.
Tool version, Cargo target/features, source generation, output bounds, deadline/cancellation,
process ownership을 기록한다. Proc macro/build-script 실행 정책도 분리해 명시한다.
도구 없음·부분 index·실패는 lexical/syntax fallback으로 표현하며 자동 설치를 전제하지 않는다.

**완료 조건:** trait/impl, import alias, 동명 method에서 실제 edge precision/recall 개선을
GR-06 corpus로 보인다. Semantic `reference`를 실행된 `call`로 바꾸지 않는다.
Sourcegraph 공식 표의 Rust cross-repository 제한을 고려해 외부 crate까지 완전하다고 하지 않는다.
새 semantic edge를 `affected`의 test 생략 증거로 승격하려면 별도 검증이 필요하다.

## GR-13 — 한국어·본문 검색 개선 실험

현재 Unicode 보존과 한국어 token 일치 fixture는 한영 의미 검색을 증명하지 않는다.
먼저 GR-06에서 body-only/동의어/한영 쌍의 실패를 분류한다. 현재 exact lookup,
`search --ranked`, `context`, `--entry-path`를 baseline으로 유지한다.

**실험 순서:** 기존 body search의 후보 재사용과 query expansion을 각각 평가한 뒤,
필요하면 optional local embedding + lexical fusion을 별도 비교한다.
Exact identifier/path의 강한 증거를 유지하고 original/expanded query와 rank 근거를 보존한다.
Model/tokenizer/version, source/span digest, embedding generation을 cache identity에 결합한다.
Unavailable model과 stale vector는 lexical 결과와 degraded 이유로 반환한다.

**완료 조건:** 동일 held-out 질문·byte budget에서 한국어 포함 strata별 recall/MRR와
shown-symbol 품질을 비교하고 exact lookup 회귀, latency/RSS/설치 비용을 함께 보고한다.
Provider translation·embedding 비용은 자동 CI에 넣지 않는다. 개선이 확인되지 않으면
새 의존성 채택을 보류한다. GitNexus/CGC의 제품 설명은 OMS의 measured outcome이 아니다.

## 구현 PR에 공통으로 필요한 증거

작업 시작 시 main과 관련 open PR을 다시 확인한다. 같은 파일을 여러 agent가 동시에 소유하지
않도록 위 표대로 분할한다. Fixture를 먼저 기존 canonical suite에 붙이고, 실제 오류 경로와
downstream consumer를 함께 고친다. 영향받는 Rust crate와 관련 integration checks만 실행한다.
정확한 commit의 CI 결과와 로컬에서 실행하지 못한 항목을 분리해 기록한다.

이 비교 작업에서는 Rust toolchain/native OMS payload가 없어 Rust tests와 retrieval benchmark를
실행하지 않았다. 문서 검증만으로 위 패킷의 correctness·성능 완료를 선언할 수 없다.

[polyglot]: ../crates/oms-graph/src/polyglot.rs
[registry]: ../crates/oms-graph/src/lib.rs
[resolve]: ../crates/oms-graph-resolve/src/lib.rs
[build]: ../crates/oms-command-graph/src/project_build.rs
[index]: ../crates/oms-command-graph/src/project_index.rs
[context]: ../crates/oms-command-graph/src/project_context.rs
[notebook]: ../crates/oms-command-graph/src/notebook.rs
[delegate]: ../crates/oms-command-peer/src/delegate.rs
[pack]: ../crates/oms-command-peer/src/context_pack.rs
[compiled]: ../crates/oms-command-peer/src/compiled_context.rs
[benchmark]: ../crates/oms-command-graph/src/project_benchmark.rs
[analytics]: ../crates/oms-command-graph/src/project_analytics.rs
[lexical-reference]: ../crates/oms-command-graph/tests/project-engine-reference.json
[engine-tests]: ../crates/oms-command-graph/tests/project_engine.rs
[parser-tests]: ../crates/oms-graph/tests/parser_contract.rs
[resolve-tests]: ../crates/oms-graph-resolve/tests/resolve_contract.rs
[py-model]: ../crates/oms-graph-python/src/model.rs
[py-extract]: ../crates/oms-graph-python/src/extract.rs
[py-tests]: ../crates/oms-graph-python/tests/graph_contract.rs
[shell-parser]: ../crates/oms-graph-shell/src/parser.rs
