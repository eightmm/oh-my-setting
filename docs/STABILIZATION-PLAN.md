# OMS 안정화 작업계획

검토일: 2026-11-08 (KST)

기준: v0.7.0, [`acf4c5a`](https://github.com/eightmm/oh-my-setting/commit/acf4c5a5f7b4fee25e77ce1fd8602d3703a1e306)

상태: S1–S6 구현을 아래 PR로 제출했다. 아직 병합하지 않았으며, 로컬 집중 검증과
전체 통합 CI의 완료 여부를 구분한다. 아래 완료 조건은 최종 수락용 checklist로
보존하고, 해당 구현 PR의 CI와 병합을 확인한 뒤 완료 처리한다.

## 구현 현황

| 작업 | 구현 PR | 로컬에서 확인한 근거 | 남은 확인 |
|---|---|---|---|
| S1, S4 | [#12 — task와 evidence 유효성](https://github.com/eightmm/oh-my-setting/pull/12) | 취소 전환 회귀, lease/reopen 3개 smoke; freshness·비용·lineage 등 runtime 8개 case | 전체 CI; 완료 task 재활성화 통합 case |
| S2, S5 | [#11 — 실제 모델 계열과 리뷰 identity](https://github.com/eightmm/oh-my-setting/pull/11) | 모델 계열 및 실제 routing 호출 2개 집중 case; 수정 전 실패 재현 | 전체 auto-review integration과 CI |
| S3, S6 | [#13 — 공개 CI 조회와 업데이트 증빙](https://github.com/eightmm/oh-my-setting/pull/13) | 전체 update-v04·install-profile·문서 검사; 기존 land CI/scope 16개 사례 | 전체 land·native/portability CI |

#12와 #13은 #11의 기존 자동 리뷰 fixture 수정을 기반으로 한 별도 PR이다.
병합 순서는 #11을 먼저 적용하고, 이후 #12와 #13을 각각 검토한다. 두 후속 PR 사이에는
구현 의존성이 없다. 이 계획 문서 PR은 기능 변경과 독립적이다.

로컬 통합 검사에서는 process identity/reference 등록 실패가 발생했고,
full 선택의 committed-range gate는 ShellCheck가 없어 중단했다. 해당 검사를 통과로
표시하거나 guard를 완화하지 않았다. 각 PR에는 실행한 명령과 실패 범위를 기록했다.

호환성상 달라지는 부분은 기본 evidence binding의 선택적 `verified_state`다.
기존 `schema: 1`과 명시적 dependency 의미를 유지하며, 지문 없는 오래된 verified
binding은 현재 worktree의 검증을 입증하지 못하므로 inconclusive로 표시한다.
S6은 exact-SHA conclusion/scope 조회를 공유하되 자동 업데이트의 affected 성공 허용,
land의 full-proof 요구, fresh/manual edge 예외를 각각 유지한다.

## 목표와 범위

단일 Main, native CLI, tmux, 격리 worktree, 검증과 landing 흐름을 유지하면서
상태 전환, evidence freshness, 모델 식별, 설치 검증 기준의 불일치를 줄인다.
기존 writer와 receipt를 재사용한다. 별도 상태 DB, 새 orchestration 계층,
추가 provider, 자동 모델 최적화는 이 안정화 범위에 포함하지 않는다.

검증은 임시 Git 저장소, 가짜 provider/HTTP 응답, 기존 회귀 테스트를 기본으로
한다. 실제 연구 프로젝트, 유료 추론, GPU 학습, Slurm 제출, 장시간 생산성
benchmark는 완료 조건이 아니다. 기본 운영은 기존 구독의 native CLI를 유지한다.

이 문서는 기준 커밋의 관찰을 후속 구현자가 사용할 수 있도록 고정한 기록이다.
작업 시작 시 최신 코드에서 해당 문제가 남아 있는지 먼저 확인하고, 이미 해결됐다면
수정 커밋과 검증 근거를 연결해 닫는다. 상태 파일을 직접 편집해 재현을 우회하지 않는다.

## 실행 순서

| ID | 우선순위 | 작업 | 근거의 강도 | 완료 시 기대 결과 |
|---|---|---|---|---|
| S1 | P1 | 취소 task의 잘못된 block 전환 차단 | 정상 CLI 조합으로 재현 | 한 task의 전환이 전체 plan 조회를 손상시키지 않음 |
| S2 | P1 | 자동 리뷰 fixture의 Main/writer identity 정리 | 현재 CI 로그와 코드 대조 | 독립 리뷰 정책을 유지하면서 해당 fixture와 CI 복구 |
| S3 | P2 | core 설치와 자동 업데이트의 gh 의존성 정리 | 설치 계획·호출 경로·없는 실행 파일 probe 확인 | 최소 설치에서도 업데이트 가능 여부와 실패 이유가 정확함 |
| S4 | P2 | evidence freshness와 실제 작업 상태 일치 | 미커밋 변경으로 재현 | 오래된 검증을 현재 verified로 표시하지 않음 |
| S5 | P2 | harness와 실제 model family 분리 | routing 함수 수준 재현 | UI 종류가 달라도 실제 모델 계열로 독립성 판단 |
| S6 | P2 | 설치 경로별 검증 계약 명시·공통화 | 설계·정책 차이 확인 | 설치 근거와 예외가 경로마다 숨겨져 있지 않음 |

P1은 우선 처리할 운영 장애, P2는 이어서 처리할 일관성 문제다.
이 표는 보안 심각도나 실서비스 피해 발생 여부를 뜻하지 않는다.

권장 구현 묶음은 S1, S2, S4/S5, S3, S6 순서다. S4와 S5는 독립적으로 진행할 수 있다.
S6은 S3의 CI 조회 경로를 재사용한다. 서로 다른 기능을 하나의 큰 수정 PR로 합치지 않는다.

## S1. 취소 task의 block 전환 차단

**관찰:** 임시 저장소에서 t1을 유효한 승인·digest·decision artifact로 취소한 뒤
`agent-plan block --id t1 --reason "reassess"`를 실행하면 성공으로 보고한다. 이후 `status --json`과
관계없는 t2의 `show`가 모두 exit 2로 실패한다.

```text
cancel t1       -> exit 0, cancelled
status         -> exit 0
block t1       -> exit 0, blocked
status/show t2 -> exit 2, cancellation evidence without cancelled state
```

원인은 block writer가 cancellation receipt를 남긴 채 상태를 바꾸고, reader는
그 조합을 거부하는 것이다. 관련 구현은
[agent_plan_core.py](../scripts/lib/agent_plan_core.py)의 `block` 분기와
`validate_cancellation()`이다.

**최소 수정:** cancelled task에 대한 block을 저장 전에 거부한다. 취소 task를
다시 여는 기능은 이번 수정에 추가하지 않는다. 기존 완료 task의 명시적 재활성화
계약과 다른 정상 전환은 보존한다.

**완료 조건**

- [ ] block이 nonzero로 끝나고 plan bytes가 변경되지 않는다.
- [ ] 거부 이후 status와 unrelated task 조회가 정상 동작한다.
- [ ] cancellation receipt와 decision artifact가 보존된다.
- [ ] 기존 claim/lease 및 완료 task 전환의 의미가 유지된다.

**검증 위치:** [scripts-smoke.sh](../tests/scripts-smoke.sh)의
`test_agent_plan_cancellation_is_closed_not_verified_and_cas_fenced`에 위 assertion을
추가한다. 실제 에이전트 없이 canonical CLI를 사용한다.

## S2. 자동 리뷰 fixture의 실행 identity 정리

기준 커밋의 [CI run #909](https://github.com/eightmm/oh-my-setting/actions/runs/42183737195)는
`focused (2/4)`의 `auto_review.py`에서 reviewer dispatch 1회를 기대했지만
실제 호출이 없어 실패했다. 직전 메시지는 OpenAI 계열의 독립 reviewer가 없다는
내용이다. 다른 smoke shard와 설치 lifecycle 등이 모두 실패한 상황은 아니다.

**원인 판단:** fixture의 Claude Main, 실행 환경에서 추론한 caller, 설치된 것으로
설정한 provider 목록이 일치하지 않는다. 이 판단은 CI 로그와 코드 대조에 근거하며,
올바르게 바인딩된 실제 CLI의 자동 리뷰가 전부 실패한다는 뜻은 아니다.

**최소 수정:** fixture에서 Main identity와 실제 patch writer의 모델 계열을
명시한다. 독립 reviewer가 없는 경우와 있는 경우의 기대 결과를 구분한다.
동일 계열 심사를 자동 허용하도록 production guard를 완화하지 않는다.

**완료 조건**

- [ ] 독립 reviewer가 준비된 fixture에서는 동일 결과를 두 번 전달해도 dispatch는 1회다.
- [ ] 독립 reviewer가 없는 fixture에서는 dispatch 0회와 명확한 검토 불가 결과를 남긴다.
- [ ] Main과 실제 writer가 다를 때 writer의 모델 계열을 기준으로 한다.
- [ ] 실제 provider 설치 여부, caller 환경, 개인 HOME에 따라 결과가 달라지지 않는다.
- [ ] 기존 실패 lane을 해당 수정 SHA에서 다시 확인한다. 문서 PR의 green과 구분한다.

**검증 위치:** [auto_review.py](../tests/operator/auto_review.py),
[fixtures.py](../tests/operator/fixtures.py),
[provider_fixture.py](../tests/operator/provider_fixture.py).
가짜 provider를 사용하는 기존 `--section auto-review` 경로를 재사용한다.
로컬 검토에서는 CI assertion에 도달하기 전에 HOME 경로에 의존하는 다른 assertion이
실패했으므로, 두 실패를 같은 재현이라고 기록하지 않는다.

## S3. 최소 설치의 자동 업데이트 의존성 정리

**관찰:** [core profile](../config/capability-profiles.json)은 gh를 필수로 설치하지
않는다. [installer](../install.sh)는 자동 업데이트를 기본 활성화하지만,
[ci-query.py](../scripts/lib/ci-query.py)는 gh로만 CI를 조회한다.
기존 gh가 없는 호스트에서는 새 버전의 자동 적용이 계속 건너뛰어질 수 있다.
이미 gh가 준비된 호스트는 이 재현 조건에 해당하지 않는다.

**권장 수정:** 공개 OMS 저장소의 제한된 CI 조회를 선택적인 사용자 GitHub 통합과
분리한다. 기존 CI helper 안에서 공개 저장소 조회 fallback을 지원하는 방안을 우선
검토하고, gh·인증이 없는 상태를 일반적인 CI failure로 뭉개지 않는다. 지원할 수
없는 경우에는 설치 계획과 doctor에서 자동 업데이트가 준비되지 않았음을 알린다.
자동 업데이트 타이머나 설치 profile의 기본값을 조용히 바꾸지 않는다.

**완료 조건**

- [ ] gh 없음·인증 불가·CI pending·CI failure·성공을 구분한다.
- [ ] 성공 근거는 정확한 대상 SHA와 workflow에 묶인다.
- [ ] 네트워크/응답 파싱 오류를 성공으로 취급하지 않고 시간·응답 크기를 제한한다.
- [ ] gh가 있는 기존 경로와 dirty checkout 보호가 유지된다.
- [ ] 최소 설치에 사용자 GitHub 로그인이나 추가 유료 서비스를 강제하지 않는다.

**검증 위치:** [update-v04-smoke.sh](../tests/update-v04-smoke.sh)의 자동 업데이트
fixture와 [install-profile-smoke.sh](../tests/install-profile-smoke.sh)의 최소 설치 검사.
없는 바이너리와 가짜 응답을 주입한다. 실제 GitHub 요청·홈 설치는 회귀 테스트에
필요하지 않다. 현재 update-v04 suite에는 `--only`가 없으므로 존재하지 않는 선택
실행 명령을 만들지 않는다.

## S4. evidence freshness의 일관성

**관찰:** [evidence.py](../scripts/lib/oms_runtime/evidence.py)의 기본 binding은
`--depends`가 없으면 HEAD만 저장한다. tracked 구현 파일을 깨뜨려도 커밋하지
않으면 criterion은 verified로 남는 반면, 같은 envelope의 task verification은
stale이 된다. 현재 확인된 영향은 projection·표시·다음 행동의 불일치다.
별도의 close/admission/landing gate를 우회했다는 증거는 없다.

**권장 수정:** 기존 작업 상태 fingerprint를 공유한다. 명시적 dependency가 있으면
그 범위의 freshness를 검사하고, 생략됐으면 HEAD와 검증 대상 작업 트리 상태를
함께 묶는다. 오래된 receipt에 필요한 fingerprint가 없을 때의 상태도 명시한다.
읽기만으로 기존 기록을 재작성하거나, 근거 없는 legacy 행을 fresh로 승격하지 않는다.

**완료 조건**

- [ ] 동일 HEAD의 relevant unstaged/staged 변경이 기존 검증을 무효화한다.
- [ ] 명시적 dependency 변경·삭제를 검사하는 기존 동작이 유지된다.
- [ ] 명시적 dependency와 관계없는 변경은 기존 scope 계약에 맞게 처리한다.
- [ ] ignored `.oms` receipt/cache 기록이 binding 자체를 stale로 만들지 않는다.
- [ ] 동일 envelope에서 task와 criterion의 freshness 근거를 설명할 수 있다.
- [ ] fingerprint 계산을 envelope당 공유해 evidence 행 수만큼 Git 호출이 늘지 않는다.
- [ ] legacy receipt의 불확실성을 표시하며 별도 landing gate는 그대로 유지한다.

**검증 위치:** [test_oms_runtime_core.py](../tests/test_oms_runtime_core.py)의
`test_explicit_binding_and_dependency_staleness`,
`test_projection_subprocess_cost_does_not_scale_with_evidence_rows` 및 기존
[RuntimeFixtureBase](../tests/runtime_test_base.py)를 확장한다.
임시 Git 저장소의 파일 변경만으로 확인할 수 있다.

## S5. harness와 model family의 구분

**관찰:** [panel_routing.py](../scripts/lib/panel_routing.py)는 독립 리뷰에서
Claude owner를 Anthropic 계열로 먼저 확정한다. 그런데
[codex_harness.py](../scripts/lib/codex_harness.py)는 Claude UI로 GPT를 실행할 수 있다.
`owner=claude`, `owner_model=gpt-6.1-sol`인 함수 호출에서 judged family가 Anthropic이
되어 같은 GPT reviewer를 선택하는 결과를 재현했다. standalone launcher와 panel
launcher는 별도 경로이며, 실제 live panel 오심사까지 재현한 것은 아니다.

**최소 수정:** 기록된 실제 model family를 우선하고, harness/owner는 소유권과 UI
식별에 사용한다. 이미 있는 registry 분류기를 재사용한다. transport 확인값,
선택한 모델, 불명확한 fallback을 구분하고 unknown을 독립성의 증거로 쓰지 않는다.

**완료 조건**

- [ ] Claude harness + GPT와 Pi + GPT가 동일한 OpenAI 계열로 판단된다.
- [ ] native Claude + Claude 모델은 기존 Anthropic 판정을 유지한다.
- [ ] linked writer가 있으면 Main이 아니라 해당 writer의 모델 계열을 사용한다.
- [ ] 기존 unknown/모순 기록의 거부 정책을 유지한다. 모델 미기록 native legacy
  동작은 이번 회귀 수정에서 불필요하게 바꾸지 않는다.
- [ ] 사용자가 명시한 same-family 선택은 기존 표시·권한 계약을 보존한다.
- [ ] owner, room identity, 인증 환경을 바꿔 family 오류를 우회하지 않는다.

**검증 위치:** [auto_review.py](../tests/operator/auto_review.py)의
`writer-model-independence` case에 Claude harness + GPT의 Main/writer 경로,
독립 reviewer 부재, 명시적 same-family 선택을 추가한다. 환경 변경이 있으면
[test_codex_harness.py](../tests/test_codex_harness.py)의 launch 환경 테스트를 재사용한다.
`allocate()`와 가짜 catalog만으로 핵심 판정을 검증할 수 있다.

## S6. 설치 검증 기준의 공통화

이 항목은 확정된 충돌 버그와 구분하는 정책 정리다. 현재 경로는 다음처럼 다르다.

| 경로 | 기준 커밋에서 확인한 검증 |
|---|---|
| land 후 설치 | 같은 SHA의 성공 CI와 full local/full CI 근거 |
| 자동 업데이트 | 같은 SHA의 성공 workflow; affected-only 성공도 가능 |
| 새 설치 | 기본 edge/default branch 선택 |
| 수동 update | 선택 ref 적용과 link/doctor; 자체 CI 성공 확인 없음 |

**먼저 할 일:** [설치 안내](INSTALL.md), [설치·검증 계약](COMPONENTS.md),
[ci-query.py](../scripts/lib/ci-query.py), [auto-update.sh](../scripts/auto-update.sh),
[land.sh](../scripts/land.sh)에서 허용할 검증 근거와 예외를 한 표로 확정한다.
그다음 대상 SHA·검증 범위·허용 여부·이유를 같은 helper로 계산하고 각 경로에서 사용한다.
CI success와 full 검증을 같은 의미로 표시하지 않는다.

**권장 경계:** land의 기존 full-proof 조건은 유지한다. 자동 적용에 요구할 검증
범위와 수동 edge 설치의 예외를 명시적으로 결정한다. 현재 source installer의 edge
기본값은 이미 회귀 계약으로 고정되어 있으므로 이 계획을 근거로 임의 변경하지 않는다.
GitHub Releases나 새 배포 시스템을 도입하지 않는다. 공개 flag, 기본값, receipt schema를
바꿔야 한다면 실제 변경 전에 영향 범위와 migration을 해당 구현 PR에 제시한다.

**완료 조건**

- [ ] full/affected-only/실패/미확인에 대한 각 경로의 허용 결과가 명확하다.
- [ ] 설치가 허용된 이유와 실제 검증 범위를 출력·기록한다.
- [ ] target SHA가 바뀌면 이전 CI 근거를 재사용하지 않는다.
- [ ] 실패·조회 불가·scope 불명확을 검증 성공으로 표시하지 않는다.
- [ ] dirty tree 보호와 기존 rollback·ownership 동작이 유지된다.

**검증 위치:** [land-smoke.sh](../tests/land-smoke.sh),
[update-v04-smoke.sh](../tests/update-v04-smoke.sh)의 기존 가짜 gh/remote fixtures.
순수 판단표를 작은 테스트로 먼저 확인하고, 바뀐 경로의 기존 통합 suite만 실행한다.

## 과도하지 않은 검증 계획

| 단계 | 실행할 것 | 통과가 증명하는 것 |
|---|---|---|
| 이번 문서 PR | 변경 diff, 문서 경로·참조, docs-only affected check | 계획 문서가 저장소와 연결됨 |
| 개별 수정 | 기존 fixture에 회귀 assertion을 추가하고 해당 case 실행 | 재현한 결함과 가까운 경계가 고쳐짐 |
| 구현 PR | committed range의 affected check와 기존 필수 CI | 변경 범위에 필요한 회귀 계약 유지 |
| 필요할 때만 실제 연결 | 영향받은 CLI 하나에서 작은 임시 작업 1회 | 해당 설치·계정·CLI 버전의 제한된 호환성 |

개발 중에는 짧은 case를 먼저 실행하고, 관련 입력을 바꾸지 않은 상태에서 같은
full suite를 반복하지 않는다. 실행 시간이 예상보다 길면 어떤 suite가 선택됐는지
먼저 확인한다. 기존 필수 gate가 full을 요구하면 해당 범위는 유지하며, 임의로
timeout을 성공으로 바꾸거나 검증을 생략하지 않는다. 새 CI job/matrix는 만들지 않는다.

### 기존 선택 실행 명령

아래는 후속 구현자가 저장소 루트에서 사용할 기존 entrypoint다. 이 문서 PR에서
모든 명령을 실행했다는 의미는 아니다.

```bash
# S1: 기존 cancellation case에 회귀 assertion을 추가한 뒤
bash tests/run-smoke-shard.sh --only test_agent_plan_cancellation_is_closed_not_verified_and_cas_fenced

# S2: 가짜 provider를 사용하는 기존 자동 리뷰 section
bash tests/operator-tools-smoke.sh --section auto-review

# S5: 기존 모델 계열 case만 실행
bash tests/operator-tools-smoke.sh --section auto-review --case writer-model-independence

# S4: 기존 evidence freshness와 호출 비용 case
PYTHONPATH=tests:scripts/lib python3 -m unittest \
  test_oms_runtime_core.RuntimeFixture.test_explicit_binding_and_dependency_staleness \
  test_oms_runtime_core.RuntimeFixture.test_projection_subprocess_cost_does_not_scale_with_evidence_rows

# Bridge 환경·프로토콜을 실제로 수정한 경우의 offline suite
python3 tests/test_codex_harness.py

# 커밋 후 실제 변경 범위 검증: BASE_COMMIT은 해당 PR의 기준 SHA로 설정
bash scripts/check.sh --affected --changed-from "$BASE_COMMIT" --changed-to HEAD
```

### 실제 작업 테스트를 필수로 하지 않는 이유

S1/S4의 상태·freshness와 S5의 family 판정은 모델 응답과 무관한 결정적 계약이다.
S2/S3/S6도 가짜 provider·CI 응답·임시 저장소로 실패 조건을 통제할 수 있다.
따라서 이 결함들을 고치는 데 실제 연구 저장소의 변경, GPU 작업, Slurm 제출,
여러 CLI의 대규모 비교를 요구하지 않는다.

인증 RPC, 스트리밍 변환, CLI lifecycle 이벤트 자체를 바꾼 경우에는 fake만으로
실제 upstream 호환성을 증명할 수 없다. 이때만 이미 허용된 계정·실행 범위에서
작은 임시 저장소의 파일 읽기나 수정 및 로컬 검사 1회를 선택적으로 확인한다.
실행할 수 없으면 native 호환성은 미검증으로 남기고, 필요한 경우 experimental
상태를 유지한다. 이를 offline regression 실패나 검증 완료로 혼동하지 않는다.

### 효과 측정은 별도 프로젝트로 키우지 않는다

실제 업무 A/B benchmark와 절감률 수집은 이번 안정화의 선행 조건이 아니다.
원하면 평소 작업 중 이미 생성되는 receipt에서 재작업, 복구, 명확한 overhead
사례만 참고한다. 전용 업무를 만들거나 token/quota 절감 수치를 추정해 보고하지 않는다.

## 검토 당시 확보한 근거

| 확인 | 기준 커밋의 결과 | 해석의 한계 |
|---|---|---|
| Runtime research suite | 22 tests 통과 | live Slurm/연구 workflow 증거 아님 |
| Codex harness offline suite | 17 tests 통과 | fake auth/HTTP/SSE; live provider 증거 아님 |
| Claim·stale lease case | 2 tests 통과 | 모든 동시성·복구 분기를 검증한 것은 아님 |
| S1 취소 후 block | canonical CLI로 재현 | 임시 Git 저장소 |
| S4 미커밋 변경 | criterion verified와 task stale 불일치 재현 | landing 우회 입증 아님 |
| S5 모델 계열 | 순수 routing 함수로 재현 | 실제 panel 오심사 입증 아님 |
| CI #909 | focused 자동 리뷰 실패 | 전체 설치·실행 기능 실패로 일반화하지 않음 |
| 로컬 auto-review 제한 실행 | CI보다 앞선 환경 의존 assertion 실패 | CI 실패의 동일 재현 아님 |

이 표는 2026-11-08 검토의 기록이다. 후속 구현의 통과 근거로 재사용하지 않는다.
각 작업을 닫을 때 수정 SHA, 실제 실행한 command/case, 결과, 미검증 범위를 남긴다.

## 후순위 항목

- Active evidence binding의 원본 receipt가 prune된 뒤 어떤 상태로 남아야 하는지
  보존 계약을 정한다. parent attestation을 독립적으로 인정하는 설계일 수 있어
  즉시 landing 결함으로 단정하지 않는다. 기존 retention을 무조건 확대하지 않는다.
- Bridge 문서의 effort 기본값과 retry 설명을 현재 구현·테스트에 맞춘다.
- Pi 최소 버전/event capability 확인은 Pi 연결 경로를 수정할 때 검토한다.

이 항목들 때문에 S1–S6을 지연하거나 실제 업무 실험을 필수화하지 않는다.
