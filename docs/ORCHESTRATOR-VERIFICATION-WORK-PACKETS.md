# Orchestrator 검증·CI 비용·설치 효율 작업 패킷

**DESIGN ONLY.** 기준 source는 main
[`f9a695ea2de90a160754252c1ee9cbaa4dd7a488`](https://github.com/eightmm/oh-my-setting/commit/f9a695ea2de90a160754252c1ee9cbaa4dd7a488)이다.
이 문서는 #258의 모델 선택 수정과 #259의 orchestrator 감사·lifecycle 계획을 보완한다.
Workflow, required checks, protection, runner, 설치 정책은 변경하지 않는다.
아래 acceptance는 후속 구현의 완료 조건이며 이번에 실행한 테스트 결과가 아니다.

## 1. 목표와 현재 관측

목표는 필요한 검증을 유지하면서 불필요한 build·재실행·로그 재수집을 줄이는 것이다.
CI 실행시간과 job 누적시간은 과금액이 아니다. 실제 청구, 무료 사용량, runner별 단가는
조사하지 않았으므로 달러 절감액이나 무료 실행 보장을 제시하지 않는다.

| source에서 확인한 내용 | 후속 작업의 판단 |
|---|---|
| Workflow trigger는 `push: branches: [main]`, PR, manual, weekly schedule | feature branch push+PR 이중 실행이 이미 있다고 주장하지 않음 |
| Concurrency key는 workflow/event/ref이며 `cancel-in-progress: true` | 동일 event/ref의 이전 run 취소는 존재; 다른 event proof는 자동 대체하지 않음 |
| `check.sh`는 mode 출력 전 native resolve/stage를 시도 | 순수 path planning이 release build에 도달할 수 있는 경계 분리 필요 |
| Docs-only affected mode도 네 invariants를 실행 | docs reference만 필요하다는 현재 계약과 실행 범위 재정렬 필요 |
| 일반 코드 변경은 `changed-code-full`, 모든 `crates/*`는 native 선택 | 작은 변경의 안전한 범위 축소는 별도 proof가 필요 |
| Focused와 native Linux job 모두 install lifecycle script 호출 | 실행 OS/image/profile을 대조한 후 중복 제거 후보로 판단 |
| 해당 script는 내부에서 `symlink copy`를 모두 순회 | matrix의 copy/auto 이름만으로 실제 coverage를 판단할 수 없음 |
| Stage elapsed는 stdout에 있지만 timings 파일은 초기화 후 기록되지 않음 | 기존 Rust timing parser와 실제 producer 연결 필요 |
| Stage log와 check runtime은 종료 시 삭제 | 재현에 필요한 bounded·scrubbed evidence를 선택적으로 보존 |
| Workflow에 cache/summary/upload-artifact step이 없음 | 기존 summary·Python helper를 설명하는 문서와 현재 구현이 불일치 |

근거: [workflow](../.github/workflows/test.yml), [gate](../scripts/check.sh),
[critical paths](../scripts/lib/critical-paths.sh),
[native staging](../scripts/lib/platform.sh),
[install fixture](../tests/install-lifecycle-smoke.sh).
`PROJECT.md`는 기준 checkout에 없다. 따라서 여기에 repository-specific CI 계약이 이미
있다고 가정하지 않는다. 후속 구현은 현재 `AGENTS.md`와 global verification 규칙을 따른다.

## 2. 비용을 늘리지 않는 공통 조건

- 새 hosted runner, OS matrix, scheduled run, 유료 provider/GPU/SSH 호출을 추가하지 않는다.
- 기본적으로 artifact upload와 remote cache storage를 추가하지 않는다. 기존 job의 로그·summary와
  job-local build 재사용부터 사용한다. 별도 storage 도입은 이 패킷의 완료 조건이 아니다.
- 테스트를 통과시키기 위해 required job을 skipped로 바꾸거나 protection을 완화하지 않는다.
- `oms land`의 exact-SHA/full-proof와 scheduled update의 affected-proof 차이를 보존한다.
- 기존 `--ci-full`의 durable dispatch intent·재시도 재사용을 유지한다. 새 full run을 자동 발사하지 않는다.
- 이미 실행한 같은 검증은 변경 입력·남은 위험이 있을 때만 반복한다. 로그 수집만을 위한 재실행은 없다.
- Full fallback과 실패 상태는 유지한다. Unknown coverage를 green이나 비용 0으로 채우지 않는다.

작업 순서는 **VW-01 → VW-04의 측정 → VW-03 → VW-02**를 권장한다.
VW-05는 lifecycle 구현의 nearest fixture에 병행할 수 있다. 다섯 개는 별도 review 단위이며,
모두 구현한다는 약속이나 CI 설정 변경의 묶음 승인이 아니다.

## VW-01 — P0: scope planning과 docs 검증에서 native build를 분리

### 문제와 재사용

`scripts/check.sh`는 `--list-stages`가 아닌 경우 먼저 maintenance marker와 native payload를
준비한다. 그 뒤 `prepare_affected_gate`와 `--print-affected-mode`가 처리된다.
`oms_native_stage`는 payload가 없으면 pinned Rust로 release binaries를 build한다.
따라서 path만 판단하는 `affected_plan`이 native build 시도와 state 준비에 의존한다.

Docs-only가 선택되어도 `RUN_QUICK`은 skill manifest, doc references, migration Cargo run,
source-distribution 전체를 실행한다. 이미 존재하는
[`source-distribution-smoke.sh --docs-only`](../tests/source-distribution-smoke.sh)를 재사용한다.
여기서 제거할 것은 판정·문서 확인에 불필요한 준비다. Native executable을 실제 호출하는
runtime/install 검증의 bootstrap·payload 검증을 생략하는 변경은 아니다.

### 구현 범위

1. Git base/head와 path·critical boundary 판단을 side-effect 없는 초기 단계로 분리한다.
2. Mode-only 결과가 결정되면 native staging·maintenance 등록·provider discovery 없이 반환한다.
3. Ordinary docs는 reference check로, skill/template/role prose는 기존 bounded contract suite로 보낸다.
4. `AGENTS.md`, policy/workflow/selector 변경과 해석 불명은 기존 full 경계를 유지한다.
5. Workflow와 local gate가 같은 계획을 읽는다. 두 번째 path classifier를 만들지 않는다.

### 완료 조건

- `cargo`/provider 실행을 거부하는 fixture에서도 mode-only가 정확한 mode/reason을 반환한다.
- Docs-only 실행에는 native build, Cargo fetch, model 호출이 없고 깨진 relative link는 실패한다.
- Mixed docs+code, deleted/renamed path, invalid base, untracked/dirty tree, force-full을 구분한다.
- 계획 stdout/CI outputs는 machine-readable하며 source `.oms`에 새 상태를 남기지 않는다.
- 현재 기능의 test selection과 다른 점을 fixture 표로 설명한다. 실제 CI 소요시간은 별도 측정한다.

## VW-02 — P2: code 변경의 affected 범위를 증거로 좁히기

### 문제와 재사용

현재 gate는 prose 외 코드에 사실상 full fallback을 적용하고 `crates/*`가 native lifecycle을
선택한다. 보수적인 선택 자체가 오류는 아니지만 작은 leaf 변경까지 전체 비용을 만든다.
재사용 대상은 `critical-paths.sh`, 기존
[project affected logic](../crates/oms-command-graph/src/project_affected.rs), Cargo dependency 정보다.
Graph의 `affected`라는 이름만으로 이 Rust workspace의 안전한 test closure가 증명되지는 않는다.

### 구현 범위

1. 실제 자주 바뀌는 noncritical leaf crate 하나를 고르고 현재 full 검증의 필요한 범위를 기록한다.
2. Changed crate와 reverse dependencies, integration tests, build scripts/features를 함께 계산한다.
3. Cargo workspace/lock/toolchain, lifecycle/permission/landing/selector는 full로 유지한다.
4. 삭제·이름 변경·동적 dependency·불완전 graph는 원인과 함께 full fallback한다.
5. 먼저 advisory plan으로 비교하고, 빠뜨린 required path가 없다는 fixture 후에만 executor와 연결한다.

### 완료 조건

- Leaf→dependent→CLI의 작은 fixture에서 역방향 test closure가 정확히 선택된다.
- Shared fixture/build script/public contract 변경과 feature 조건을 놓치지 않는다.
- 최소 한 실제 변경에서 기존 full과 새 선택의 coverage 차이를 설명하고 기존 검사 하나를 대체한다.
- Default matrix/job 수는 증가하지 않으며 gate·`ci::full_scope`가 새로운 scope를 오인하지 않는다.
- Scope 축소는 full installation proof로 승격되지 않는다. 생략된 범위는 receipt에 남는다.

## VW-03 — P1: build·install 반복과 CI proof 이름을 정렬

### 문제와 재사용

Full workflow의 `focused`는 `install-lifecycle-smoke.sh`를 실행하고 별도 native job도 같은
script를 실행한다. 이 script는 `OH_MY_SETTING_LINK_MODE`를 내부 loop에서 다시 설정하여
symlink와 copy를 모두 검사한다. 따라서 현재 matrix label은 실제 실행 mode의 정확한 설명이 아니다.
Linux jobs의 runner label은 `ubuntu-24.04`와 `ubuntu-latest`이므로 동일 image라고 단정하지 않는다.

여러 job이 toolchain/dependency/native build를 별도로 준비한다. 우선 같은 job 내부의 재사용과
필요한 artifact 목록을 정리한다. Cross-job cache나 binary download를 필수로 추가하지 않는다.
재사용: [smoke-env staging](../tests/lib/smoke-env.sh), `oms_native_stage`,
[CI full-scope classifier](../crates/oms-command-landing/src/ci.rs),
[installation verification policy](INSTALL.md#verification-policy).

### 구현 범위

1. Check/OS/image/toolchain/profile/link-mode별 실제 invocation과 coverage 표를 만든다.
2. 동일 source·binary·환경·fixture 입력임을 관측한 install lifecycle 검사만 한 canonical job에
   귀속한다. 서로 다른 image/profile 또는 확인하지 못한 coverage는 유지한다.
3. Local full gate는 여전히 해당 coverage를 한 번 수행한다. Native macOS/BSD 검사는 유지한다.
4. Native build는 job 안에서 한 번 stage하고 manifest/source SHA/hash로 재사용한다.
5. Job label·`gate`·`full_scope`·install receipt가 같은 coverage를 해석하도록 함께 바꾼다.
6. PR와 main의 중복 의심 run은 commit/tree/workflow recipe/event/환경을 비교해 보고만 한다.
   Squash/merge 후 main SHA의 proof를 PR green으로 대체하거나 trigger를 일괄 삭제하지 않는다.

### 완료 조건

- 각 supported host의 copy/symlink coverage와 native binary provenance가 이전보다 약해지지 않는다.
- Wrong SHA, wrong hash, incomplete binary set, cache miss는 재검증 또는 실패로 이어진다.
- 동일 exact SHA의 pending/dispatched CI 재시도는 기존 intent를 재사용하고 두 번째 run을 만들지 않는다.
- Canonical `full_scope` fixture에서 success/failed/skipped/unknown/new job-name 처리를 검증한다.
- 같은 workload의 build 횟수와 suite 중복 횟수를 비교한다. 절감 달러나 host portability를 추정하지 않는다.

## VW-04 — P1: 실패 evidence와 timing을 기존 gate에 연결

### 문제와 재사용

`stage()`는 elapsed seconds를 출력하고 로그를 삭제하며, 종료 trap은 check runtime을 삭제한다.
`timings`와 `timings-dir`은 만들어지지만 현재 gate에서 기록하는 producer가 없다.
`COMPONENTS.md`의 `gate_timings.py`, `ci-query.py`, GitHub slowest-stage summary 설명은
현재 Python-free tree/workflow와 맞지 않는다. 해당 Python helper는 복원하지 않는다.

이미 [Rust `helpers::gate_timings`](../crates/oms-command-landing/src/helpers.rs)가
stage/test/TSV 형태를 읽고, [output-digest](../crates/oms-command-peer/README.md)와
[`oms-scrub`](../crates/oms-scrub/Cargo.toml)가 bounded output 및 scrubber 경계를 제공한다.
이를 연결하며 새 telemetry DB·daemon은 만들지 않는다.

### 구현 범위

1. 기존 stage 실행에서 name/status/exit/wall/source SHA/selection reason을 bounded record로 남긴다.
2. 성공·실패·timeout·cancelled·not-run을 구분한다. 실패 직전 기록도 trap에서 보존한다.
3. Raw log 전체 대신 allowlisted metadata와 scrubbed bounded tail을 기존 landing evidence 또는
   명시된 local evidence 경로에 보존한다. Live checkout `.oms` state guard를 우회하지 않는다.
4. 현재 job 로그와 `GITHUB_STEP_SUMMARY`에 같은 record의 요약을 출력한다. Upload step은 추가하지 않는다.
5. 로그 bytes/omitted bytes, build 횟수, cache hit 근거, queue/run 시간을 분리한다.
   Stage duration 합은 parallel wall time·physical I/O·청구액과 구분한다.
6. 문서의 stale helper와 현재 구현에 없는 summary 주장을 실제 producer로 정정한다.

### 완료 조건

- Fail/timeout/signal/parallel early-exit마다 수행·생략 stage와 원래 exit가 보존된다.
- Credential-shaped fixture와 private path는 출력 전 scrub/withhold되고 artifact는 명시적 byte cap을 가진다.
- Summary 실패가 원래 검사 실패를 success로 바꾸지 않으며 nested fixture는 실제 job summary를 쓰지 못한다.
- Mode-only/stdout JSON 계약은 유지하고 추가 model/network/test 실행을 만들지 않는다.
- 디스크 bytes는 실제 계측한 범위만 보고한다. 알 수 없는 metric은 null이며 0으로 대체하지 않는다.

## VW-05 — P1: native fixture의 대기·환경·포터빌리티 증거 정리

### 문제와 재사용

이미 실제 child process와 fake provider를 사용하는 Team/native fixtures가 있다.
`services_control.rs::team_contract_enrollment_uses_native_lifecycle_and_room_proofs`는
실제 UTC expiry를 만들기 위해 minute boundary와 만료 시각까지 기다리는 구간이 있다.
시간 의존 회귀를 blanket timeout 증가로 처리하면 느린 gate와 불안정한 원인이 남는다.

재사용: [native control fixture](../crates/oms/src/services_control.rs),
[panel parity](../crates/oms-command-panel/tests/panel_parity.rs),
[orchestrator tests](../crates/oms-command-panel/src/orchestrator.rs),
[BSD smoke](../tests/bsd-portability-smoke.sh), 기존 hermetic `smoke-env.sh`.

### 구현 범위

1. Protocol/receipt/replay/expiry 결정은 가능한 기존 clock seam이나 deterministic event barrier로 검증한다.
2. Process ownership/cancel/reap 증거에는 실제 local child를 유지한다. 순수 mock으로 대체하지 않는다.
3. Provider prompt 변화·fragmented output·late ACK·busy/draft·missing capability는 기존 fixture를 확장한다.
4. Feature flag/CLI help fixture와 실제 provider account 시험을 구분하고 실제 provider 호출은 하지 않는다.
5. Linux/macOS 기존 job에서 실제 실행/명시적 capability skip을 기록한다. Stable gate에는
   계획이 허용한 skip만 반영하고, 필요한 검사를 실행하지 못한 skip을 full proof로 승격하지 않는다.
6. Ubuntu 20.04 floor·WSL2 지원은 현재 Ubuntu/macOS CI만으로 증명됐다고 쓰지 않는다.
   기존 호스트에서 수행할 수 있는 명시적 portability smoke를 정리하되 새 자동 matrix를 추가하지 않는다.

### 완료 조건

- 같은 deterministic case가 host wall-clock 위치와 무관하게 같은 결과를 낸다.
- 실제 owned descendant의 cleanup, PID/connection generation, stop/timeout 이유는 계속 검사된다.
- Fake-provider 환경에서는 외부 provider executable·네트워크가 없어도 실행되며 accidental 호출은 실패한다.
- 새 테스트는 가장 가까운 canonical fixture를 확장하고 중복된 기존 case를 대체한 내역을 적는다.
- Supported-host evidence와 아직 없는 floor/WSL/live vendor evidence를 분리한다.

## 3. 제출 기준과 미확인 범위

공식 [GitHub required-check 문서](https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks)는
workflow를 path/branch/commit 조건으로 건너뛰면 required check가 Pending으로 남을 수 있고,
job 조건에 따른 skip과도 다르게 처리됨을 설명한다. 따라서 VW-01/02는 workflow 전체를 숨기는
필터 대신 기존 stable gate와 실제 selection evidence를 유지한다.
기존 concurrency 취소는 [동일 group의 실행 제어](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency)이며,
취소된 run의 누락 검증을 성공으로 채우거나 다른 SHA의 proof를 재사용하는 근거가 아니다.

각 구현 PR은 source anchors, 변경 전후 실행 계획, 대체한 중복 검사, 실제 실행 결과와
남은 증거를 포함한다. [#259](https://github.com/eightmm/oh-my-setting/pull/259)의
[감사](https://github.com/eightmm/oh-my-setting/blob/7161f94/docs/ORCHESTRATOR-COMPLETION-AUDIT.md)와
[lifecycle 패킷](https://github.com/eightmm/oh-my-setting/blob/7161f94/docs/ORCHESTRATOR-LIFECYCLE-IMPLEMENTATION.md)의
기능 계약을 완화하지 않는다. 이 파일은 main에서 독립적으로 검토할 수 있다.

이번 문서의 근거는 workflow/gate/helper/nearest fixture의 정적 조사다.
GitHub 청구·계정 limits·production I/O·real vendor·지원 최소 OS의 실행 결과는 조사하지 않았다.
검사 범위를 줄이기 전에 correctness를 증명하고, 실제 반복을 줄인 뒤 측정한다.
새 비용이 생기는 CI 구성이나 artifact storage는 이 문서만으로 활성화하지 않는다.
