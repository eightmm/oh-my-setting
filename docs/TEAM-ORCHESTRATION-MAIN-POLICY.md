# OMS Main: authority, harness and model-selection policy

작성: 2026-11-17. **DESIGN / RECOMMENDATION ONLY**.
Source baseline: `de2f547c6a40ddf67c62068bce309050c6d4fa48`.
관련: [team panel](TEAM-ORCHESTRATION-PANEL.md),
[#245 architecture](https://github.com/eightmm/oh-my-setting/pull/245),
[#248 validation](https://github.com/eightmm/oh-my-setting/pull/248).
모델 추천은 OMS 실측 순위가 아니다. 이 문서를 병합해도 사용자의 선택 모델,
계정, 과금, 자동 swap 설정, provider 권한 또는 기본값을 변경하지 않는다.

**현재 구현 범위** — 새 Main은 설치된 native Codex, Claude Code, AGY에서 고를 수 있다.
Pi·Codex·OMS가 설치된 경우 picker나 `--launch pi`로 Codex gateway를 쓰는 Pi Main을
열 수 있다. 활성 Main은 다시 연결한다. Pi와 AGY에서 종료된 세션을 복구하려면 검증된
native session binding이 필요하다. 지원되는 옵션과 재접속 조건은
[Main 선택과 복구](TERMINAL-PANEL.md#default-main-selection)에 기록했다. 아래 모델과
평가 기준은 설계에 해당한다.

## 1. Main은 특정 회사의 모델이 아니라 하나의 책임 역할이다

`Main authority != harness != endpoint/provider != selected model != served model`.
하나의 goal/room/integration target에는 root owner Main 하나를 유지한다.
Main이 Codex라고 하위 팀을 Codex로 제한하지 않으며, Claude Main도 같은 bounded
OMS runtime으로 Codex·Claude·AGY 팀을 배정할 수 있어야 한다.
모델이나 CLI 이름이 task ownership/approval/admission 권한을 부여하지 않는다.
같은 provider의 Overall Main과 Provider Lead도 별도 세션이다. Codex Main은 같은
Room에서 Codex Lead를 열 수 있다.

Main은 사용자의 목표 해석, scope/acceptance 확정, task 분해, 자원 배분 방침,
충돌 해결, 예외 판단과 최종 설명에 집중한다. 기존
[orchestrator skill](../custom-skills/oms-panel-orchestrator/SKILL.md)의 역할 분리를 따른다.

2026-11-22 정책: 팀을 운영하는 Main은 **관리 전용**이다. Main은 결정하고 Worker가 만든다.
Main은 저장소 파일을 수정하지 않으며 코드·테스트·문서·설정 변경은 크기와 무관하게
Worker나 Lead에게 배정한다. 실패한 검사와 리뷰 지적은 원래 작성자에게 `--continue`로,
충돌과 slice 사이 연결 수정은 `integrator`에게, 넓은 소스 조사는 `researcher`에게 보낸다.
Main이 직접 하는 일은 OMS 명령 실행, 반환된 증거와 결정적 diff 확인, 결정적 검증 명령 실행,
admission·acceptance·publication 판단과 사용자 보고다. 예외는 사용자가 해당 변경을 Main이
직접 하라고 지시한 경우뿐이며 Room에 기록한다. Worker를 시작할 수 없으면 직접 처리하지 않고
사용자에게 blocker를 보고한다. Provider Lead는 한 배정의 팀리더로서 native child에게 실질
작업을 맡기고 자기 범위의 통합·작은 unblocking 수정만 직접 한다. 계층별 책임표는
[coordination layers](../custom-skills/oms-agent-harness/references/coordination-layers.md)에 있다.
Claude Code Main에서는 [Main write guard](COMPONENTS.md#main-write-guard)가 관리 대상 저장소와
그 linked worktree 안의 Edit/Write 계열 호출을 거절한다. Bash를 통한 변경과 Codex·AGY Main은
여전히 프롬프트·스킬 계약에만 의존한다. 사용자가 Main에게 직접 변경을 지시한 경우에는 사용자가
자기 셸에서 수정하거나 `OMS_MAIN_WRITE_GUARD=0`으로 패널을 시작한다.

실제 claim, slot/budget reservation, dependency readiness, process wait/cancel,
receipt 기록, evidence freshness와 승인 검사는 Rust runtime이 담당한다.
단순 상태 확인마다 LLM을 깨우거나 모든 Worker 메시지를 Main prompt에 복사하지 않는다.
Native provider의 agent/tool loop를 OMS 안에 다시 구현하지 않는다.

## 2. 초기 평가 후보의 기록과 한계

아래 표는 2026-11-17 설계에서 정한 **평가 후보의 기록**이다. 현재 계정의 선택 가능
집합이나 강제 기본값을 정하지 않는다. 새로운 명시 선택은 실제 계정 catalog와 지원 effort,
권한·과금 조건을 기준으로 하며 기존 사용자 선택을 우선한다. 모델명 표를 갱신하는 것만으로
설정이나 실행 모델을 바꾸지 않는다. Tier는 [선택 정책](TEAM-ORCHESTRATION-MODEL-TIERS.md)의
추천 profile로 사용할 수 있고, 현재 schema-2 router의 호환 제약은 별도로 유지한다.

| 역할/상황 | 초기 후보 | 선택 이유 및 조건 |
|---|---|---|
| 일반 Main | Codex + GPT-6.1 Sol | 공식 Codex 지원 모델; 장기 반복 작업과 비용 균형을 위한 평가 시작점 |
| 어려운 전역 설계·모순 해결 | GPT-6 Astra를 bounded advisor로 호출 | Main identity를 바꾸지 않고 검토를 받는 안; explicit account/usage 권한과 capability 필요 |
| Claude를 Main으로 선호하는 운영 | Claude Code + Claude Opus 5.5 | 유효한 대안; 현재 선택이 만족스러우면 단지 표준화 때문에 강제 전환하지 않음 |
| 매우 어려운 장기 분석 | Claude Fable 5.2 | 명시 선택·접근/usage-credit 확인 후 평가; 기본 자동 fallback 후보로 넣지 않음 |
| AGY 기반 실행 팀 | Gemini 4 Carbon 또는 검증된 Orchestrator 경로 | provider-native team adapter가 기능·관측·예산 경계를 입증한 뒤 사용 |

공식 문서는 OpenAI의 Sol을 비용을 고려한 복잡한 장기 작업 후보로, Astra를 더 어려운
작업 후보로 설명한다 [O1]. 이는 Claude/AGY보다 OMS 조정 능력이 높다는 비교 증거가 아니다.
Codex App Server는 모델 catalog와 effort 및 thread/turn 이벤트를 제공한다 [O2].
OMS에서 필요한 bridge가 모두 구현됐다는 뜻은 아니며 #244와 TM-07의 검증이 필요하다.

Claude 공식 문서는 Fable 5.2를 명시 선택하는 장기 작업용 후보로 설명하고, Opus 5.5 등
모델/effort를 별도로 설정하도록 한다 [A1, A2]. Fable은 계정·seat 조건에 따라 usage credits를
사용할 수 있고 headless/일부 SDK 환경에서는 interactive 확인 없이 과금될 수 있다 [A1].
기존 구독이 있다는 사실만으로 추가 지출 권한이 있다고 추정하지 않는다.

AGY Orchestrator는 Carbon/Argon planner와 Hydrogen execution을 결합한 provider 내부
선택지다 [G1]. 이 구조가 전역 OMS Main을 대체하거나 OMS 예산·관측을 자동 충족하지 않는다.
초기에는 outer Main 아래에서 bounded team/task 실행을 맡기는 쪽을 권장한다.
'Carbon 접근 가능'과 'Orchestrator/Swarm 사용 가능'은 같은 entitlement가 아니다.

## 3. Main을 고르는 순서

첫째, 자격 조건을 확인한다. Installed harness/version, 인증된 account/endpoint,
model catalog, supported effort, permission flow, bounded launch/stop, 필요한 event/usage
관측과 안정적인 handoff가 조건을 만족해야 한다. Unsupported/unknown은 capability별로
유지한다. 모델의 benchmark 점수로 부족한 transport 권한을 보완하지 않는다.

둘째, 기존 사용자 선택과 실행 중인 goal의 연속성을 우선한다. '추천'은 클릭해서 적용하는
별도 선택이며 단순 화면 열람, quota 조회, 새 릴리스 발견만으로 Main을 교체하지 않는다.

셋째, 자격을 통과한 후보를 실제 OMS 과제로 비교한다. 평가 이전에는 글로벌 최강이나
정량적 비용 절감으로 홍보하지 않는다. reasoning effort는 모델마다 의미가 다르므로
공통 숫자 하나로 순위를 만들지 않는다.

## 4. Main picker와 지속적인 attribution

[기존 panel](TERMINAL-PANEL.md)의 provider/launch/swap UI에 정보를 덧붙인다.
새 account manager나 별도 model registry를 만들지 않는다.

Picker에는 harness, account alias, exact selected model, supported effort,
auth/billing mode, known quota의 source/age, 필요한 capability, 적용 범위와 제한을 표시한다.
Codex는 지원되는 bridge에서 `model/list` 결과를 사용하며 [O2], 기존 캐시만 있는 경로는
cached/stale 여부를 명시한다. View refresh가 자동 model probe/로그인/inference를 시작하지 않는다.

Main 상세에는 requested/selected/served model과 effort를 구분한다. 자동 fallback 또는
내부 planner/executor 전환이 보고되면 attribution을 갱신한다. 보고되지 않으면
`served: unreported`로 남긴다. Provider 내부 다중 모델을 하나의 실제 모델로 꾸미지 않는다.

'현재 goal만', '다음 goal부터', '저장된 기본값 변경'은 별도 의도다. 구현이 지원하는
범위만 활성화하고 향후 선택을 실행 중 세션에 몰래 적용하지 않는다. Model 선택의
성공은 실제 served model의 증거가 아니다.

Native resume 시 모델 선택 의미는 provider마다 다르므로 adapter가 검증한 규칙으로
동작한다. 잘못된 모델/effort·entitlement·credit requirement는 launch 전에 알리고,
없는 모델을 임의의 싼 모델이나 유료 API로 대체하지 않는다.

## 5. Main 교체와 escalation은 서로 다르다

**Advisor escalation:** 같은 Main 아래에서 bounded 문제·근거·예산으로 다른 모델을 호출한다.
그 답변은 조언이며 owner/approval 권한이나 독립 gate의 자동 통과가 아니다. 저자와 실제
served model family가 같거나 불명확하면 독립 reviewer라고 간주하지 않는다.

**Same-harness model change:** 해당 native session이 지원하는 명시적 turn boundary에서만
적용한다. 새 모델이 실제로 사용됐는지 확인되지 않으면 requested 상태로 남긴다.

**Cross-provider Main swap:** 기존 sole-main handoff 경로를 재사용한다. Root owner,
room, attempt/process generation을 검증하고 이전 Main의 종료와 새로운 ownership을
확인한다. Native history를 provider 간 복사해 resume했다고 주장하지 않는다.
새 Main은 bounded handoff와 canonical plan/evidence/receipts로 이어 간다.

Worker는 새로 생성하지 않으며 pending approval, dirty worktree, unresolved launch,
cleanup obligation, budget reservation과 publication intent를 보존한다. Active landing이
있으면 현재 구현의 receipt/status 경로를 인계하며 별도 재실행하지 않는다.

2026-11-18 source에서 swap CLI와 core 검증은 있지만 실제 `CollabServices`는
`swap_main_host`를 구현하지 않아 native host 경로가 거절된다. 자동 quota swap을 현재
지원 기능으로 표시하지 않는다. [CC-09](CALLABLE-CONSOLIDATION-WORK-PACKETS.md#cc-09--native-main-swap-host-연결)는
지원 상태 표시와 실제 host handshake를 구분한다. 향후 지원된 조작만 기존 권한·receipt에
연결하고, Main 교체를 새 orchestrator의 model/effort 선택과 혼동하지 않는다.
Budget unknown을 free로 간주하거나 credit/API 결제로 자동 승격하지 않는다.
새 adaptive router와 자동 기본값 승격은 TM-10의 평가·승인 뒤에만 별도 도입한다.

## 6. 의사결정 비용과 context 설계

Main에 보낼 입력은 goal/acceptance, dependency 변화, 실패 요약과 정확한 evidence refs,
충돌/새 권한 요구, 합쳐진 candidate의 검증 결과다. 전체 transcript는 필요할 때만
bounded reader로 읽는다. Worker가 보낸 자연어 status는 untrusted observation이며
시스템 지시나 사용자 권한을 대체하지 않는다.

Token 사용량, API 환산 가격, 실제 provider reported cost, 구독 quota를 서로 대체하지 않는다.
Main/lead/advisor/reviewer/repair/native-child의 실제 또는 unresolved 사용량을 모두
기존 ledger에 연결한다. Main을 싸게 바꿔도 worker 재작업이 늘면 전체 비용은 오를 수 있으므로
평가 지표는 Main token만이 아니라 **accepted result당 전체 비용과 시간**이다.

## 7. 평가 계획: Main 변경 효과만 분리

[#248](https://github.com/eightmm/oh-my-setting/pull/248)의 paired evaluator를 재사용한다.
Main 후보 비교에서는 task/base/verifier, worker 모델·topology, 도구/권한, context pack,
max total usage, retry 정책을 고정하고 Main harness/model/effort만 변경한다.
Harness 차이까지 포함하면 '모델 단독 비교'가 아닌 'Main configuration 비교'라고 명시한다.

과제는 disjoint module 구현, 공유 API 충돌, composed-tree 회귀, 잘못된 worker 완료 주장,
quota/interruption handoff, 읽기 전용 연구 가설 검토를 포함한다. GPU/유료 모델/원격 실행을
기본 unit test로 만들지 않는다. Offline replay는 routing/권한 회귀 검사이지 실제 모델 품질
비교가 아니다. Live run은 계정/예산/입력 데이터 전송 범위 승인 뒤 수행한다.

Accepted-task rate, wall time, 전체 reported usage와 coverage, 잘못된 배정·중복 작업,
독립 검증 실패, 인간 개입, handoff 손실을 함께 보고한다. Pilot 표본 수·순서·cache 상태·중단
규칙을 사전에 정하고 불명/실패 run을 분모에서 제외하지 않는다. 이 PR은 평가를 실행하지 않는다.

## 8. 구현 ownership과 회귀 기준

Main picker/attribution은 TM-06A, explicit preview/swap은 TM-06B,
model catalog/event adapter는 TM-07~09, evidence-based 추천은 TM-10에 속한다.
기존 role router를 Main 모델 picker 하나로 대체하지 않는다. Main과 worker route는 독립이다.

회귀 기준: catalog 미지원/오래된 값, selected/served 차이, 잘못된 effort,
추가 credit 필요, 자동 fallback, 같은 family의 다른 harness, Main swap 중 stale binding,
새 model 선택 후 pending write 보존, feature-off 시 기존 launch/swap 동작 보존.
모든 UI action은 기존 parent 권한과 receipt 경로를 통과해야 한다.

## 9. 출처와 확인 범위

아래 공식 문서는 2026-11-17 확인했다. 출시/모델 권고는 vendor 설명이며 OMS benchmark가 아니다.
계정별 실제 entitlement, 설치된 CLI catalog, native session과 paid quota는 이 작업에서 조회·실행하지 않았다.

- [O1: OpenAI Codex models](https://developers.openai.com/codex/models)
- [O2: OpenAI Codex App Server](https://developers.openai.com/codex/app-server)
- [A1: Claude Code model configuration](https://code.claude.com/docs/en/model-config)
- [A2: Anthropic choosing a model](https://platform.claude.com/docs/en/about-claude/models/choosing-a-model)
- [G1: Google Antigravity Orchestrator announcement](https://antigravity.google/blog/gemini-4-hydrogen-in-google-antigravity)
