# Orchestrator performance: 실행 가능한 I/O 최적화 패킷

기준 source: `f9a695ea2de90a160754252c1ee9cbaa4dd7a488` (2026-11-18).
상태: **명세 전용; 아래 최적화는 미구현·미측정**.
관련: [기존 완료 감사·lifecycle 패킷 PR #259](https://github.com/eightmm/oh-my-setting/pull/259),
[Team 측정 원칙](TEAM-ORCHESTRATION-VALIDATION.md).

## 채택 상태

현재 실행 범위는 [통합 작업 큐](ORCHESTRATOR-WORK-QUEUE.md)를 따른다.
아래 ID는 재사용 계약의 참조이며 네 최적화를 모두 구현하라는 지시가 아니다.

| ID | 채택 조건 |
|---|---|
| PERF-02 | 유지. 동일 refresh의 Plan 읽기와 projection 일관성을 작은 변경으로 정리 |
| PERF-01 | 조건부. REL-01의 관측 상태를 먼저 고정하고 반복 transcript 읽기의 비용을 확인한 뒤 같은 observer에서 진행 |
| PERF-03 | 조건부. Lifecycle 작업에 필요한 계측부터 재사용; 실제 batch caller와 persistence 병목이 확인된 범위만 변경 |
| PERF-04 | 조건부. GR-05의 identity 계약과 동일 workload의 반복 query 측정 뒤 채택 |

## 1. 무엇을 다시 만들지 않는가

Rust 전환은 parser·process startup 비용을 바꿀 수 있지만 동일 파일 전체를 반복해서 읽거나
다시 쓰는 작업량을 자동으로 제거하지는 않는다. 이번 목표는 확인된 호출 경로의 반복 작업을 줄이는 것이다.

| 이미 있는 기능 | 확인 source | 이번 작업에서 보존할 점 |
|---|---|---|
| Reader의 view/width별 rendered-line 재사용 | [reader.rs](../crates/oms-command-panel/src/reader.rs) 42–55 | idle polling·scroll에 artifact 재읽기 추가 금지 |
| Watch session의 plan/index/result cache와 metrics TTL | [commands.rs](../crates/oms-command-panel/src/commands.rs) 96–209 | 같은 기능을 별도 cache service로 복제하지 않음 |
| unchanged Room poll과 append tail reader | [room storage.rs](../crates/oms-command-room/src/storage.rs) 354–465 | no-follow descriptor·rotate/truncate 확인 패턴 재사용 |
| extraction SQLite cache와 source freshness | [project_build.rs](../crates/oms-command-graph/src/project_build.rs) 210–361, 620–912 | parser cache가 없다고 잘못 전제하지 않음 |
| usage 중복 validation 감소 | [PR #249](https://github.com/eightmm/oh-my-setting/pull/249) | 같은 중복 제거를 신규 구현으로 다시 올리지 않음 |
| 동일 byte 결과의 atomic no-op | [atomic_bytes.rs](../crates/oms-safe-fs/src/atomic_bytes.rs) 525–531 | duplicate receipt가 매번 write/fsync된다고 주장하지 않음 |

아래 네 패킷은 source에서 확인한 반복 경로다. 현재 서버의 병목 순위나 개선율은
실제 실행·I/O 측정 전까지 미확인이다. 별도 authoritative store, background daemon,
provider 호출, filesystem watcher dependency를 추가하지 않는다.

## 2. PERF-01 — native child 관측의 증분 읽기

### 확인한 문제

[watch.rs](../crates/oms-command-panel/src/watch.rs) 73–100의 refresh는
`Panel::observe`를 호출한다. [native_children.rs](../crates/oms-command-panel/src/native_children.rs)
33–44의 `jsonl`은 호출마다 파일 전체를 읽고 JSON rows로 변환한다.
Claude parent와 표시되는 child transcript, AGY parent와 child가 이 reader를 사용한다.
AGY는 parent를 찾지 못하면 2048개 directory entry를 보고 최근 후보 24개를 읽는다
(230–263행). 캐시된 AGY parent 경로도 매번 본문을 다시 읽는다.
각 transcript 상한은 4 MiB이고 child 표시는 마지막 3개다. 이는 bound이며 실제 읽힌 양은 아니다.

### 구현 지시

1. 기존 watch `Observations`에 bounded per-source 관측 상태를 연결한다.
   key는 provider/account-or-config-root, actual process generation, session, repo, source identity다.
   PID만 같은 새 세션 또는 같은 경로의 새 inode는 이전 관측을 재사용하지 않는다.
2. descriptor/metadata가 같은 경우 본문 재읽기를 생략할 수 있지만 이는 cached observation이다.
   같은 inode·size·mtime은 bytes가 같다는 증명이 아니다. 마지막 byte 검증의 `observed_at`과
   coverage/status를 유지하고 metadata 확인 시각으로 최신 byte 관측 시각을 덮어쓰지 않는다.
   append는 이전 complete-row 경계 이후만 읽고 partial final row를 별도 bounded tail로 보관한다.
   suffix만 읽은 경우 이전 prefix 전체를 다시 검증했다고 표시하지 않는다.
3. Room `LogReader`의 no-follow·정규 파일·owned descriptor·prefix/boundary 검사를 참고한다.
   Room 전용 validation을 native transcript에 억지로 적용하지 말고 공통 native FS primitive를 재사용한다.
4. rename/rotate/truncate/감지된 in-place rewrite/접근 실패 시 관측 generation을 무효화한다.
   identity가 확인된 bounded rebuild 또는 explicit partial/unavailable로 전환한다.
   append-only 계약이 증명되지 않은 source는 최대 재검증 간격과 read budget을 정하고,
   주기적 bounded 전체 읽기/digest 검증으로 size·mtime을 보존한 rewrite도 다시 관측한다.
   prefix/boundary sampling만으로 중간 bytes의 무변경을 증명하지 않는다. cache miss·불확실성은
   bounded rebuild로 fallback하며 재검증 기한을 넘기면 fresh 대신 stale/partial을 표시한다.
5. AGY discovery는 현재 binding/anchor가 바뀔 때나 실패 후 bounded retry 시에만 수행한다.
   단순 negative TTL로 새 child를 영구히 놓치지 않으며 재검색 deadline/원인을 노출한다.
6. 원문 전체 복제 대신 현재 화면에 필요한 제한된 projection과 cursor만 유지한다.
   cache 삭제는 읽기 비용만 늘려야 하며 task/admission/result 상태를 바꾸면 안 된다.
   validated bytes가 필요한 evidence consumer는 cached UI 관측을 검증 증거로 승격하지 않는다.
   필요한 검증을 새로 수행하거나 unavailable/held를 반환하는 기존 의미를 보존한다.

### 선행과 완료 조건

관측 coverage/status를 명시하는 REL-01의 interface를 먼저 확정한다.
REL-01의 정확성 수정은 이 최적화나 성능 평가가 끝날 때까지 기다리지 않는다.
missing/invalid/oversize/PID mismatch를 빈 child 배열로 숨기는 기존 의미를 cache에 고정하지 않는다.
코드 PR은 같은 `native_children.rs`를 바꾸므로 REL-01 → PERF-01 순서로 병합한다.
두 명세 PR 자체는 서로 독립적으로 검토할 수 있다.

기존 native identity fixture를 확장하고, 새 incremental reader가 아직 없던 경계만 추가 시험한다.
실제로 바뀌지 않은 parent+3 children fixture의 재검증 기한 전 두 번째 refresh만
transcript body read 0을 목표로 삼고, 이전 `observed_at`과 cached coverage가 보이는지 검증한다.
이 수치를 metadata에 드러나지 않는 rewrite까지 즉시 감지한다는 보장으로 해석하지 않는다.
size·mtime 보존 rewrite와 중간 prefix 변경 fixture는 재검증 기한 내 반영, 기한 초과 시 stale,
strict evidence 요청 시 재검증 또는 held를 확인한다. 주기 검증 비용도 성능 결과에 포함한다.
append에서는 읽은 bytes가 새 bytes와 명시된 boundary overhead로 설명되어야 한다.
rotation·partial-row·oversize·권한 변경·잘못된 세션에서 coverage와 누락 수가 정확해야 한다.
두 panel은 각각의 비용을 측정하며 첫 PR에서 cross-process cache/daemon을 만들지 않는다.

## 3. PERF-02 — 한 refresh의 source 읽기와 projection 공유

### 확인한 문제

[commands.rs](../crates/oms-command-panel/src/commands.rs) `snapshot_with` 233–263은
[state.rs](../crates/oms-panel-core/src/state.rs) `snapshot`을 먼저 호출한다.
`state::snapshot` 97–100에서 Plan을 읽고, 이어 `session.plan`이 같은 Plan을 다시 취득한다.
Session cache가 hit여도 앞의 Plan 읽기는 남는다. cache miss에서는 동일 refresh에 두 번 읽는다.
또한 report Plan과 finalized 계산 Plan이 다른 시점의 내용일 수 있으므로 단순 속도뿐 아니라
같은 화면 안에서 source cohort를 명확히 하는 이점이 있다.

### 구현 지시

1. 한 collection turn이 취득한 bounded Plan bytes/parsed value를 기존 snapshot과
   finalized/results 계산에 함께 넘긴다. 기존 `cache::TickReads` 또는 작은 typed read set을 쓴다.
2. 동일 tick의 index/plan/outcome artifact도 source identity가 같은 경우 한 번 취득한다.
   원래 producer별 unavailable·partial·stale 진단을 결과와 함께 유지한다.
3. cache key에 repo/state-root, source generation, 실제 파일 stamp/digest를 포함한다.
   파일 변경 전후 검사가 다르면 오래된 값을 fresh로 붙이지 말고 retry/held 표기로 반환한다.
4. lifecycle projection의 순수 log decoding과 실제 process liveness 검증을 분리해서 계측한다.
   parser 재사용을 이유로 현재 native process/owner generation 확인을 생략하지 않는다.
5. mutation은 기존 producer를 다시 검증한다. UI read set을 lease·budget·cleanup 권한으로 사용하지 않는다.
6. 여러 writer를 걸친 결과를 전역 atomic snapshot이라고 부르지 않는다.
   같은 tick에 관측한 source별 generation과 read failure를 유지한다.

### 완료 조건과 시험

Fake service/file-reader counters로 cold/warm refresh 각각의 Plan 취득 횟수를 고정한다.
한 tick에 Plan body를 중복해서 읽지 않으며 stable outcome은 기존 cache를 이용한다.
Plan이 취득 도중 바뀌는 fixture에서 report와 finalized가 서로 다른 Plan인 채 fresh로 표시되지 않는다.
plan 삭제·권한 오류·state-root 변경·index 변경은 cache를 무효화한다.
`panel_parity`, core result-cache parity, 실제 native liveness fixture의 의미를 모두 보존한다.
Reader의 idle/scroll test는 회귀 확인만 하며 Reader cache를 새 기능으로 설명하지 않는다.

## 4. PERF-03 — usage history의 write amplification 축소

### 확인한 문제

[usage.rs](../crates/oms-command-telemetry/src/store/usage.rs) `begin_inner` 579–642와
`record` 646–681은 history를 읽고 검증한 뒤 `old.to_vec()`에 새 receipt를 붙인다.
[store::mutate](../crates/oms-command-telemetry/src/store/mod.rs) 293–328와
`Root::atomic_mutate`는 바뀐 전체 byte stream을 temporary file로 쓰고 fsync/rename한다.
따라서 **새 revision을 추가할 때** history 크기만큼 쓰는 경로가 남아 있다.
동일 receipt replay는 `new == old`로 physical write를 생략하므로 이 둘은 분리 측정한다.
현행 input/output 상한은 256 MiB이며 이를 없애거나 단순히 키우는 것이 해결책은 아니다.

### 단계 A: 현재 형식을 유지하는 작은 개선

1. pending begin, 실제로 바뀐 partial/final revision, 동일 replay, 늦은 correction별
   bytes read/written, allocations, lock hold, fsync/rename을 먼저 구분 계측한다.
2. 같은 producer가 이미 가지고 있는 여러 독립 observation을 한 transaction으로 기록할 수
   있는 실제 caller가 있을 때만 bounded batch API를 추가한다. 미래 event를 기다리지는 않는다.
3. batch도 각 call/revision 검증을 적용하고 historical rows를 그대로 보존한다.
   begin acknowledgment와 physical launch 사이의 durable pending 규칙은 개별 호출 그대로 유지한다.
4. final/unknown receipt의 durable write를 timer 뒤로 미루지 않는다.
   모델 호출 전에 메모리 buffer만 만든 뒤 예약 완료로 처리하는 경로를 금지한다.
5. #249와 달리 새 revision의 persistence cost가 줄었는지 보여 준다.
   실사용 caller에서 묶을 기회가 없다면 batch API를 억지로 추가하지 않는다.

### 단계 B: 측정상 필요할 때만 저장 형식 검토

여전히 history 크기에 따른 write amplification이 주된 비용이면 기존 usage writer 안에서
durable append/segment/checkpoint 중 하나를 선택하는 별도 계약 변경 PR을 작성한다.
이 문서는 특정 형식이나 schema 번호를 확정하지 않는다. 다음 조건이 설계 gate다.

- canonical history는 하나이며 최신 projection은 삭제 후 history로 재구성 가능하다.
- partial append/중간 crash/rename·directory sync 실패에 대해 publication uncertainty를 보존한다.
- ordinal, call identity, revision conflict와 out-of-order correction 의미가 동일하다.
- cumulative session 차분, baseline unknown, inclusive usage 및 cost basis를 보존한다.
- pending/unknown liability, stale observation과 모든 audit evidence를 보존한다.
- 구버전 reader의 refusal·migration·rollback과 held 상태의 recovery가 명시된다.
- 일반 artifact-index retention 규칙으로 usage를 잘라내지 않는다.

### 완료 조건과 시험

[usage tests](../crates/oms-command-telemetry/tests/usage.rs)의 concurrent begins/replay,
unknown cost, malformed history, late correction, physical intent 및 resource 연계 fixture를 확장한다.
입력 receipt sequence에 대한 현재 구현의 projection을 oracle로 비교한다.
read-only load와 writer crash 지점 모두 검사하고, 실패를 성공 sample에서 삭제하지 않는다.
LC lifecycle consumer와 같은 dispatch/usage lock 순서를 유지한다.
저장 형식 변경은 앞선 lifecycle correctness를 우회하거나 예약을 자동 반환하지 않는다.

## 5. PERF-04 — graph lexical context의 per-query 전처리 재사용

### 확인한 문제

[project_context.rs](../crates/oms-command-graph/src/project_context.rs) `rank` 20–146은
매 query마다 non-test nodes의 source를 파일별로 읽고 digest 확인, quoted-string extraction,
field token counts와 inverted postings를 다시 만든다.
[project.rs](../crates/oms-command-graph/src/project.rs) 205행의 SQLite fast path에는
`symbol/api/neighbors/trace`만 들어가며 `context`는 전체 Graph 경로를 사용한다.
이미 존재하는 extraction cache가 이 lexical 전처리까지 재사용한다는 뜻은 아니다.

### 구현 지시

반복 query의 전처리 비용을 확인해 PERF-04를 채택한 경우 다음 범위를 적용한다.

1. query와 무관한 lexical fields/postings를 graph revision, source digest,
   extraction/ranking version에 결합해 기존 `cache.sqlite3` 또는 graph projection 안에서 재사용한다.
   새 검색 DB/service를 만들지 않고 현재 disposable projection의 versioning을 확장한다.
2. name/path/summary/quoted-string 가중치와 rounding/tie-break를 그대로 유지한다.
   성능 PR에서 retrieval algorithm을 동시에 바꾸지 않는다.
3. 현재 `rank`의 source digest mismatch/접근 실패 시 quoted source 제외 의미를 보존한다.
   source 검증을 생략한 결과를 fresh라고 하지 않는다. 완전한 identity 증명이 없으면 기존 경로로 fallback한다.
4. 기존 discovery가 이미 제공하는 validated source identity를 같은 command 안에서 재사용한다.
   별도 cached HEAD만으로 unstaged/untracked/source 변경을 무시하지 않는다.
5. graph revision·parser/schema·source content가 바뀌면 해당 posting을 재생성한다.
   초기에는 validation 비용을 유지하면서 tokenization/index 구축 재사용부터 구현해도 된다.
6. `--entry-path` 경로는 lexical ranking을 우회하는 기존 의미를 유지한다.
   cache miss/corrupt/삭제는 결과 차이나 provider 호출 없이 bounded 재구축으로 처리한다.

### 완료 조건과 시험

[project_engine.rs](../crates/oms-command-graph/tests/project_engine.rs)의
`check_lexical_reference`가 entries와 BM25 scores를 정확히 비교하므로 그대로 oracle로 사용한다.
추가 fixture는 repeated-query, 새 query/같은 revision, 하나의 source 변경,
quoted-string 변경, UTF-8, corrupt cache, stale source, parser-version 변경을 다룬다.
warm query에서 lexical index 재구축 횟수가 줄고 순위/점수/coverage가 동일해야 한다.
source bytes 감소는 source identity 재사용이 증명된 경우에만 별도 성과로 기록한다.
기존 graph recall benchmark는 품질 확인용이며 새 query latency benchmark와 혼동하지 않는다.

## 6. 공통 측정·PR 제출 조건

고정 fixture와 동일 host/filesystem에서 baseline/candidate를 교차 실행한다.
각 패킷은 실제 기대하는 반복 단위와 data size를 적고 cold/warm 결과를 분리한다.

| 축 | 최소 workload / 기록 |
|---|---|
| Panel | no-change/append/rotate/error, parent+여러 children, panel 1개/2개 |
| Usage | history 크기별 begin/새 revision/replay/correction, concurrent writers |
| Graph | 작은/큰 저장소, 동일·상이 query, 1-file 변경, cache hit/miss |
| 비용 | bytes read/written, metadata calls, subprocess, fsync/rename, peak RSS, lock hold, p50/p95 latency |
| 정확성 | current oracle와 결과·unknown·coverage·publication phase 비교 |

Source에서 계산한 byte 수, userspace read/write counters, 실제 storage I/O는 서로 다르다.
page cache가 따뜻한 wall-time만으로 Disk I/O 개선을 선언하지 않는다.
OS 계측이 불가능하면 가능한 counter와 그 한계를 보고하고 physical-I/O 결론을 보류한다.
CI에는 결정적 regression/count assertions만 넣는다. 비싼 성능 반복은 opt-in으로 둔다.

PERF-02는 현재 호출 경로의 작은 정리로 진행한다. PERF-01은 REL-01 이후 측정 근거가 있을 때,
PERF-04는 GR-05 이후 반복 query 비용이 확인될 때 채택한다. PERF-03의 batch나 저장 형식 변경은
실제 caller와 persistence 비용이 요구하는 범위만 진행한다. 동일 파일의 코드 PR은 순차 병합한다.
Lifecycle P0의 종료·예약 정산 누락을 성능 작업으로 대체하지 않는다.

확인 범위: Reader 전체, watch refresh caller, Session/snapshot, native child collector 전체,
usage read/write와 atomic writer, graph discovery/cache/context ranking의 결정적 경로 및 관련 tests.
미확인 범위: 실제 provider transcript·서버 저장장치·실제 p50/p95·모든 host의 syscall 결과.
이번 작성 환경에는 Cargo가 없어 runtime tests와 benchmark를 실행하지 않았다.
문서 검증과 미래 코드·성능 검증을 구분해 PR에 기록한다.
