# OMS model tiers, task profiles and resolved-route policy

작성: 2026-11-17. **설계와 MR-A 선택 snapshot 구현 / 기본값·운영 승격 없음**.
Source audit: `81cbee84d32b14d7737c01a1995f64ec7e182ac5`.
관련: [Main policy](TEAM-ORCHESTRATION-MAIN-POLICY.md),
[team panel](TEAM-ORCHESTRATION-PANEL.md),
[#245 roadmap](https://github.com/eightmm/oh-my-setting/pull/245),
[#247 adapters](https://github.com/eightmm/oh-my-setting/pull/247),
[#248 validation](https://github.com/eightmm/oh-my-setting/pull/248).

이 문서는 기존 네 티어를 설명하고 후속 routing 계약을 제안한다.
모델/effort/계정/과금/권한/default 변경, catalog refresh, native agent 실행,
추가 model benchmark, 설치 또는 자동 merge를 수행하거나 승인하지 않는다.
벤더 설명에 기초한 초기 후보는 OMS 실측 성능 순위가 아니다.

2026-11-18 구현 범위: 기존 [routing producer](../crates/oms-panel-core/src/routing.rs)와
[dispatch writer](../crates/oms-panel-core/src/dispatch.rs)가 schema-1 route snapshot을
연결한다. Policy/Candidate의 schema 2와 unknown-field 거절은 유지한다. Policy digest,
configured/requested/resolved 구분, 실제 catalog 반환 metadata, unreported served null을
기존 launch/reservation/process receipt에 보존하고 `panel --routes`에서 읽는다.
Family 문자열은 identifier-derived/unverified이며 composite/fanout은 unknown이다.
이는 MR-A의 로컬 fixture 구현 범위다. MR-B의 공유 예산·과금 승인 전체, MR-C UI 전체,
MR-D 평가·기본값 승격, 실제 provider capability 검증의 완료를 뜻하지 않는다.

MR-C의 제한된 읽기 구현: `OMS_PANEL_READER_BOARD`와 foreground TTY에서
`panel --routes`는 기존 Reader의 proposed-route inspector를 연다. 기존 JSON과
`--json` 우선 동작은 보존한다. MR-A chosen snapshot의 identity/effort/account와
catalog 출처·보고된 나이·exact effort evidence만 표시하며, 실제 실행·승인·served·
과금/entitlement/quota를 추정하지 않는다. 화면 열람은 refresh나 model call을 만들지
않는다. 이는 [Panel adapter](../crates/oms-command-panel/README.md)의 읽기 범위이며,
MR-C의 전체 controls/attribution이나 MR-B 자격·과금 검증 완료를 뜻하지 않는다.

MR-B의 제한된 evidence consumer는 선택적 source-reported account/endpoint와
observed/expiry를 snapshot에 보존한다. Source scope의 read 시점 freshness와 선택된
account 일치를 분리하며 TTL 기본값·endpoint 추론·eligibility 권한을 만들지 않는다.
현재 native catalog는 이 scoped observation을 생산하지 않아 unknown으로 남는다.
Known fresh/stale positive는 injected catalog→snapshot/renderer fixture 범위이며, 실제 model-list
context·계정 alias·endpoint·cache generation을 함께 묶는 선행 producer가 필요하다.

## 1. 현재 구현과 제안의 경계

정적 확인한 소스:
[panel-routing.json](https://github.com/eightmm/oh-my-setting/blob/81cbee84d32b14d7737c01a1995f64ec7e182ac5/config/panel-routing.json),
[task-routing-rules.json](https://github.com/eightmm/oh-my-setting/blob/81cbee84d32b14d7737c01a1995f64ec7e182ac5/config/task-routing-rules.json),
[routing.rs](https://github.com/eightmm/oh-my-setting/blob/81cbee84d32b14d7737c01a1995f64ec7e182ac5/crates/oms-panel-core/src/routing.rs).

| 관측한 현재 계약 | 이 명세의 처리 |
|---|---|
| Panel schema 2는 `light/routine/deep/frontier`를 요구한다 | 그대로 유지한다. 새 S/A/B 등급 체계를 만들지 않는다 |
| Main 기본은 Codex/Sol high, Claude/Opus high다 | 현재값과 향후 default/medium 비교 실험을 구분한다. 이 PR은 낮추지 않는다 |
| Candidate는 provider/model/effort/public_only/access를 가지며 unknown field를 거부한다 | 아래 설계 필드를 기존 JSON에 바로 붙이지 않는다. schema/consumer 이행을 먼저 구현한다 |
| Panel effort는 low/medium/high만 허용하고, 한 tier에 같은 provider를 중복 등록할 수 없다 | xhigh/max/default, 같은 provider의 복수 후보는 후속 계약 변경이다 |
| 자동 AGY 후보는 read-only이며 model parser도 versioned Gemini 형태를 요구한다 | Hydrogen/Orchestrator를 설정에 추가하는 것만으로 write/native team이 지원되지 않는다 |
| Task rules에 kind/risk/size와 tier/effort가 이미 있다 | 새 classifier를 복제하지 않고 기존 producer/consumer를 확장한다 |
| `researcher`는 light이며 Luna/Haiku와 low effort로 제한된다 | 수집은 researcher, 과학적 해석은 기존 scholar/advisor로 구분한다 |
| Catalog 해석은 같은 route family의 더 높은 version을 선택할 수 있다 | configured ID와 실제 selected ID를 분리한다. 새 버전 등록을 품질 검증으로 표현하지 않는다 |
| Fable 제한에는 특정 configured model 문자열에 대한 조건이 있다 | 최종 resolved model의 billing/eligibility 재검증을 회귀 항목으로 둔다 |

마지막 두 항목은 확인한 resolver 경로의 정적 관찰이다. 모든 launch/계정/과금
producer를 end-to-end 검증한 결과나 실제 과금 우회가 발생했다는 주장이 아니다.
Source가 이동하면 pinned baseline과 current main 차이를 먼저 확인한다.

## 2. 티어의 의미: 모델 서열이 아니라 작업에 필요한 실행 등급

| Tier | 적합한 작업 | 기본 정책 제안 | 피할 사용 |
|---|---|---|---|
| `light` | 근거가 주어진 추출·정리, 파일 위치 탐색, 기계적 변경 | 좁은 scope와 직접 검사 가능한 산출물 | 연구 결론, 전역 설계, 최종 독립 검증을 단순 요약처럼 배정 |
| `routine` | 명확한 모듈 구현, 재현된 작은 버그 수정, 정해진 테스트 작성 | 일상 Worker의 출발점; 유효한 verifier 필요 | 짧은 diff라는 이유로 권한·암호화·데이터 무결성 변경을 낮게 분류 |
| `deep` | Main/Lead 판단, 아키텍처, 복잡한 디버깅, 독립 리뷰, 연구 해석 | 중요한 판단의 기본; phase와 risk에 따라 effort 선택 | 모델이 답했다고 acceptance/approval을 대신함 |
| `frontier` | 어려운 설계 모순, 반복적인 reasoning 실패, 고난도 수학·연구·감사 | 제한된 advisor/second opinion; 사전 예산과 접근 확인 | 모든 Main/Worker의 상시 기본값, 실패할 때마다 무제한 승격 |

프로세스 대기, 상태 집계, 정해진 테스트 실행, 해시·범위·receipt 검사는
가능하면 기존 Rust/runtime으로 처리한다. 이는 다섯 번째 모델 tier가 아니다.
테스트를 실행하는 작업과 테스트를 설계하거나 결과의 의미를 판단하는 작업을 구분한다.

모델 하나는 여러 workload tier의 후보가 될 수 있다. 현재 Carbon이 deep/frontier에
함께 있는 것도 tier가 서로 다른 고유 모델을 뜻하지 않음을 보여 준다.
`frontier`는 나머지 모든 모델보다 우수하다는 증명이 아니라 예외적 자원 배정이다.

기존 다른 진입점의 `fast/balanced/deep`는 해당 consumer가 허용하는 동안 보존한다.
새 Panel에서는 네 티어를 사용한다. Fast를 light/routine 중 하나로 무조건 치환하지 말고
phase/risk와 기존 intent를 확인하는 명시적 호환 경로를 사용한다.

## 3. 현재 모델 배치와 향후 평가 후보

아래 configured 값은 위 baseline 파일에서 읽었다. 실제 계정 catalog/served model을
조회하지 않았으므로 사용자의 모든 계정에서 실행 가능하다는 뜻이 아니다.

| Tier | 현재 Codex configured | 현재 Claude configured | 현재 자동 AGY configured |
|---|---|---|---|
| light | `gpt-6-luna`, low | `claude-haiku-5-5`, low | 없음 |
| routine | `gpt-6.1-terra`, medium | `claude-sonnet-5-5`, medium | 없음 |
| deep | `gpt-6.1-sol`, high | `claude-opus-5-5`, high | `gemini-4-carbon-high`, read |
| frontier | `gpt-6-astra`, high | `claude-fable-5-1`, high | `gemini-4-carbon-high`, read |

Light/routine에는 Pi/OpenRouter의 public-only 후보도 설정되어 있다. 이는 모든 role에서
그 후보가 선택된다는 뜻이 아니다. 특히 researcher에는 별도의 family filter가 있다.
공개 repository라도 prompt/로그/첨부/다른 파일이 비공개일 수 있으므로 공개 repo flag만으로
새 endpoint에 모든 context를 전송할 권한을 부여하지 않는다.

OpenAI의 Luna/Terra/Sol/Astra 용도 구분 [O1,O2], Claude의 Haiku/Sonnet/Opus/Fable
용도 구분 [A1]은 초기 후보 설정의 참고 자료이지 provider 간 동급 성능의 증거가 아니다.
Terra는 시각·computer-use와 scoped work 후보이므로 일반 코드 구현 전체에서 Sol보다
경제적이라고 단정하지 않는다. 기존 route를 유지하면서 작업군별로 비교한다.

향후 후보: Gemini 4 Hydrogen은 routine 실행 평가, Carbon/Argon은 deep 판단 평가,
Fable 5.2는 explicit frontier 평가 대상으로 둘 수 있다 [A1,G1]. Fable configured 5.1을
유효 catalog가 5.2로 해석할 가능성과, 사람이 default를 5.2로 승인하는 것은 별개다.
모델 단독 접근과 Orchestrator/Swarm 접근도 별도로 확인한다.

Orchestrator는 Carbon/Argon planner와 Hydrogen executor의 복합 profile이며 고정 effort다
[G1]. Single model처럼 기록하거나 `frontier + high`로 강제 변환하지 않는다.
`single`, `oms-team`, `native-team`, native multi-model orchestration은 tier와 별도 축이다.
여러 agent를 쓰는 mode를 켜는 행위는 단순 effort 증가로 처리하지 않는다.

## 4. 미리 정의할 정보: 정책, capability, 계정 상태를 분리한다

아래 이름은 설계 항목이며 현재 CLI/schema가 지원한다는 뜻이 아니다.
기존 policy, capability catalog, account store, attempt/usage ledger를 확장한다.
별도 authoritative model DB나 자동 LLM classifier 서비스를 만들지 않는다.

| 축 | 필요한 정보 | 금지할 혼동 |
|---|---|---|
| Task profile | kind, phase, risk, ambiguity, domain tags, verifiability, context need, deadline, 추천 profile와 legacy tier | 파일 수/줄 수나 tier 이름을 난이도·영향도·권한의 완전한 대리값으로 사용 |
| Identity | harness/version, endpoint, account alias, configured/requested/resolved/served model, provider/model family | Claude CLI라는 이유만으로 실제 모델도 Anthropic이라고 가정 |
| Effort | requested, native-supported set, applied/default/unknown, override source | 모든 provider의 high를 동일 계산량으로 비교 |
| Capabilities | read/write, tool/structured output, context, resume, cancel, streaming, usage, native-child observability | 모델 지능으로 부족한 adapter 제어·격리를 보완 |
| Billing | included-plan, credits, API, verified-free, unknown; approval/spend ceiling | 구독을 무료로, entitlement를 추가 결제 승인으로 해석 |
| Runtime availability | auth/health, account/model access, quota pool, reset/source/age, concurrency, cooldown | unknown을 zero 또는 무제한으로 처리 |
| Evidence | source, tested harness/version, evaluation domain, date, sample count, quality/latency/usage coverage | vendor claim, fixture pass, live capability test, quality benchmark를 같은 검증 상태로 표시 |
| Execution profile | single/team/composite, declared max fan-out/depth, observable usage coverage | 단일 parent만 집계하고 hidden children의 사용량을 버림 |

Immutable attempt에는 policy revision, task/base/context/verifier identity와 resolved route를
묶는다. Mutable health/quota는 시각과 source를 가진 실행 전 검증 입력으로 기록한다.
명시적 model pin도 capability, 데이터 정책, 예산, 권한을 우회하지 못한다.

## 5. 선택 순서와 재현성

아래는 전체 model/effort 조합을 선택하는 후속 계약이다. 현재 schema 2의 네 tier,
tier당 provider 하나, role/family/access 및 low/medium/high 제약은 호환 경로에 남는다.
이 제약을 문서 변경만으로 해제하지 않으며 실제 변경은 parser/consumer/fixture와
migration을 함께 다룬다. 이미 있는 최종 resolved eligibility와 exact effort 검사는 재사용한다.

1. 기존 사용자 선택과 account allowlist, task/phase/risk, verifier, 데이터 전송 정책,
   role/access를 확인한다. 단순 size나 LLM self-rating으로 위험도를 낮추거나 권한을 올리지 않는다.
2. 계정/endpoint에 유효한 catalog로 exact model과 effort/composite profile을 해석한다.
   Catalog stale/unknown과 documented-only capability를 분리한다.
3. 최종 resolved 후보에 billing authorization, required capabilities, 독립성,
   scope/context 및 실행 예산을 다시 적용한다. 해석 전 이름에 한 번 검사하고 끝내지 않는다.
   명시 pin이 요구 조건과 충돌하면 이유를 알리고 임의의 다른 모델로 바꾸지 않는다.
4. 자격을 통과한 전체 후보에서 명시 선택을 받는다. Tier/role profile은 추천과 기본 선택의
   설명에 사용하며, 초기 모델 배치표에 없다는 이유로 새 선택 경로에서 후보를 제외하지 않는다.
5. 자동 추천이 필요한 경우에만 품질 근거, 지연, quota 여유, 실제 비용과 기존 tie-break를
   비교한다. 임의의 단일 점수로 권한 부족·독립성 실패·추가 과금을 상쇄하지 않는다.
6. 기존 parent-owned writer에서 account/quota-pool 및 팀 전체 예산을 예약하고
   launch intent/attempt와 route snapshot을 기록한다. 중복 요청은 같은 identity로 조정한다.
7. 실행 중 served model/effort/fallback/children이 보고되면 attribution을 기록한다.
   선택 정보만 있으면 served는 unreported다. 완료 후 acceptance와 독립성을 재검증한다.

Model을 고정해도 provider revision을 pin할 수 없는 endpoint에서는 비트 단위 재현성을
보장하지 않는다. Catalog/선택 시각/보고된 revision을 보존하고 한계를 표시한다.
허용된 fallback도 attempt의 원래 route를 덮어쓰지 않고 transition과 사유를 남긴다.
Fallback이 승인된 경계 밖이면 가능한 안전 경계에서 추가 호출을 막고 parent에 알린다.
Endpoint가 fallback을 강제하고 차단/관측할 수 없다면 strict 경로의 capability 미충족이다.
이미 사용된 비용이 취소로 되돌아온다고 가정하지 않는다.

## 6. 역할과 phase의 기본안

| 역할/phase | 제안 출발점 | 예외 |
|---|---|---|
| Main | deep, 현재 선택 유지 | 어려운 쟁점만 frontier advisor; runtime bookkeeping은 LLM 밖 |
| Team Lead | deep | 단순 작업에는 별도 Lead를 만들지 않음 |
| Builder/refactorer | routine | 공유 API/병렬성/상태 무결성은 deep부터 |
| Researcher | light | 검색·추출만; 논문 타당성·수학·실험 해석은 scholar/advisor deep |
| Scholar/architect | deep | 어려운 증명·모델 설계의 unresolved 쟁점만 frontier |
| Test execution | deterministic runtime | 테스트 설계는 routine/deep; 결과 수락은 별도 gate |
| Reviewer/verifier | deep 기본 | 기존 low-risk small routine 예외는 승인된 평가·계약 안에서만 유지 |
| Security auditor | frontier 후보 또는 충분한 deep | 과금/접근 제약을 role 이름으로 우회하지 않음; 정책상 거부를 우회하기 위한 rerouting 금지 |
| Documenter | light/routine | 과학·보안·공개 API 의미를 판단하는 문서는 deep |

Role은 기본값이며 실제 phase/risk와 independence requirement가 우선한다.
예: PoseFixer 파일 목록 수집은 light, 이미 정한 인터페이스의 데이터 로더는 routine,
분할 누수·equivariance·gradient 의미 검토는 deep다. 새로운 수학적 layer 검토는
명시된 질문과 근거를 주고 frontier advisor로 평가한다. 연구 결과의 수치 검증은 모델의
말이 아니라 실행된 test/experiment evidence로 남긴다.

독립 리뷰는 실제 patch writer의 family를 기준으로 한다. Main family만 보지 않는다.
같은 family의 다른 모델/effort/CLI는 독립 family를 늘리지 않는다. Provider diversity는
오류의 통계적 독립을 보장하지도 않는다. 여러 family가 쓴 합성 patch는 writer set을
기록하고 가능한 외부 family를 선택한다. 불가능/불명확하면 제한을 드러내고 기존
acceptance 계약에 따른 명시적 처리 없이는 독립 gate가 통과했다고 표시하지 않는다.

## 7. Effort와 escalation

현재 schema 2는 low/medium/high만 지원한다. xhigh/max/default/native-fixed를
도입하려면 native capability와 parser/consumer/test를 함께 바꿔야 한다.
지원하지 않는 effort를 조용히 high로 낮추거나 max를 모든 모델에 전달하지 않는다.
Provider default 역시 model/client/account/version에 따라 달라질 수 있다 [O1,A1].

현재 Main의 high는 유지한다. Default/medium과 high 비교는 별도 승인된 평가다.
새로 설계하는 routine 작업은 medium, deep 판단은 지원되는 default/high를 비교하는
출발점으로 삼을 수 있지만 출시 문서만으로 기존 runtime 설정을 교체하지 않는다.

Pilot escalation 예시: 검증 가능한 reasoning 실패에 한해 같은 route에서 근거를 보강한
repair 한 번, 계속 실패하면 허용된 effort 증가 또는 다른 deep advisor 한 번을 고려한다.
이 횟수는 초기 평가안이지 현재 구현 상수나 보편적 최적값이 아니다. Task 전체
retry/time/usage ceiling이 항상 우선하며 tier 변경으로 예산을 초기화하지 않는다.

429/auth/quota, 네트워크 단절, missing dependency, GPU OOM, verifier 자체 오류는
별도의 infrastructure/environment failure다. 큰 모델로 올려 해결하려 하지 않는다.
권한·정책 거부는 상위 모델이나 다른 endpoint로 우회하지 않는다. 원인 불명은
추가 증거 수집 또는 parent 판단으로 넘긴다.

Model/effort 전환은 adapter가 지원하는 명시적 boundary 또는 새 attempt에서만 한다.
검증된 patch/context만 전달하고 native history를 복제해 resume했다고 표현하지 않는다.
Main은 goal 동안 안정적으로 유지하며 advisor 호출과 Main ownership swap을 구분한다.

## 8. 비용·쿼터·팀 전체 한도

최적화 대상은 Main의 token 단가가 아니라 accepted result당 전체 사용량·시간과
실패/인간 개입이다. Planning, lead, worker, review, repair, hidden native child와
context 재전송 비용을 포함한다. 서로 다른 provider quota percentage를 같은 금액처럼
더하거나 token 수에서 남은 구독 비율을 추정하지 않는다.

저장할 지표는 reported token usage, provider-reported charge, API-equivalent estimate,
subscription quota reading 및 coverage다. Estimate와 실제 청구를 분리한다.
같은 계정의 여러 모델이 quota pool을 공유할 수 있으므로 모델별 독립 잔액을 가정하지 않는다.
사용자의 account scope/승인을 벗어나 다른 계정이나 API 결제로 전환하지 않는다.

CLI가 금액의 hard cap을 제공하지 않으면 OMS timeout/token 추정으로 exact spending cap을
보장한다고 쓰지 않는다. 관측·예약 가능한 soft bound와 과금 상한 보장의 차이를 표시한다.
Native orchestration에서 child 수/사용량을 제한·관측하지 못하면 strict budget 모드에서는
후보로 넣지 않거나 명시적으로 승인된 보수적 allowance와 한계를 요구한다.

## 9. 모델 출시와 catalog 변경

기존 catalog/정책 writer를 재사용한다. 발견, capability 확인, 품질 평가,
기본 후보 승인, deprecated 처리를 서로 다른 기록으로 둔다. 하나의 verified 배지로 합치지 않는다.
새 모델 발견은 즉시 default 승격이나 비용 증가 권한이 아니다.

현재 resolver의 최신 family 선택 동작은 이 문서로 변경하지 않는다. 후속 구현은
기존 track-compatible 동작과 opt-in pinned/pilot 경로를 명시적으로 구분하고,
최종 resolved route의 접근·과금 검증을 양쪽 모두에 적용한다. 기본값/호환성 변경은
별도 PR에서 migration과 rollback을 제시한다. Feature-off는 기존 동작을 유지한다.

공식 문서/모델 catalog는 모델 존재·native capability 근거이며 품질 benchmark가 아니다.
TTL은 source별로 정하고 expiry를 표시한다. Panel refresh나 agent의 발견 메시지가
network refresh, 유료 smoke test 또는 자동 config write를 시작하지 않는다.

## 10. Panel에 표시할 정보

Team card에는 task/role/tier/selected model/effort와 다음 조치를 짧게 표시한다.
상세 reader에는 선택 사유, configured와 resolved/served 차이, actual family,
account alias, billing mode, quota source/age, candidate 제외 이유, capability/evaluation
수준과 policy revision을 보여 준다. Credentials나 native account home은 출력하지 않는다.

`deep / Sol / high / included-plan?`에서 물음표를 정상 포함 상태처럼 숨기지 않는다.
Unknown billing은 실행 전 필요한 확인을 뜻한다. `frontier`는 승인·완료 배지가 아니다.
AGY Orchestrator는 `Carbon + Hydrogen / native-fixed / usage coverage: partial`처럼
보고 가능한 범위만 표시한다. UI를 열거나 tier 설명을 펼치는 것은 호출/승격 승인이 아니다.

## 11. 후속 구현 분할과 완료 조건

MR 묶음은 #245의 기존 작업에 귀속되는 acceptance 참조다.
현재 착수 범위와 순서는 [통합 작업 큐](ORCHESTRATOR-WORK-QUEUE.md)를 따르며,
아래 표를 이미 연결된 snapshot/consumer를 다시 만드는 별도 router backlog로 사용하지 않는다.

| 묶음 | 기존 작업에 귀속 | 구현 범위 |
|---|---|---|
| MR-A: route identity/compatibility | TM-01, TM-07~09 | 정확한 모델·effort·family·catalog evidence, schema 호환, composite 구분 |
| MR-B: eligibility/budget | TM-02~04 | 최종 resolved 후보 검증, 승인된 escalation, 공유 quota pool, attempt 전체 한도 |
| MR-C: explainable panel | TM-06A/B | 티어/사유/제외 조건/served unknown 표시; view는 read-only |
| MR-D: calibration/promotion | TM-10 및 #248 | 동일 과제 비교, evidence-based 후보 승인, default 변경의 opt-in/rollback |

MR-A의 snapshot/consumer 계약을 먼저 정하고 MR-B가 이를 소비한다. MR-C 읽기 화면은
확정된 projection 뒤 병행 가능하다. MR-D는 실제 capability/usage coverage가 준비된 뒤
수행한다. 본 PR의 문서 병합으로 어떤 묶음도 완료 처리하지 않는다.

기존 routing/task-routing/account/capability/panel/usage fixture를 확장한다.
새 test framework나 중복 scheduler는 만들지 않는다. 필수 회귀 항목:

| ID | 시나리오와 기대 |
|---|---|
| MR-01 | Schema 2 fixture의 기존 route/권한과 지원되는 native 동작 보존; 미지원 필드·host 조작 명시 거부 |
| MR-02 | 작은 high-risk 변경은 size shortcut으로 low tier가 되지 않음 |
| MR-03 | researcher 수집과 scholar 해석의 scope 구분; 연구 키워드만으로 frontier 호출 안 함 |
| MR-04 | unsupported effort/native-fixed에서 arbitrary effort 전달 차단 |
| MR-05 | configured 5.1에서 catalog 5.2 해석 후 최종 billing/allowlist 재검증 |
| MR-06 | alias/default/revision 변경과 요청/선택/실제 모델의 분리; unreported를 추정하지 않음 |
| MR-07 | 다른 harness의 같은 family, same-family fallback 및 multi-family writer set 처리 |
| MR-08 | unknown/stale capability·quota·billing을 지원/무료/무제한으로 바꾸지 않음 |
| MR-09 | 공개 repo의 비공개 첨부·context는 public-only endpoint로 전송하지 않음 |
| MR-10 | 명시 pin·security role·frontier 선택도 권한/추가 지출 승인을 우회하지 않음 |
| MR-11 | 같은 quota pool의 동시 팀 예약 및 중복 요청이 예산을 이중 할당하지 않음 |
| MR-12 | retry/escalation/native child가 task 총한도를 초기화하거나 숨기지 않음 |
| MR-13 | 429/auth/network/OOM과 reasoning failure가 다른 recovery 경로를 사용 |
| MR-14 | native composite/hidden fan-out의 capability 부족을 strict mode에서 표시·차단 |
| MR-15 | catalog refresh/UI 열람이 model call/default 승격/자동 과금을 일으키지 않음 |
| MR-16 | Main advisor 호출이 ownership swap이 되지 않고 pending Worker/approval을 유지 |
| MR-17 | rollout/rollback 뒤 in-flight attempt의 policy/route snapshot과 evidence가 보존됨 |
| MR-18 | 복구 후 unresolved 사용량·launch·partial output을 실패 분모와 비용 coverage에서 숨기지 않음 |

품질 평가는 같은 task/base/context/verifier, 권한·도구·total budget·retry 정책을 고정한다.
Main 비교는 worker/team 구성을 고정하고, model/effort 비교는 가능한 한 같은 harness를 쓴다.
불가능하면 모델 단독이 아닌 실행 configuration 비교라고 명시한다. 단순 extraction,
모듈 구현, 상태/동시성, 독립 리뷰, ML 수학·데이터 누수 등 작업군별로 보고한다.
Accepted-task rate, false acceptance, regression, wall-time p50/p95, 총 usage/coverage,
추가 지출과 사용자 개입을 함께 평가한다. 표본 수/순서/cache/중단 규칙을 사전에 정하고
실패·불명 run을 제외하지 않는다. 소규모 pilot을 통계적으로 확정된 순위로 발표하지 않는다.

## 12. 공식 출처와 이번 확인 범위

2026-11-17 문서 확인. 아래 출처는 존재/용도/effort/복합 실행에 관한 참고이며,
계정 entitlement나 OMS 성능을 증명하지 않는다.

- [O1: OpenAI Codex/Work models](https://learn.chatgpt.com/docs/models)
- [O2: OpenAI model selection](https://learn.chatgpt.com/docs/model-selection)
- [O3: OpenAI Codex/Work pricing](https://learn.chatgpt.com/docs/pricing)
- [A1: Claude Code model configuration](https://code.claude.com/docs/en/model-config)
- [G1: Antigravity model and Orchestrator configuration](https://antigravity.google/docs/models)

원래 설계 조사는 문서 작성과 pinned source/공식 문서의 정적 대조였다. 위 MR-A 구현은
로컬 fixture로 별도 검증하며, 실제 계정/CLI capability 조회, 유료 모델 평가, 설치 및
runtime config 변경을 수행했다는 증거로 사용하지 않는다.
게시된 문서 확인과 exact-head CI 결과는 PR 본문 또는 후속 기록에서 별도로 보고한다.
