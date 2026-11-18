# Graph 저장·검색·context 품질 비교

확인일: 2026-11-18. OMS 기준은
[`f9a695ea2de90a160754252c1ee9cbaa4dd7a488`](https://github.com/eightmm/oh-my-setting/commit/f9a695ea2de90a160754252c1ee9cbaa4dd7a488)다.
이 문서는 소스와 fixture를 대조한 비교 분석이다. 외부 제품 설치, OMS 검색 실행,
검색 정확도·latency 측정, provider 호출은 수행하지 않았다.
아래 구현 제안은 [GR-01–13 작업 패킷](GRAPH-RETRIEVAL-WORK-PACKETS.md)에 연결된다.

## 1. 현재 충분한가

**로컬 탐색과 제한된 context를 만드는 기반은 있다. Rust 정의·참조·영향 분석을
완전하게 수행하거나 한국어 자연어 질문을 충분히 잘 찾는다고 판정할 근거는 부족하다.**
실제 소스에서 Rust symbol 충돌, module 경로 누락, worker에 전달되는 검색 상태 손실을
확인했다. 우선순위는 이 정확성 문제와 품질 측정이다. DB 엔진 교체의 필요성은 아직 입증되지 않았다.

| 사용 목적 | 현재 판정 | 근거와 사용 경계 |
|---|---|---|
| 알려진 이름·경로·문자열에서 탐색 시작 | 사용 가능한 기반 | `find`, `symbol`, `api`, literal/ranked `search`가 구현되어 있다. 원문과 coverage를 함께 확인한다. |
| 관련 파일·테스트를 제한된 pack으로 전달 | 조건부 | lexical seed와 graph 확장, byte budget은 있다. 맞는 파일을 골라도 필요한 symbol이 출력에서 빠질 수 있다. |
| Rust의 정확한 caller/trait/impl 탐색 | 보완 필요 | flat-token extraction이며 Rust call ref를 생산하지 않는다. 동명 method ID와 module 경로도 불완전하다. |
| Python/shell 관계 분석 | 보완 필요 | Python AST와 shell extractor가 있지만 lexical binding·command 위치에 따른 잘못된 edge 가능성이 남는다. |
| 한국어 의미 검색 | 미검증·기능 제한 | Unicode는 보존하지만 번역·embedding 검색은 없다. 영어 식별자와 공유하는 단어가 없으면 lexical no-match가 자연스럽다. |
| 대규모·여러 worktree의 상시 검색 | 성능 판정 보류 | 증분 extraction은 있으나 전체 resolve/projection 및 query별 전처리 비용은 실측하지 않았다. |
| 검색 결과로 검증 범위 축소 | 기존 보수적 경계 유지 | `affected`의 full fallback을 보존한다. 새 heuristic/semantic edge를 곧바로 test 생략 근거로 쓰지 않는다. |

기준 commit의 Git tracked path inventory는 1,069개이며 `.rs` 574개,
`.sh/.bash` 138개, `.py` 2개다. 이는 `git ls-tree -r --name-only -z` 결과의
확장자 집계이며 parser coverage나 검색 성공률이 아니다. 이 저장소 자체의 탐색 품질에서도
Rust 정확성은 주변 기능이 아니다.

## 2. 이미 있는 기능과 재사용 지점

| 계층 | 현재 구현 | 평가에서 구분할 점 |
|---|---|---|
| Discovery·extraction | [project_build](../crates/oms-command-graph/src/project_build.rs)의 safe read, 크기 제한, stat/digest 확인, `cache.sqlite3` per-file cache | 변경 파일의 재추출과 전체 graph 재구성은 다르다. 전체 `resolve`와 JSON/SQLite publication은 여전히 수행한다. |
| 저장·질의 | `graph.json`, `manifest.json`, [immutable index.sqlite3](../crates/oms-command-graph/src/project_sqlite.rs), in-memory CSR/path indexes | `symbol/api/neighbors/trace`는 SQL fast path가 있다. SQLite를 전혀 안 쓰거나 매 query마다 모두 파싱하는 구조가 아니다. |
| 언어 추출 | [parser registry](../crates/oms-graph/src/lib.rs), [polyglot](../crates/oms-graph/src/polyglot.rs), Ruff helper, shell parser, [notebook adapter](../crates/oms-command-graph/src/notebook.rs) | 지원 확장자·구문 추출·이름 해석·실제 실행 의미는 서로 다른 수준이다. |
| 관계 근거 | [resolver](../crates/oms-graph-resolve/src/lib.rs)의 `EXTRACTED/INFERRED/AMBIGUOUS`, source line/digest | confidence가 있다는 것과 해당 분류가 항상 정확하다는 것은 다르다. 해석되지 않은 ref의 진단도 필요하다. |
| Context | [context selector](../crates/oms-command-graph/src/project_context.rs), `--entry-path`, bounded graph expansion, signature map, [runtime compiler](../crates/oms-runtime/src/context.rs) | pack의 file 선택, 출력된 symbol, provider가 실제 받은 bytes를 따로 평가한다. |
| 평가·UI | [history benchmark](../crates/oms-command-graph/src/project_benchmark.rs), [recall comparison](../crates/oms-command-graph/src/project_analytics.rs), [engine fixtures](../crates/oms-command-graph/tests/project_engine.rs), bounded viewer/MCP | benchmark와 viewer가 존재해도 실제 작업의 정답률·완전성·속도를 증명하지는 않는다. |

### 검색 표면은 하나가 아니다

| 명령 | 실제 입력·순위 기준 | 기대하면 안 되는 동작 |
|---|---|---|
| `find` / `symbol` / `api` | 이름·qualname·경로 탐색, 정의·관계·선언 조회 | compiler가 확인한 Rust reference resolution |
| `search` | 허용된 indexed source의 literal scan, bounded preview | 제외·stale 파일까지 포함한 저장소 전체의 무제한 완전 검색 |
| `search --ranked` | 본문을 symbol block으로 묶는 BM25 | 아래 context rank와 동일한 점수·입력 |
| `context --task` | 이름/qualname, 경로, summary, quoted string의 field BM25와 graph 확장 | 본문의 모든 identifier/comment가 자동으로 seed ranking에 들어가는 것, 한국어→영어 의미 변환 |
| `map --signatures` | context가 고른 파일의 선언을 byte budget에 맞춰 출력 | 관련 symbol을 먼저 보여준다는 보장 |

근거는 [project_index](../crates/oms-command-graph/src/project_index.rs)의
`search`·`file_api`와 [project_context](../crates/oms-command-graph/src/project_context.rs)의
`rank`·`select`·`signature_map`이다. Ranked body search는 context seed에 자동 연결되지 않는다.
Notebook raw JSON은 output 유입을 피하기 위해 body search와 quoted-body ranking에서 제외된다.

## 3. 실제 소스에서 확인한 우선 문제

### Rust 전환 후 정확성

[polyglot](../crates/oms-graph/src/polyglot.rs)은 symbol ID를 `path + name`으로 만들고,
[resolver](../crates/oms-graph-resolve/src/lib.rs)는 같은 ID를 마지막 node로 덮어쓴다.
실제 [process.rs](../crates/oms-supervisor/src/process.rs)의 `PhysicalCall::new`,
`Duplex::new`, `Request::new`, `Reactor::new`는 이 규칙에서 같은 ID가 된다.
또 `end_line`은 이름 토큰의 줄이고 Rust signature가 없어서 함수 본문 귀속과 API 표시에
영향을 준다. 이 결과는 소스 경로를 따라 도출했으며 CLI로 재현한 실행 결과는 아니다. **GR-01**.

`src/foo.rs`의 `mod bar;`는 현재 sibling `src/bar.rs`로 후보를 만들지만,
[Rust module 규칙](https://doc.rust-lang.org/reference/items/modules.html)은 논리 module
경로를 따른다. `#[path]`도 반영하지 않는다. OMS의
[project.rs](../crates/oms-command-graph/src/project.rs) 자체가 11개 module에 이 attribute를
사용한다. 잘못 추정한 sibling 파일이 실제 존재할 때의 false edge도 fixture로 막아야 한다. **GR-02**.

Rust `#[test]`/`#[tokio::test]`의 `metadata.test`는 생산하지만 `Graph::is_test`는
file/kind 위주이며 `blast.test_cases`는 `test_` 이름 관례를 요구한다.
`#[test] fn check()`나 source 내부 test를 일관되게 분류하는 작업이 남는다. **GR-08**.

### Orchestrator에서 결과가 달라지는 지점

[delegate::build_graph_context](../crates/oms-command-peer/src/delegate.rs)는 signature map을
text로 받아 stderr와 JSON metadata를 버린다. 정상 `code=0`이면서 결과가 비어도 실패와
같은 분기로 들어가 `build --exclude '*.py'`를 시도하고 일반 orientation으로 fallback한다.
따라서 no-match가 검색 실패·Python 제외·일반 개요와 구분되지 않을 수 있다. **GR-03**.

일반 `peer-delegate`의 graph 준비는 opt-in이지만
[panel dispatch](../crates/oms-panel-core/src/dispatch.rs)의 `Delegate` 분기는
`--graph-context`를 추가한다. Reviewer/advisor와
[직접 native orchestrator 시작](../crates/oms-command-panel/src/orchestrator.rs)은 별도 경로다.
명시적 `--context-pack`도 다른 경로로 처리된다. 이를 모든 provider 세션의 공통 동작으로
일반화하지 않는다. 현재 graph 생성은 parent repo에서, worker worktree 생성보다 먼저 이루어지므로
detached snapshot과의 source identity 검증도 필요하다.

`signature_map`은 파일별 `file_api` 결과를 선언 순서로 출력한다. 첫 파일이 budget을
다 쓰면 뒤의 관련 함수와 다른 선택 파일이 빠질 수 있다. JSON의 `truncated/omitted`를
worker text 경로가 전달하지 않는 문제와, 실제 선택 순위 문제를 나누어 고친다. **GR-07**.

### Parsed·fresh·EXTRACTED의 의미

Python helper에는 `syntax_valid`와 protocol v2가 **이미 구현**되어 있다. 그러나 generic
parser conversion은 그 값을 전달하지 않고 coverage는 discovery의 지원 파일 수를 센다.
Invalid syntax, 유효한 빈 파일, 부분 지원, unresolved reference를 구분해야 한다. **GR-04**.

현재 stat shortcut은 같은 size/inode/mtime이면 digest를 재사용한다.
기존 engine fixture도 바이트를 바꾸고 mtime을 복원한 뒤 `fresh=true`를 요구한다.
이는 의도된 최적화이며 항상 byte-verified라는 뜻은 아니다. 개별 parser version만 바꾸고
global build version을 그대로 두면 freshness/revision이 이를 검출하지 못할 가능성도 있다.
후자는 upgrade 계약의 위험이며 현재 설치된 graph에서 관측한 장애는 아니다. **GR-05**.

Python import map은 파일 단위이고 import binding이 parameter/local 판단보다 우선한다.
Imported name을 parameter로 가리거나 함수별 동명 import를 쓸 때 잘못 연결될 수 있다.
Shell은 command 위치 밖의 `echo "scripts/helper.sh"`도 path-call 후보로 추출할 수 있다.
두 경우 모두 단순 성능 개선과 분리해 semantic correction fixture가 필요하다. **GR-10/11**.

Notebook의 top-level span은 remap하지만 nested data/callable contract의 line은
joined-cell 좌표에 남는다. Raw output을 검색에 섞지 않는 code-only source view와
모든 anchor의 좌표 계약을 함께 보강한다. **GR-09**.

## 4. 비슷한 프로젝트와 비교

다음은 공식 문서와 구현 비교다. 별도 제품의 홍보 수치나 다른 corpus의 benchmark를
OMS 검색 성능으로 옮기지 않는다. 각 프로젝트의 기본 의존성을 추가하는 제안도 아니다.

| 대상 | 확인한 접근 | Rust·운영 한계 | OMS에 적용할 부분 |
|---|---|---|---|
| [Aider repo map](https://aider.chat/docs/repomap.html) | Tree-sitter tag, graph ranking, 대화 파일·식별자에 따른 personalization, 제한된 signature context | [Rust repo map 지원](https://aider.chat/docs/languages.html). 구문/이름 기반 map은 semantic resolution과 다르며 map budget은 상황에 따라 조정된다. | 파일 선택 이후에도 관련 symbol을 먼저 보존하는 GR-07 |
| [Serena](https://oraios.github.io/serena/01-about/025_features.html) | LSP 또는 JetBrains 기반 definition/reference/overview와 body retrieval | [Rust는 rust-analyzer](https://oraios.github.io/serena/01-about/020_programming-languages.html). Backend별 차이가 있고 dependency search/type hierarchy는 기능표상 JetBrains 전용이다. | 설치된 도구를 선택적으로 쓰는 GR-12, 검색 근거·backend availability 표시 |
| [Sourcegraph](https://sourcegraph.com/docs/code-search/features) + [SCIP](https://sourcegraph.com/docs/code-navigation/precise-code-navigation) | Text/symbol 검색과 precise navigation을 분리하고 precise 정보가 없으면 search 경로로 fallback | [공식 Rust 표](https://sourcegraph.com/docs/code-navigation/writing-an-indexer)는 cross-file 지원, cross-repository 미지원이다. 서비스·index 운영과 revision 관리가 필요하다. | Literal match, syntax edge, semantic reference를 구분하고 occurrence snapshot으로 검증 |
| [GitNexus][nexus-arch] | Tree-sitter와 언어별 resolver, local LadybugDB, optional embedding과 BM25/vector/RRF, incremental/watch | [CLI 문서][nexus-cli]에 Linux native glibc 2.34 floor가 있어 OMS Ubuntu 20.04 기본 의존성과 맞지 않는다. Graph/embedding readiness도 별개다. | Bounded expansion, incomplete/lower-bound 결과 표시, incremental/full 정합성; GR-13의 조건부 비교군 |
| [CodeGraphContext][cgc-readme] | Tree-sitter graph와 Cypher, 여러 graph backend, optional SCIP와 watch | Backend별 설치·메모리 비용이 있다. [Rust SCIP 실행 매핑][cgc-scip]은 확인했지만 command/image의 실제 성공은 미검증이다. | Storage와 semantic enrichment 분리, rename/delete 정합성, 재현 가능한 index generation |

확인한 immutable 구현 기준:

| 프로젝트 | 소스 기준 |
|---|---|
| Aider | [5dc9490 — repomap.py](https://github.com/Aider-AI/aider/blob/5dc9490bb35f9729ef2c95d00a19ccd30c26339c/aider/repomap.py) |
| Serena | [0cef825 — Rust adapter](https://github.com/oraios/serena/blob/0cef825f802d5323f8cb4bda3a5af52bf704770f/src/solidlsp/language_servers/rust_analyzer.py) |
| Sourcegraph Zoekt | [33ce1d0 — search engine](https://github.com/sourcegraph/zoekt/blob/33ce1d04d373d9c8dcdb8b0a67119dedb6345ccc/README.md); 제품 기능표는 확인일의 공식 문서 |
| GitNexus | [8783a91 — architecture][nexus-arch] |
| CodeGraphContext | [f09cf00 — README][cgc-readme] |

GitNexus의 graph/vector 구성이나 CGC의 backend 선택이 정확도를 자동 보장하지 않는다.
Serena의 공개 평가는 [자체 task 선택·평가 방법](https://oraios.github.io/serena/04-evaluation/010_methodology.html),
GitNexus는 [별도 evaluation harness](https://github.com/abhigyanpatwari/GitNexus/blob/8783a91a54fe80be3f09b79e7e88edbc148d1538/eval/README.md)를
사용한다. 이 조사에서는 어느 결과도 OMS와의 동일 조건 성능 비교로 사용하지 않았다.

## 5. Graph DB에 대한 결정

**현재 SQLite/JSON을 유지하고 source·retrieval 계약부터 보완한다.** 이유는 저장과
bounded graph traversal이 이미 있으며, 확인한 ID 충돌·잘못된 edge·body 누락·worker 상태 손실은
저장 엔진을 교체해도 그대로 남기 때문이다. 이 판단은 설계상의 우선순위이며 SQLite가
대규모 workload에서도 충분히 빠르다는 실측 결론은 아니다.

| 선택 | 채택 조건 |
|---|---|
| 기존 `cache.sqlite3`·`index.sqlite3` 재사용 | 기본 경로. [#260 PERF-04](https://github.com/eightmm/oh-my-setting/pull/260)의 query-independent lexical 전처리 재사용을 GR-05/06과 연결한다. |
| Bounded rust-analyzer/SCIP adapter | GR-12. 실제 ambiguity 사례 개선, source/engine identity, unavailable·timeout fallback이 입증될 때 선택한다. |
| Local hybrid retrieval | GR-13. 한국어·동의어·본문 질문의 구체적 실패를 같은 corpus·budget에서 개선할 때 선택한다. |
| 별도 property-graph DB / 상시 service | 여러 저장소의 반복 질의·동시 reader·복잡한 traversal이 기존 구조의 측정된 한계를 넘을 때 별도 설계한다. 현재 default로 도입할 근거는 부족하다. |

후보 DB 평가에서도 재색인 비용, cold/warm p50/p95, peak RSS, 저장 크기, concurrent
reader/writer, crash 복구와 지원 OS를 함께 본다. 이미 있는 source provenance와
authority 경계를 저장 엔진의 기능으로 대체하지 않는다.

## 6. “검색이 잘 된다”를 판정할 방법

기존 benchmark는 `fix:` subject를 query로 삼고 parent tree에서 entry file 최대 8개를
고른다. Gold는 변경된 non-test/non-doc 경로이며 additions/deletions도 포함한다.
따라서 미래에 추가되는 파일은 parent에 없고, 큰 diff는 8개 제한 자체로 recall 상한이 낮다.
이 지표는 보존하되 parent-present/unsupported/absent gold 수와 달성 가능한 상한을 함께 보고한다.
불리한 case를 삭제하거나 새 분모로 기존 결과를 덮어쓰지 않는다.

[GR-06](GRAPH-RETRIEVAL-WORK-PACKETS.md#gr-06--retrieval-evaluation)은 다음을 추가한다.

- **정확성:** symbol identity·span, definition/reference edge precision/recall, source anchor 검증,
  invalid/unsupported/unresolved 구분, dirty/rename/delete/parser-upgrade 정합성.
- **검색 품질:** query 종류별 hit@1, recall@8, MRR, graded relevance가 있을 때 nDCG.
  `entries`, 확장된 `files`, 실제 출력된 `symbols`를 따로 측정한다.
- **사용성:** no-match/partial/stale/timeout 전달, 다음 탐색 anchor, worker snapshot과 source identity,
  body-only·한국어 질문 및 필요한 symbol이 budget 밖으로 밀리는 비율.
- **비용:** build와 query 분리, cold/warm/one-file edit, p50/p95, peak RSS, read bytes·stat·hash·parse 수.
  Pack bytes는 provider token·실제 storage I/O·작업 성공률을 대신하지 않는다.

알려진 오류를 검증하는 deterministic fixture에서는 ID 손실·잘못된 authoritative edge·잘못된
source anchor가 없어야 한다. 품질 변경은 같은 snapshot·budget에서 baseline과 paired 비교하고,
held-out 질문을 tuning에 사용하지 않는다. 실제 기준선과 결과가 나오기 전에는 충분한 검색 품질,
특정 정확도 비율, 속도 향상을 완료 성과로 표시하지 않는다.

현재 문서 PR이 변경한 것은 비교·구현 패킷과 오래된 native README 설명이다.
런타임 검색·저장 형식·parser·CI workload는 변경하지 않았다.

[nexus-arch]: https://github.com/abhigyanpatwari/GitNexus/blob/8783a91a54fe80be3f09b79e7e88edbc148d1538/ARCHITECTURE.md
[nexus-cli]: https://github.com/abhigyanpatwari/GitNexus/blob/8783a91a54fe80be3f09b79e7e88edbc148d1538/gitnexus/README.md
[cgc-readme]: https://github.com/CodeGraphContext/CodeGraphContext/blob/f09cf00af9e13969ab31c5841ed39670cd00d9f1/README.md
[cgc-scip]: https://github.com/CodeGraphContext/CodeGraphContext/blob/f09cf00af9e13969ab31c5841ed39670cd00d9f1/src/codegraphcontext/tools/scip_indexer.py
