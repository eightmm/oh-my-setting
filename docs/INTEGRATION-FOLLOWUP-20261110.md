# OMS 비교 조사 후속 구현·검증 작업

기준일: 2026-11-10. 기준 main: `b588c9ea3983764d752e8545acf43f359fde16bd`.
상태: **후속 구현 명세와 인계 문서. 아래 체크리스트는 구현 완료 표시가 아니다.**
사용자는 비교 조사에서 도출한 작업을 PR로 게시하도록 요청했다.
이 문서는 이미 존재하는 기능과 PR을 재사용하여 남은 연결·정확성 문제를 구현 가능한 단위로 정리한다.

관련 문서: [기반 계획](CONTROL-PLANE-EVOLUTION.md),
[UX 계획](CONTROL-PLANE-UX-AND-FEATURES.md),
[이전 구현 리뷰](CONTROL-PLANE-IMPLEMENTATION-REVIEW.md),
[작업 검사](WORK-INSPECTION.md), [컨텍스트 검사](CONTEXT-INSPECTION.md).
기존 계획의 완료 상태를 일괄 초기화하거나 같은 기능을 다시 만들지 않는다.

## 1. 시작점과 중복 방지

최초 조사 기준은 `a33c461bcbd84ea704803f74801fd34795452cec`였다.
PR 준비 때 원격 main을 다시 읽었고, 열린 PR이 45개에서 25개로 줄어 있었다.
아래는 새 기준의 파일과 commit history로 확인한 상태다.
이 저장소는 별도 landing commit으로 반영된 PR도 있으므로 `merged_at`만으로 구현 여부를 판단하지 않는다.
작업 재개 시 main과 관련 PR head를 다시 비교한다.

게시 직전 병행 작업의 [#104](https://github.com/eightmm/oh-my-setting/pull/104),
[#105](https://github.com/eightmm/oh-my-setting/pull/105),
[#106](https://github.com/eightmm/oh-my-setting/pull/106) 본문과 현재 상태도 확인했다.
각각 run budget 예약, CPU resume smoke, 데이터 출처·split 근거를 구현 중이다.
이 세 PR의 실행 결과는 작성자 보고이며 이 문서의 독립 재실행 결과가 아니다.
아래 작업은 이 구현을 먼저 검토·재사용하며 같은 기능을 다시 만드는 지시가 아니다.

| 영역 | 기준 main에서 확인한 구현 | 이어서 사용할 열린 PR | 이번 후속 작업 |
|---|---|---|---|
| 수락 기준·계획 | #53의 `covers`, contract 검사, intent amend와 #64·#76의 계획 확장 | #86 입력 출처 고정 | F01: 기준 내용과 evidence freshness 연결 |
| 리뷰 | #47 finding 기록, #66 확인·규칙·advisory security seat | #81 GitHub 후속 리뷰 | F02: finding–fix–receipt 연결과 다음 리뷰 반영 |
| 메모리·컨텍스트 | #65 경로별 recall, #70 ranked search·phase 규칙, #85 정책 digest 비교 | #68 기억·handoff, #103 skill 안내 | F03: 실제 작업 범위에서 선택·주입까지 연결 |
| 작업 화면 | evidence matrix, next 설명, brief, 패널 결과 paging·export | #97 그래프 변경 표시 | F04·F05: task inspector, 담당자·알림·provider 동작 |
| 실험 | capsule, metric contract·seed group, #62 claim·figure, #49 Slurm reconcile | #87 실행 입력 식별, #88·#89·#90 출력·split, #75·#84·#104 HPC 확장, #106 데이터 출처 | F06·F07: 집계·출처·budget의 일관성 |
| 학습 기본값 | 기존 ML 문서·실험 계약 | #98·#101 기본 정책, #102 checkpoint 감사, #105 resume smoke | F08: 정책 안내와 실제 중단·재개 검증 연결 |
| 에이전트 성능 | 기존 회귀 테스트, graph recall snapshot | 위 기능 PR 재사용 | F09: replay와 작업 품질 평가 |

새 명령을 만드는 것보다 기존 typed entry point와 canonical reader를 먼저 연결한다.
단일 main orchestrator와 native CLI를 유지하고, 패널은 상태·근거·허용된 행동을 연결한다.
관찰 화면, 메모리, 외부 도구 응답은 승인·lease·publication authority를 만들지 않는다.

## 2. 먼저 처리할 정확성 수정

이 문서와 함께 다음 구현 PR을 게시했다. 모두 검토 중이며 main 반영 완료 표시는 아니다.
각 PR에 수정 전 재현, 통과한 집중 검증, 전체 CI의 남은 범위를 기록했다.

| 작업 | 구현 PR | 검토할 변경 |
|---|---|---|
| F01 | [#109](https://github.com/eightmm/oh-my-setting/pull/109) | 전체 criterion statement fingerprint, legacy binding 처리, 의미 있는 부호 보존 |
| F02a | [#107](https://github.com/eightmm/oh-my-setting/pull/107) | finding–fix commit–scope–receipt 및 clean tracked tree 연결 |
| F00 | [#108](https://github.com/eightmm/oh-my-setting/pull/108) | digest/ci-status fixture의 cwd·HEAD·upstream 격리 |

### F01 — criterion 내용에 맞춰 evidence를 무효화

**관측:** 파일 의존성이 명시된 binding에서 해당 파일을 그대로 두고 criterion ID를 유지한 채
요구사항 문장을 바꾸면 `verified`가 남을 수 있었다.
원인은 [evidence.py](../scripts/lib/oms_runtime/evidence.py)의 binding에 기준 내용 식별자가 없고,
파일별 freshness 경로가 해당 의존성만 검사하기 때문이다.

- [ ] 표시용으로 잘리기 전의 실제 criterion statement를 fingerprint한다.
- [ ] binding이 그 fingerprint를 보존하고, 같은 ID의 내용 변경을 stale로 표시한다.
- [ ] 문구가 같은 경우와 요구사항 외 문서 변경은 구분한다.
- [ ] legacy binding의 증명 범위를 명시하고 근거가 없을 때 verified로 추정하지 않는다.
- [ ] 긴 statement의 표시 한도 이후 변경도 canonical fixture에서 확인한다.
- [ ] Markdown 표기 정리 과정에서 요구사항의 음수 부호·CLI flag 같은 의미 있는 문자를 지우지 않는다.

주요 위치: [projection.py](../scripts/lib/oms_runtime/projection.py),
[evidence.py](../scripts/lib/oms_runtime/evidence.py),
[runtime fixture](../tests/runtime_test_base.py).
다른 task gate의 authority를 새롭게 부여하는 변경으로 확대하지 않는다.

### F02a — finding을 해당 수정의 검증 receipt에 연결

**관측:** #47의 advisory finding 상태는 종류·종료 코드·시점이 맞는 다른 scope의 검증 receipt로도
`verified`가 될 수 있었다. 승인 또는 배포 gate 우회가 확인됐다는 뜻은 아니다.

- [ ] receipt가 실제 검증한 scope를 내용 digest와 함께 가리킨다.
- [ ] finding의 원래 scope, 수정 commit, 후속 scope와 검증 receipt의 관계를 검사한다.
- [ ] 수정 전후 diff digest의 동일성을 요구하지 않는다. 수정은 diff를 바꾼다.
- [ ] mutable ref 이름의 문자열 일치 대신 검증 시점에 고정한 Git identity를 사용한다.
- [ ] 관계없는 성공 receipt, 다른 base·path·head, 손상된 scope와 근거가 없는 legacy 기록을 거부한다.
- [ ] 같은 HEAD라도 미커밋 tracked 변경을 검증한 scope를 그 commit의 증거로 인정하지 않는다.
- [ ] 올바르게 연결된 정상 후속 검증은 통과하고 기존 `fixed` 기록은 읽을 수 있다.

주요 위치: [review_findings.py](../scripts/lib/review_findings.py),
[peer-review.sh](../scripts/peer-review.sh), 기존 review smoke fixture.

### F00 — CI fixture의 현재 디렉터리와 Git 상태 격리

**관측:** 최초 조사 환경에서 `test_ci_status_reports_conclusion` 단독 실행은 통과했고,
`test_output_digest_keeps_failures_and_exit_code` 다음에 실행하면 실패했다.
앞 테스트의 `cd "$ROOT"`와 뒷 테스트의 암묵적 Git 상태 의존이 원인이었다.
detached worktree에서는 같은 결함이 가려질 수 있으므로 재현 조건을 함께 기록한다.

- [ ] 앞 테스트의 작업 디렉터리 변경이 호출자에게 남지 않는다.
- [ ] ci-status fixture는 자기 저장소·HEAD·upstream과 일치하는 응답을 명시한다.
- [ ] 단독·연속 실행이 같은 결과를 내고 실제 실패 상태를 검사하는 assertion을 유지한다.

주요 위치: [scripts-smoke.sh](../tests/scripts-smoke.sh).
제품 ci-status 동작을 테스트에 맞춰 바꾸지 않는다.

### F06a — seed 재실행 집계: 기존 #87 우선

metric 정렬 후 `(group, seed)`의 마지막 행을 고르면 기록 시간 대신 점수 순서가 대표 실행을 결정할 수 있다.
seed 1의 이전/최신 값이 0.2/0.8이고 seed 2가 0.6인 fixture에서 최신 평균은 0.7이다.
확인한 기존 max 경로에서는 0.4가 나왔다.
[#87](https://github.com/eightmm/oh-my-setting/pull/87)이 이 문제와 capsule 입력 연결을 다루므로
중복 구현 PR을 만들지 않고 그 변경을 먼저 검토한다.

- [ ] 최신 run 선택은 출력 정렬과 독립적이다.
- [ ] 집계에 사용한 run·seed와 제외 이유를 확인할 수 있다.
- [ ] #89의 표가 top/diff/claim과 같은 집계 함수를 사용한다.

## 3. 연결 작업과 완료 기준

### F02b — 리뷰 지적을 다음 리뷰와 task 화면까지 전달

선행: F02a. 재사용: #47·#66 구현, [#81](https://github.com/eightmm/oh-my-setting/pull/81).

1. 다음 리뷰 입력에 이전 finding ID, 열린 지적, 수정 commit과 관련 검증만 제한된 크기로 넣는다.
2. GitHub comment·review의 source ID와 최신 읽기 시점을 유지하고 같은 내용을 중복 task로 만들지 않는다.
3. task 화면에 open/fixed/verified 상태와 증거를 표시한다. advisory label은 gate verdict를 대신하지 않는다.
4. dismissed finding의 이유를 보존하고, 기준이나 diff가 바뀐 경우 재검토 상태를 드러낸다.

완료 기준: 관계없는 receipt는 해결 상태를 만들지 않고, 정상 수정은 다음 리뷰와 패널에서 동일한 근거로 보인다.
기존 review fixture에 정상·재등장·중복 comment·삭제된 source 경우를 추가한다.

### F03 — task 범위에서 memory·검색·context 선택까지 연결

재사용: #65·#70·#85 main 구현, [#68](https://github.com/eightmm/oh-my-setting/pull/68),
[#103](https://github.com/eightmm/oh-my-setting/pull/103).

확인한 간격: `OMS_MEMORY_TARGET_PATHS`를 사용하는 recall 경로에 실제 task 범위를 공급하는 연결이 없고,
ranked search는 context compiler의 자동 선택 경로에 연결되지 않았다.

1. canonical task의 허용 경로와 phase를 읽는다. 내용에서 경로·권한을 임의로 추정하지 않는다.
2. 관련 memory와 code symbol 후보를 구하고 기존 context budget 안에서 선택한다.
3. 선택 결과에 source·freshness·선택 이유·누락 이유와 사용한 정책 digest를 기록한다.
4. inspector가 실제 주입한 동일 pack을 설명하게 한다. 별도 추천 화면을 사실처럼 표시하지 않는다.
5. skill 안내는 기존 명령과 연결하고 새 verb/상시 모델 호출을 기본으로 추가하지 않는다.

주요 위치: [peer-common.sh](../scripts/lib/peer-common.sh),
[agent_memory_review.py](../scripts/lib/agent_memory_review.py),
[context.py](../scripts/lib/oms_runtime/context.py),
[context_pack.py](../scripts/lib/oms_runtime/context_pack.py),
[context_inspect.py](../scripts/lib/oms_runtime/context_inspect.py),
[ranked.py](../scripts/lib/oms_graph/project/ranked.py).

완료 기준: task path 변경 시 관련 기억 선택이 바뀌고 범위 밖/retired/redacted 기록이 섞이지 않는다.
동일 budget의 고정 fixture에서 필요한 근거 포함률과 선택 지연을 비교한다.
bytes를 provider 청구 token·비용으로 변환해 성과를 주장하지 않는다.

### F04 — task 중심 inspector와 복귀 요약

재사용: [work_inspect.py](../scripts/lib/oms_runtime/work_inspect.py),
기존 evidence matrix·next explanation·brief, panel의 현재 탐색 구조.

1. 선택한 task에서 criterion, blocker, finding, CI, run과 해당 native 세션을 조회한다.
2. 다음 담당자는 명시된 근거로만 parent/worker/human을 구분한다. 근거가 없으면 unknown이다.
3. 사용자 확인 시점의 bookmark를 기존 typed state 경로로 저장하고 이후 변화 요약에 사용한다.
4. 두 snapshot 사이 전체 history가 없으면 그 한계를 유지한다. 삭제는 완료로 해석하지 않는다.
5. 좁은 화면·NO_COLOR·키보드 탐색과 focus 복귀를 기존 operator fixture로 검증한다.

완료 기준: 사용자가 task에서 막힌 이유와 다음 행동 근거를 확인하고 해당 세션·diff·검증 결과로 이동할 수 있다.
핵심 이동은 두 번 이내를 설계 목표로 삼되 실측 전 달성했다고 쓰지 않는다.
패널 화면을 읽는 것만으로 승인·실행·읽음 상태가 의도치 않게 변하지 않는다.

### F05 — provider capability, 알림 전달과 복구 상태

재사용: [현재 coverage](TERMINAL-PANEL.md),
[panel_workspaces.py](../scripts/lib/panel_workspaces.py),
[panel_chats.py](../scripts/lib/panel_chats.py), [panel_watch.py](../scripts/lib/panel_watch.py).

- [ ] launch/observe/resume/steer/stop/approval/usage 지원을 provider별로 분리한다.
- [ ] foreground running, background-only waiting, input-ready, approval-needed를 명시적 신호로 구분한다.
- [ ] 알림의 requested/skipped/delivered/read 상태를 구분하고 exit 0만으로 전달을 확정하지 않는다.
- [ ] 입력 직전에 task·session·pane incarnation과 소유자를 다시 검사한다.
- [ ] batch 관찰 실패는 제한된 retry와 stale 관찰로 처리하고 per-pane 조회 폭증을 막는다.
- [ ] 일회성 hook·worker가 공유하는 endpoint별 요청 한도·장애 상태를 기존 typed state에 둔다.
  동시 시작·재시작에도 예산을 중복 예약하지 않고, 중복 heartbeat는 같은 전달을 반복하지 않는다.
- [ ] 파일·작업 상태 복구, native 대화 resume, 실행 중 process 복구를 각각 표시한다.

완료 기준: pane 재사용, 오래된 알림, muted/skipped 전달, provider 종료,
background subagent만 남은 상황을 fake provider와 private tmux fixture로 검사한다.
live 검증이 없는 provider는 fixture-only/unknown 표시를 유지한다.

### F06b — run → 집계 → 표·그림·claim 출처 연결

선행: #87. 재사용: main의 #62,
[#89](https://github.com/eightmm/oh-my-setting/pull/89),
[#90](https://github.com/eightmm/oh-my-setting/pull/90),
[#106](https://github.com/eightmm/oh-my-setting/pull/106)의 source·split rationale·entity kind.
주요 위치: [run_claims.py](../scripts/lib/run_claims.py),
[run_ledger_metrics.py](../scripts/lib/run_ledger_metrics.py), run capsule·ledger entry point.

1. code/config/data/split/metric contract/seed identity를 run과 함께 보존한다.
2. group claim은 실제 선택한 run·seed 목록과 집계 규칙을 가리킨다.
3. figure의 입력과 capsule 내용 digest를 확인한다. 파일 존재만으로 출처 일치를 선언하지 않는다.
4. 평가 dataset·split·metric protocol의 비교 가능성을 검사한다.
5. 모델·학습 config·훈련 데이터 등의 의도한 차이는 실험 변수로 표시한다. 모든 hash의 동일성을 요구하지 않는다.

완료 기준: 같은 경로의 입력이 바뀌면 기존 claim/figure 연결이 stale 또는 mismatch로 드러난다.
원본 run부터 표·그림의 숫자까지 같은 집계 정의를 사용한다.
SBDD 프로젝트에서 protein/complex·데이터 출처·split 근거를 명시할 수 있어야 하며,
그 metadata의 존재만으로 leakage가 없다고 주장하지 않는다.

### F07 — Slurm 상태와 동시 실험 budget

재사용: main의 #49,
[#75](https://github.com/eightmm/oh-my-setting/pull/75),
[#84](https://github.com/eightmm/oh-my-setting/pull/84),
[#88](https://github.com/eightmm/oh-my-setting/pull/88),
[#104](https://github.com/eightmm/oh-my-setting/pull/104)의 예약·정산 구현.

**근거 수준:** 최초 분석은 #75의 완료 run 집계와 확인–시작 사이 예약 부재에서 나온 정적 위험이었다.
이후 #104 작성자는 budget 1에서 동시 두 시작이 모두 통과하는 수정 전 재현과 예약 후 거부를 보고했다.
실제 Slurm 부하 검증과 구분하고, 새 예약 구현을 추가하기 전에 #104를 먼저 검토한다.
#104 본문은 #75 선행을 명시하지만 조회한 GitHub base는 main이다. 통합 시 diff에 포함된 선행 commit 범위를 확인한다.

1. submit 전 budget을 원자적으로 예약하고 running·finished·failed·cancelled로 정산한다.
2. run/attempt/job/array task/requeue를 연결하고 중복 accounting 응답을 중복 소비로 세지 않는다.
3. gpu-hours와 final-evaluation 소비를 계산할 근거가 없으면 unknown으로 남긴다.
4. process crash·submit 실패·requeue의 예약 복구 규칙을 문서화한다. TTL만으로 실행 중 예약을 임의 반환하지 않는다.
5. 실제 Slurm reservation과 OMS 내부 예산 예약을 혼동하지 않는다.

#104 후속 검토점: PID 재사용·다른 host, launcher 종료 후 원격 job 생존,
ledger/backend 기록 실패를 `released`로 계산하는 계약이 실제 소비와 일치하는지,
`gpu_hours`·`final_evals`의 미확인 값을 어떻게 유지하는지 확인한다.

주요 위치: [experiment_contract.py](../scripts/lib/oms_runtime/experiment_contract.py),
[experiment_run.py](../scripts/lib/oms_runtime/experiment_run.py),
[run-reconcile.sh](../scripts/run-reconcile.sh), #75의 사용량 reader.
완료 기준: budget 1에서 동시 두 시작이 둘 다 admitted되지 않고, 실패·재시도·requeue가 사용량을 부풀리지 않는다.
fake scheduler·고정 accounting 응답으로 검증한다. 자동 kill 정책이나 실제 cluster 설정은 이 작업에 포함하지 않는다.

### F08 — checkpoint 정책의 실제 복구 검증

재사용: [#98](https://github.com/eightmm/oh-my-setting/pull/98),
[#101](https://github.com/eightmm/oh-my-setting/pull/101),
[#102](https://github.com/eightmm/oh-my-setting/pull/102),
[#105](https://github.com/eightmm/oh-my-setting/pull/105)의 stdlib CPU resume smoke.

- [ ] #105의 demo·고장 주입 self-test를 검토하고 실제 프로젝트의 save/load/step adapter에 연결한다.
- [ ] 중단·재개 후 step, optimizer, scheduler, RNG 및 해당 fixture의 data-order 상태를 비교한다.
- [ ] best 선정과 final split 사용 조건을 metric contract에 연결한다.
- [ ] 파일 수·크기 감사와 학습 상태의 복구 가능성 검증을 별도로 표시한다.
- [ ] seed 기본값이 실행 식별·집계·불확실성 평가를 대신하지 않도록 문서를 유지한다.

완료 기준: 연속 실행과 중단 후 재개가 fixture의 허용 오차 안에서 일치하고, 손상된 checkpoint를 조용히 수용하지 않는다.
#105의 내장 demo는 adapter를 교체하기 전에는 harness 자체만 검증한다.
data-order 복원, 실제 signal/preemption, 선택적 torch adapter의 미검증 범위도 유지한다.
실제 대규모 학습/GPU 비용을 이 PR 묶음의 검증 전제로 만들지 않는다.

### F09 — 전체 흐름 replay와 작업 품질 평가

재사용: 기존 runtime·operator·review fixture, graph recall benchmark.
평가 runner를 새로 도입하기 전에 기존 테스트 등록과 artifact 형식을 사용한다.

| 시나리오 | 검증할 결과 |
|---|---|
| 기준 변경 후 resume | 이전 evidence가 현재 기준을 증명한 것으로 남지 않음 |
| 다른 scope의 성공 검증 | finding이 verified로 승격되지 않음 |
| seed 재실행·표시 정렬 | 대표 실행과 집계값이 동일 |
| 중단·재시작·중복 event | task 전환·부수 효과·budget 소비가 중복되지 않음 |
| native 세션 종료·pane 재사용 | 오래된 입력 대상과 새로운 소유자를 구분 |
| 동시 hook·worker·heartbeat와 재시작 | 공유 요청 한도·장애 상태가 유지되고 중복 전송·예약이 없음 |
| context 재선택 | source와 정책 digest, 사용한 budget·누락 근거를 추적 |
| task 화면 조회 | canonical source와 화면 상태가 일치 |

기록할 값은 완료율·잘못된 상태 확정 횟수·중복 행동·복구 성공률·검색 근거 포함률·조회 지연이다.
실제 모델 성능·비용 주장은 이후 동일 과제·모델·예산의 baseline이 있을 때만 한다.
CI 성공, 작은 context, 많은 명령을 곧바로 품질 향상으로 간주하지 않는다.

## 4. 외부 프로젝트에서 가져올 패턴과 근거

아래는 설계 참고자료다. 외부 프로젝트의 결함이 OMS에도 있다는 주장이나 코드 복사 승인이 아니다.
코드를 채택한다면 해당 commit과 license를 구현 PR에서 별도로 확인한다.

| 참고 자료 | 가져올 패턴 | 연결 작업 |
|---|---|---|
| [OMC](https://github.com/Yeachan-Heo/oh-my-claudecode), [OMX](https://github.com/Yeachan-Heo/oh-my-codex) | 단계별 실행 책임과 복귀 시 목표·검증 복원 | F02·F04 |
| [OmO](https://github.com/code-yeongyu/oh-my-openagent), [OMP AgentHub](https://github.com/can1357/oh-my-pi/blob/main/docs/agent-hub.md) | 역할 routing·fallback, agent 계층과 가능한 조작 | F04·F05 |
| [AgentDeck](https://github.com/asheshgoplani/agent-deck), [dmux](https://github.com/standardagents/dmux), [Claude Squad](https://github.com/smtg-ai/claude-squad) | task·worktree·세션 탐색과 짧은 조작 경로 | F04 |
| [cmux 알림](https://cmux.com/docs/notifications), [Conductor 리뷰](https://www.conductor.build/docs/guides/review-and-merge), [Paradev](https://github.com/ParadevAI/Paradev) | unread/focus, finding–diff–CI 연결 | F02·F04·F05 |
| [Aider repo map](https://aider.chat/docs/repomap.html), [OpenHands condenser](https://docs.openhands.dev/sdk/arch/condenser) | budget 안의 코드 관계 선택, 원본 event와 context view 분리 | F03 |
| [Pi usage](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/usage.md), [OpenCode snapshots](https://opencode.ai/docs/snapshots/), [ACP](https://agentclientprotocol.com/protocol/v1/overview) | native steering·복구 범위·capability와 permission 연결 | F05 |
| [LangGraph interrupt](https://docs.langchain.com/oss/python/langgraph/interrupts), [Temporal replay](https://docs.temporal.io/develop/python/best-practices/testing-suite), [Inspect AI logs](https://inspect.aisi.org.uk/eval-logs.html) | 재개 부수 효과, history replay, 평가 근거 | F09 |
| [DVC 비교](https://doc.dvc.org/user-guide/experiment-management/comparing-experiments), [MLflow dataset](https://mlflow.org/docs/latest/ml/dataset/), [Hydra 설정 기록](https://hydra.cc/docs/1.3/tutorials/basic/running_your_app/working_directory/) | 실험 변수·평가 조건·입력 출처 | F06 |
| [Snakemake Slurm](https://snakemake.github.io/snakemake-plugin-catalog/plugins/executor/slurm.html) | 자원 요청·상태 조회·accounting 연결 | F07 |

외부 PR에서 확인한 실패 시나리오:

| PR | 조사 시점 상태 | OMS에서 검증할 내용 |
|---|---|---|
| [AgentDeck #3092](https://github.com/asheshgoplani/agent-deck/pull/3092) | merged | batch timeout 뒤 per-pane fallback 폭증 방지, stale 관찰 표시 |
| [AgentDeck #3094](https://github.com/asheshgoplani/agent-deck/pull/3094) | merged | skipped/muted 전송을 delivered로 기록하지 않음, 동시 heartbeat 중복 방지 |
| [OMX #4067](https://github.com/Yeachan-Heo/oh-my-codex/pull/4067) | merged | 오래된 알림의 pane/session 소유권을 입력 직전에 다시 검증 |
| [Vibe Kanban #3487](https://github.com/BloopAI/vibe-kanban/pull/3487) | merged | provider별 permission bridge를 끝까지 검증 |
| [Paradev #9583](https://github.com/ParadevAI/Paradev/pull/9583) | open | background-only 대기에서 사용자의 입력을 무한 queue하지 않음 |
| [OMC #4939](https://github.com/Yeachan-Heo/oh-my-claudecode/pull/4939) | open | 일회성 hook·worker 사이에 요청 제한과 장애 상태를 공유 |

열린 외부 PR은 출시된 동작으로 취급하지 않는다.
Vibe Kanban은 [공식 운영 종료 안내](https://www.vibekanban.com/blog/shutdown)에 따른
커뮤니티 유지보수 상태를 도입 검토에서 구분한다.

## 5. 구현 순서와 에이전트 인계

1. F00·F01·F02a 수정 PR과 #87의 정확성을 우선 검토한다.
2. F02b와 F03을 각각 기존 PR 위에서 연결한다. 공통 receipt/context 계약은 먼저 확정한다.
3. F04는 canonical reader를 재사용하고 F05의 capability·identity 정보와 연결한다.
4. F06b는 #87의 입력 식별·집계 결과를 재사용한다. #62의 이미 반영된 코드를 다시 추가하지 않는다.
5. F07·F08은 #104·#105를 재사용하고 synthetic scheduler·실제 프로젝트 CPU adapter로 남은 경계를 검증한다.
6. F09는 각 변경의 실패 시나리오를 기존 fixture에 추가한다. 무관한 새 CI matrix를 만들지 않는다.

각 구현 PR에는 다음을 남긴다.

- 실제 base와 선행 PR/commit, 기존 구현을 재사용한 위치.
- 관측된 오류·정적 추론·미검증 가정의 구분.
- 수정 전 실패와 수정 후 통과를 보여주는 canonical test 및 실행 환경.
- legacy 데이터의 동작, 변경된 공개 계약과 reader의 호환성.
- affected 검사와 CI 결과. 개별 PR의 성공을 합친 stack의 성공으로 간주하지 않음.
- 남은 작업과 rollback 범위. 기록 삭제·설치 변경·실제 학습 제출을 구현 완료처럼 표시하지 않음.

인계할 작업 문구:

> 최신 main과 이 문서의 관련 F 항목, 선행 PR의 현재 diff를 먼저 대조하라.
> 이미 들어간 기능은 재구현하지 말고 명시된 연결·완료 기준만 구현하라.
> 기존 canonical test를 확장하고, 상태를 추정하거나 evidence·approval authority를 만들지 마라.
> fake provider/private Git 또는 tmux/고정 Slurm 응답/작은 CPU fixture로 검증하라.
> 실제 모델 호출·GPU·cluster 변경이 필요해지는 경계는 미검증으로 보고하라.
> 코드와 검증 결과를 별도 PR로 반환하고 main 반영·설치는 별도 권한으로 남겨라.
