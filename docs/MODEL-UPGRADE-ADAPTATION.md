# 모델 업그레이드에 따른 OMS 수정과 후속 작업

확인일: 2026-11-19. 기준 main:
[`7d9b291592a926ce389693141d11ef8269eccfcd`](https://github.com/eightmm/oh-my-setting/commit/7d9b291592a926ce389693141d11ef8269eccfcd).
이 문서는 GPT/Codex, Claude Code, Antigravity의 공식 사용 지침을 현재 Rust caller와
대조한 수정 범위다. 모델 품질·비용·실제 계정별 실행을 측정한 보고서는 아니다.
실행 우선순위는 [통합 작업 큐](ORCHESTRATOR-WORK-QUEUE.md), 기존 acceptance는
[model tiers](TEAM-ORCHESTRATION-MODEL-TIERS.md)와
[adapter 계약](TEAM-ORCHESTRATION-ADAPTERS.md)이 계속 소유한다.

## 1. 기존 PR과 이번 변경의 경계

아래 상태는 확인일 기준이다. 구현 착수 시 현재 main과 PR HEAD를 다시 확인한다.

| 작업 | 범위 | 완료로 간주하면 안 되는 것 |
|---|---|---|
| [#258](https://github.com/eightmm/oh-my-setting/pull/258), open draft | 새 native orchestrator의 명시적 model/effort 선택 | 기존 세션 재설정, managed panel의 모든 model/effort 지원, 실제 served 설정 |
| [#276](https://github.com/eightmm/oh-my-setting/pull/276), open | upstream release 추적과 full catalog routing consumer | 모든 실행 경로의 capability 계약 일치, 계정별 실제 inference 성공 |
| [#280](https://github.com/eightmm/oh-my-setting/pull/280), merged | Dreaming/memory reuse 명세 | 새 memory runtime 구현 |
| [#281](https://github.com/eightmm/oh-my-setting/pull/281), draft | Native Claude `xhigh`, subagent effort 전달, Fable status 인식 | 전체 catalog/policy 통합, footer가 증명하지 않는 실제 inference identity |
| [#282](https://github.com/eightmm/oh-my-setting/pull/282), draft | Native compaction 우선 안내와 Main/Lead/leaf·읽기 범위 명료화 | 자동 compaction 실행, leaf의 runtime 권한 승격 |
| Prompt audit 변경 | 스킬 참조 문서·출력 스타일 포함, 미검사 영역을 명시 | runtime prompt·실제 주입·의미 충돌·중복 분석 완료 |

PR에 구현과 회귀 fixture가 있어도 통과한 검증과 live provider 증거는 별도로 기록한다.
현재 기준 main의 panel resolver는 이미 unpinned 모델을 같은 family의 catalog 후보로
갱신하는 경로가 있다. `config/panel-routing.json`에 오래된 버전이 보인다는 이유만으로
모든 실행이 그 버전을 사용한다고 판단하거나 seed 모델명을 일괄 치환하지 않는다.
Native role renderer와 raw peer 등 실제 consumer별 차이를 확인한다.

## 2. 공식 사용 지침이 바꾸는 판단

| Provider | 공식 지침에서 확인한 점 | OMS에 적용할 판단 |
|---|---|---|
| GPT/Codex | GPT-6.2 계열은 목표·완료 조건을 명확히 전달하고 과도한 절차·반복 검증을 줄이는 지침이 있다. Codex model list는 지원 effort와 기본 effort를 제공한다. Responses는 이전 prefix 보존이 중요하다. | 역할·성공 조건을 유지하면서 불필요한 읽기/테스트 절차를 줄인다. 사용 가능한 설정은 실행 경로의 catalog에서 읽고, bridge 변환은 실제 endpoint 계약부터 확인한다. |
| Claude Code | 최신 model configuration은 `xhigh`와 subagent/skill의 effort 설정을 설명한다. Best practices는 과도한 context 관리·강조·반복 지시를 재검토하도록 한다. | Native effort 전달 누락을 수정한다. 지원되는 native compaction을 먼저 사용하고, leaf와 lead를 구분한다. CLI 설정과 Messages API의 계약을 섞지 않는다. |
| Antigravity | `agy models`는 계정에서 사용 가능한 모델을 보여준다. Neon/Hydrogen의 effort와 fixed-effort Orchestrator는 같은 형태의 선택지가 아니다. | 낮음/중간/높음 세 값이나 모델 이름 suffix만으로 모든 선택을 검증하지 않는다. Composite profile과 adjustable effort를 구분하고 실제 host 지원을 확인한다. |

공식 근거:

- [OpenAI latest model guide](https://developers.openai.com/api/docs/guides/latest-model.md)
- [OpenAI skills and prompts guidance](https://developers.openai.com/blog/rethinking-skills-and-prompts-for-gpt-6-astra.md)
- [Codex app-server model list](https://learn.chatgpt.com/docs/app-server.md)
- [OpenAI: keep prefix unchanged](https://developers.openai.com/api/docs/guides/keep-prefix-unchanged)
- [Claude Code model configuration](https://code.claude.com/docs/en/model-config)
- [Claude Code best practices](https://code.claude.com/docs/en/best-practices)
- [Antigravity models](https://antigravity.google/docs/models)
- [Antigravity swarm](https://antigravity.google/docs/swarm/)

이는 현재 공식 지침과 source 차이의 근거다. 특정 effort가 모든 작업에서 더 좋다는
평가 결과는 아니다. 기본값 승격은 기존 MR-D/TM-10 평가 owner가 다루며, 단순
호환성 수정을 유료 모델 전체 조합 평가에 종속시키지 않는다.

## 3. 공통 model × effort × 실행 경로 계약

**기존 owner: MR-A / TM-01, TM-07~09.** Provider identity는 CC-01/03과 조정한다.
CC-02의 full catalog consumer는 #276에 있으므로 별도 구현하지 않는다.

### 구현된 native 요청 검증과 남은 경계

`oms-command-agent-role::native_model`의 작은 공통 validator를 role policy,
panel resolver, peer route/argv, native orchestrator argv가 함께 사용한다. 기존
dependency 경로를 재사용하며 새 catalog, registry, DB나 외부 dependency는 없다.
Role renderer의 Claude 버전 목록과 panel의 세 단계 제한을 제거하고 versioned
family 검증 및 기존 native effort vocabulary를 맞춘다.

| Native 요청 | 검증 경계 |
|---|---|
| Codex | `low/medium/high/xhigh/max/ultra` 토큰 전달; 개별 모델 지원을 단정하지 않음 |
| Claude | `low/medium/high/xhigh/max`; `ultra`는 거절 |
| AGY Neon/Hydrogen | `xhigh` 포함 native 요청과 versioned selector 지원; `max/ultra` 거절 |
| AGY fixed Orchestrator 2종 | effort 생략을 허용하며 구체적인 effort 요청은 argv 생성 전 거절 |
| Unknown | 명시적 raw native 모델은 기존 token 검증 후 전달할 수 있지만 지원 근거는 `unknown`; managed role의 미확인 family/provider는 거절 |

Schema 2 route의 `effort` 생략은 additive하게 허용하며 native 기본값을 뜻한다.
기존 명시 effort 값과 role renderer의 출력은 유지한다. Fixed 모델만 명시한 panel
요청은 이전 adjustable tier의 effort를 상속하지 않고, 명시한 effort와 충돌하면
거절한다. `researcher`의 light/low 제한, 명시 account pin, catalog family upgrade와
최종 resolved eligibility 검사는 유지한다. Fixed 모델의 자동 선택은 exact catalog
entry를 요구하며 새 composite profile의 membership을 추론하지 않는다.

기존 `ProviderEvidence`, `CapabilityEvidence`, `RouteSnapshot`은 계속 원본 목록과
출처를 보존한다. `effort_support`에는 `fixed`, `provider_default`를 더하며, 실제
exact-model 목록이 있을 때만 `reported_supported/unsupported`로 판정한다. Provider
단위 effort 목록이나 모델 이름만으로 supported 목록을 만들지 않는다. 관측 account가
선택 account와 다르거나 만료·미래 관측이면 해당 목록을 route에 적용하지 않는다.
현재 native route는 endpoint를 증명하지 않으므로 endpoint가 지정된 capability
근거도 `unknown`으로 보존한다. 고정 profile의 실제 effort 값이나 served/applied
설정은 새로 추정하지 않는다.

이 변경은 native 요청 검증이다. Subscription bridge/API의 capability, 새 endpoint
binding, 실제 계정 entitlement·inference 성공은 별도 증거가 필요하다. Native display
alias 정규화와 full catalog consumer는 기존 owner를 사용한다.

### 완료 조건

1. 같은 모델·effort·실행 경로 요청은 panel, peer, agent-role에서 같은 근거로 판단한다.
2. AGY fixed Orchestrator는 effort 생략을 지원하고, 명시한 effort를 조용히 무시하지 않는다.
   Adjustable Neon/Hydrogen `xhigh`, 미지원 `max`, unknown을 fake catalog/argv로 검증한다.
3. 계정 A 또는 endpoint A의 capability를 다른 계정/endpoint의 증거로 재사용하지 않는다.
4. Exact pin, schema compatibility 및 최종 resolved eligibility 검사를 유지한다.
5. 기존 route/adapter fixture로 public entrypoint까지 확인한다. 새 registry·scheduler·
   상주 서비스나 실제 모델 호출을 첫 PR의 전제로 추가하지 않는다.

## 4. 요청·해결·관측과 기존 세션 재사용

**기존 owner: MR-A/MR-C + REL-01.** #258의 launch flag 구현 뒤에 연결한다.

`oms-panel-core/src/routing.rs`의 requested/resolved/served snapshot과
`teams.rs`의 `served_model:null`, `applied_effort:null`은 재사용 대상이다.
`oms-command-panel/src/orchestrator.rs`의 footer parser는 모델을 관측하지만 일부
effort 표시는 제거하며, 기존 lead 연결만으로 새 요청 설정의 적용을 증명하지 않는다.

가장 작은 후속 변경은 기존 snapshot과 출처가 있는 native 관측을 연결하는 것이다.
Footer에 표시된 설정과 inference response로 확인한 served 설정을 구분한다.

### 완료 조건

1. Requested/resolved는 기록하되 provider가 확인하지 않은 served/applied는 null로 둔다.
2. 기존 세션 연결을 model/effort 변경 성공으로 표시하지 않는다. Override는 #258의
   거절 계약 또는 별도로 검증한 native reconfigure 경로를 따른다.
3. 관측을 source/parser version·timestamp·session/pane/process identity·generation에 묶는다.
   Footer 부재·형식 변경·PID 재사용은 stale/unknown이며 이전 값을 새 증거로 쓰지 않는다.
4. Alias/composite/effort 미보고 fixture를 기존 panel suite에 추가한다.
5. `--routes`/`--teams` 열람은 모델 호출·재설정·retry를 만들지 않는다.

REL-01의 전체 native child coverage 작업이나 별도 model state DB를 복제하지 않는다.

## 5. 파일 읽기 전용과 허가된 공개 웹 조사

**기존 owner: TM-07~09 adapter permission + MR-B.** 같은 `providers.rs`를 수정하는
REL-02/CC-01 작업과 writer를 조정한다.

현재 Codex read 경로는 read-only sandbox와 `web_search="disabled"`를 함께 적용하고,
Claude read 경로는 plan mode와 `Read,Grep,Glob,Bash` tool 목록을 함께 적용한다.
모든 읽기 작업의 결함이라는 뜻은 아니다. 파일 쓰기는 허용하지 않으면서 공개 자료를
조사해야 하는 작업에 맞는 실행 profile이 필요하다는 의미다.

기존 task/route 권한에서 filesystem read/write와 public search/fetch 허용 여부를
분리한다. Private connector, upload, 외부 쓰기는 각자의 authority를 계속 따른다.

### 완료 조건

1. Local review는 현재 제한을 유지한다. 허가된 public research는 read-only filesystem과
   지원되는 native search/fetch를 함께 사용한다.
2. 기존 `provider_native_transports_match_shell_contract`에서 두 profile의 argv와
   tool 설정을 검증한다. AGY의 실제 isolation 제약도 유지한다.
3. 미지원 provider를 web enabled로 표시하지 않는다. Read 권한을 이유로 모든
   connector·upload·쓰기 제한을 해제하지 않는다.
4. 이미 부여된 task authority를 소비하며 반복적인 추가 승인 절차를 만들지 않는다.

## 6. Subscription bridge의 Responses prefix 보존

**기존 owner: `oms-command-providers-skills` bridge.** REL-04의 Broker I/O supervision과
공유 파일만 조정한다. 이 작업은 request translation의 의미를 다룬다.

`messages.rs::subscription`은 leading system/developer를 top-level instructions로
합치며 stateless full-input 계약을 사용한다. `gateway.rs`는 Messages 변환 경로와
native Responses 경로를 이 normalization에 합류시킨다. `https.rs`는 고정된
subscription endpoint를 사용한다. Opaque reasoning 보존 경로는 이미 있다.
Messages 왕복 변환에서 원래 Responses item의 ID/phase/argument 표현을 모두
보존하는지는 별도 검증이 필요하다. 실제 reasoning 손실이 발생했다고 측정한 것은 아니다.

가장 작은 변경은 Messages 변환과 유효한 native Responses item 전달을 구분하는 것이다.
일반 API 문서만으로 subscription endpoint가 새 item이나 stateful API를 지원한다고
가정하지 않는다. [기존 harness 계약](CODEX-HARNESSES.md)과 실제 endpoint/native source
근거를 먼저 확인한다.

### 완료 조건

1. Native Responses 경로의 기존 item order, role, IDs, tool call/result binding,
   opaque reasoning/compaction payload를 보존하거나 미지원으로 명시한다.
2. Messages 변환을 native Responses 경로에 중복 적용하지 않는다. 첫 요청 → 병렬
   tool call/result → 후속 요청의 최소 3-turn synthetic fixture로 prefix를 비교한다.
3. Model pin, supported effort 선택 및 stateless 계약을 유지한다. `previous_response_id`,
   `configuration_update`, compaction 지원 확대는 endpoint 확인 없이 켜지 않는다.
4. Loopback auth, 고정 upstream origin, no redirects, redaction, bounds와 cancellation을
   유지한다. 가짜 Backend로 검증하고 live subscription 호환성 증거는 별도로 기록한다.

## 7. Prompt audit와 검증 범위

이번 prompt audit 변경은 기존 `project-doctor --prompts`의 Markdown 범위를 넓힌다.
스킬의 `references/`와 출력 스타일도 점검하며, JSON에 depth/byte 한계와 미검사 항목을
기록한다. Text 요약도 미검사 영역을 명시한다. `duplicates: []`는 호환 필드이고
중복 없음의 증거가 아니다.
`--budget`의 loaded-instruction 중복 검사는 별도의 기존 기능이다.

Runtime-generated prompt의 추출/검증과 의미 충돌 분석은 남아 있다. 이를 진행할 때는
`orchestrator.rs` startup, `delegate.rs` worker brief, hooks의 동적 안내처럼 실제
producer와 native role renderer를 기존 fixture에 연결한다. Repository 문서 전부를
매 task마다 주입하거나 prompt 품질 평가를 모든 작업의 선행 단계로 만들지 않는다.

검증은 변경한 경로의 canonical fixture, docs references, 해당 critical gate를 사용한다.
새 hosted CI job/matrix/schedule이나 유료 모델 전체 조합 benchmark를 기본으로 추가하지
않는다. Fixture 성공은 CLI/변환 계약의 증거이고 모델 품질·비용·실제 served 설정의
증거는 아니다. PR에는 실행한 검사, 미실행 검사, 실제 provider 증거를 각각 기록한다.
