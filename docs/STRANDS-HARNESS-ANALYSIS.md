# Strands Harness 참고 분석

분석일: 2026-09-27. 후속 구현: 2026-09-28. OMS 기준: `5bd5bb8`.
Strands 소스 기준: `c56b7dea985c0c596ab021c46d3d4d15b4c35b67`.
상태: 독립 검토·상호 반박 완료. 결과 preview와 MCP 토론의 선택적 작업·메모리 연결을
로컬 구현했다. 후속 요청으로 실행 전용 답변의 범위 재조회도 구현하고 관련 검사를 통과했다.

사용자가 공유한 페이지는 [Strands harness 개요](https://strandsagents.com/docs/user-guide/harness/)다.
공식 문서, 공개 소스와 테스트, 관련 프레임워크의 1차 자료를 비교했다.
외부 코드는 읽기만 했으며 Strands 패키지 설치·모델 실행·벤치마크 재현은 하지 않았다.
OMS peer 도구로 GPT-6 Astra와 Claude의 독립 검토 및 상대 답변에 대한 최종 반박을 받았다.

## 결론

사용자가 확정한 방향은 **Strands의 유용한 기능과 구현 방식을 OMS 자체 기능으로 도입**하는 것이다.
Strands 패키지 설치, 별도 실행 백엔드, Strands와 OMS의 MCP 연결은 이번 도입 범위가 아니다.
기존 OMS 코드로 충족하는 기능은 재사용하고, 부족한 동작은 그 실행 경로에 구현한다.
Strands는 모델 호출과 도구 실행 루프를 직접 소유한다. OMS는 기존 provider CLI의
바깥에서 작업 범위, 협업, 실행 기록, 패치 반영과 검증을 관리한다.
Strands 내부의 context manager를 OMS에 추가해도 Codex/Claude의 대화 압축을
대신 제어할 수 있는 것은 아니다. [Strands SDK 구조](https://github.com/strands-agents/harness-sdk)

첫 채택은 **기존 `bounded_answer`의 재사용**이다. 최초 분석의 범위 재조회 우선안은
토론 뒤 보류했다. 현재 소비자는 보존된 파일을 읽을 수 있고 MCP만 사용하는 소비자의
실제 실패는 입증되지 않았다. 새 조회 경계보다 재현한 preview 결함을 먼저 수정했다.

후속 요청에서는 **기존 작업·메모리를 MCP 토론에 연결**했다. CLI에 있던 선택 기능을
`oms_peer_start`의 `include_memory`, `include_task`로 노출한다. 기본값은 기존과 같다:
메모리는 모두 제외하고 작업은 consult만 포함한다. 명시한 옵션은 시작 응답·실행 기록·목록에
남으며 자료가 실제 존재했다는 증거는 아니다. 잘못된 타입이나 message/ack의 옵션은 시작 전에
거절한다. 기존 질문 기반 메모리 선택과 민감정보 검사를 재사용하고 자동 쓰기는 추가하지 않는다.
이는 상담 시작 시점의 맥락 전달이며 Strands의 매 모델 호출 메모리 주입과는 다르다.

MCP 옵션 추가는 기존 OMS 기능의 접근 경로를 확장한 것이다. 이것만으로 Strands의
메모리 생명주기를 이식했다고 보지 않는다. 후속 구현은 각 기능의 실제 동작과 누락을 기준으로
정하며, 프레임워크 연결이나 옵션 노출을 기능 이식의 대체물로 삼지 않는다.

## OMS보다 나은 기능과 직접 도입 판단

| Strands의 우위 | OMS에서 확인한 차이 | OMS 내부 도입 방향 |
| --- | --- | --- |
| 결과를 저장하고 reference·범위·검색으로 다시 가져오는 context offloader | 기존에는 원본 파일과 제한된 preview만 제공 | OMS 자체에 preview, 실행별 범위 재조회와 literal 검색 구현 |
| 매 모델 호출의 메모리 주입, 모델 기반 추출, 검색과 읽기 전용 자식 store의 통합 | OMS의 append/pin·검색·압축 API와 동작·비용이 다름 | 기존 recall을 작업 시작·위임·재개 지점에 활용하고 부모의 기록 권한을 유지; 매 호출 주입과 자동 추출은 아직 미구현 |
| 루프 내부 turn/token 한도, 취소·interrupt 재개, 중복 호출 처리 | 외부 CLI supervisor는 모델 내부의 모든 단계·도구 호출을 통제할 수 없음 | OMS operation의 실행 한도·종료 사유·취소·재개를 기준으로 누락을 보강; CLI 내부 token 강제 한도는 별도 한계로 명시 |
| typed delegation 설정과 도구 호출 전 intervention | OMS는 CLI dispatch·승인·patch 경계를 소유 | 고정·상속·선택 가능 항목을 기존 assignment 검증과 실행 전 검사에 대응; 새 설정 DSL은 필요성이 입증될 때만 추가 |
| output·trace·session 평가, simulator·failure detector | OMS 관측 집계와 결정론적 acceptance는 의미 평가 SDK를 대체하지 않음 | 기존 receipt·trace·acceptance로 결과와 실행 과정의 평가 사례를 확장; 모델 기반 평가는 별도 명시 실행 |

이는 기능·통합 수준의 우위다. 더 높은 작업 성공률·더 낮은 비용·더 빠른 실행은 이번에
검증하지 않았다. 위 표의 내부 도입 방향은 후속 설계이며 완료 기능 목록이 아니다.
OMS core의 Python 3.9와 native CLI 계약을 유지한다. Strands가 소유한 내부 모델 루프와
OMS가 소유한 실행 경계의 차이는 구현 위치와 보장 범위를 정하는 제약이다.

의존성 근거: [harness manifest](https://github.com/strands-agents/harness-sdk/blob/c56b7dea985c0c596ab021c46d3d4d15b4c35b67/harness-py/pyproject.toml),
[SDK manifest](https://github.com/strands-agents/harness-sdk/blob/c56b7dea985c0c596ab021c46d3d4d15b4c35b67/strands-py/pyproject.toml).
Strands 소스를 복사하지 않았으며 추가 의존성·새 MCP 도구·새 CI 작업은 없다.

## 토론에서 바뀐 판단

- GPT-6 Astra는 처음에 operation에 묶인 범위 조회를 추천했지만, 최종 반박에서는
  소비자 필요가 확인되지 않은 새 읽기 경계보다 Claude의 preview 재사용안을 채택했다.
- Claude는 메모리 API가 동등하다는 초기 주장을 철회했다. 같은 markdown 저장 형식은
  every-turn 주입·추출·검색 생명주기가 같다는 뜻이 아니다.
- 양쪽 모두 거절 이유를 알리지 않는다는 초기 주장도 정정했다. OMS는 child-policy 오류를
  반환한다. Strands는 일부 도구/MCP 선택을 필터링하지만 다른 잘못된 설정은 오류를 낸다.
- 부모 검토는 label·separator를 먼저 예산에 넣고, 파일을 다시 읽는 대신 이미 추출한 답변의
  digest를 계산하도록 좁혔다. 합의는 정확성 증거로 취급하지 않고 기존 테스트로 검증한다.

원본 토론은 로컬 thread `strands-adoption-0927`에 남았다. 자동 반박 직전 부모가 테스트를
추가하여 revision guard가 실행을 중단했다. 수정분을 따로 보관하고 원래 기준 소스를 복원한
뒤 두 seat 모두 상대의 원문을 읽는 최종 반박을 완료했다. 중단된 단계를 완료로 세지 않았다.

## Strands에서 확인한 점

| 영역 | 확인한 구현 또는 계약 | OMS 적용 판단 |
| --- | --- | --- |
| 큰 결과 처리 | 결과를 외부 저장소에 두고 preview/reference로 대체하며 범위·검색 재조회를 제공 | 가장 직접적인 참고 대상 |
| 위임 설정 | `Fixed`, `Inherit`, `Choice`, `Open`으로 모델이 정할 수 있는 항목을 구분 | 개념을 참고하되 새 DSL보다 기존 assignment 검사에 결합 |
| 자식 메모리 | 기본 delegate는 공유 메모리를 읽되 추출·쓰기는 제한 | 기존 OMS worker 격리와 대조; 별도 자동 메모리 도입은 불필요 |
| 개입 | 도구 실행 직전 정책 검사·거절·재안내를 지원 | OMS가 소유한 dispatch/admission 경계에서만 적용 가능 |
| 실행 한도 | invocation별 turn/token 한도와 종료 사유 | 현재 wall/cycle/attempt 제한 재사용; 금액 상한 보장으로 확대 해석 금지 |
| 세션과 메모리 | 한 작업 재개와 작업 간 지식 축적을 분리 | OMS의 plan/receipt와 memory 분리를 유지 |
| 백그라운드 실행 | 동시 실행·완료·timeout 정책을 제공 | 기존 operation/cursor/대기 방식과 중복; 기본 병렬화 확대는 보류 |
| 평가 | 결과·실행 과정·세션을 나눠 평가하고 실패 진단을 별도로 취급 | 검증 통과와 provider 정상 종료를 계속 분리 |
| 자동 최적화 | trace와 reward를 보고 노출된 prompt parameter를 수정 | 관측과 후보 제안만 참고; 활성 정책 자동 수정은 부적합 |
| 실행 격리 | 기본 host 실행 환경과 별도 가상 Strands Shell은 다른 경계 | 이름만으로 host 보호를 가정하지 않음 |

근거: [context/offloading](https://strandsagents.com/docs/user-guide/harness/configure/context-and-caching/),
[위임 소스](https://github.com/strands-agents/harness-sdk/blob/c56b7dea985c0c596ab021c46d3d4d15b4c35b67/harness-py/src/strands_harness/tools/subagent.py),
[자식 메모리 소스](https://github.com/strands-agents/harness-sdk/blob/c56b7dea985c0c596ab021c46d3d4d15b4c35b67/harness-py/src/strands_harness/memory.py),
[interventions](https://strandsagents.com/docs/user-guide/harness/configure/interventions/),
[lifecycle controls](https://strandsagents.com/docs/user-guide/sdk/agents/lifecycle-controls/),
[checkpoints/memory](https://strandsagents.com/docs/user-guide/harness/configure/checkpoints-and-memory/),
[background tasks](https://strandsagents.com/docs/user-guide/harness/configure/background-tasks/),
[evaluation](https://strandsagents.com/docs/user-guide/evals-sdk/how-evaluation-works/),
[optimizer](https://strandsagents.com/blog/introducing-harness-optimizer/),
[shell tools](https://strandsagents.com/docs/user-guide/harness/tools/shell-and-files/).

## 1. 큰 결과에서 필요한 근거만 다시 읽기

### 현황과 차이

OMS의 `ma_emit_bounded_prompt_file`은 크기를 제한하고 생략량을 표시한다.
기준 revision의 `oms_peer_result`는 각 응답에 예산을 배분하고 artifact 경로와 결과 cursor를 반환했다.
원본을 버리는 설계는 아니며 shell을 가진 부모는 직접 파일을 읽을 수 있다.
부족한 것은 얇은 MCP 클라이언트에서도 같은 operation의 특정 결과를 안전하게
범위 지정해 읽는 공통 인터페이스다. 현재 cursor는 변경 여부 확인용이며 페이지 위치가 아니다.

소스: [출력 제한](../scripts/lib/peer-common.sh),
[MCP 결과](../scripts/oms-mcp-server.py), [artifact 색인](../scripts/artifact-index.sh).

관찰 실험: 11,219-byte 합성 결과의 중간에 근거를 두고 256-byte 예산으로 전달했다.
326-byte 결과(생략 안내 포함)에는 중간 근거가 없고 원본에는 남았다.
이는 기존 제한이 계약대로 작동한다는 증거이며, 실제 모델 성능 저하나 비용 절감의 증거는 아니다.

### 후속 구현: OMS 자체 범위 재조회

사용자의 자체 기능 도입 요청에 따라 보류했던 재조회를 구현했다. 기존 `oms_peer_result`에
`answer_ref`, `answer_offset`, `answer_limit`를 추가했다. 줄이 매우 길어도 출력 한도를 지키도록
줄 번호 대신 정규화한 답변의 UTF-8 byte 위치를 사용하며, 다국어 문자를 중간에서 자르지 않는다.

- 새 MCP 실행은 `.oms/artifacts/mcp/OPERATION/answers/`에 결과를 저장한다. consult/advise에
  기존 ask와 같은 `--artifact-dir` 옵션을 추가했다. 직접 CLI의 기본 저장 위치는 그대로다.
- 시작 응답이 실제 위치를 반환한다. 완료 결과의 `answer_details[].read_arguments`로
  한 답변을 조회하고 `next_read_arguments`로 이어 읽는다. 페이지 본문은 최대 16 KiB이며
  JSON 전체 크기 제한과는 다르다. 질문이 인용된 prompt 대신 기존 parser의 답변만 반환한다.
- 식별자는 실행·파일명·정규화한 내용에 묶인다. 내용 변경·다른 실행·삭제·링크·경로 이탈을
  거절한다. CRLF/LF만 다른 같은 답변은 식별자가 유지된다. digest는 권한 인증이 아니다.
- 입력 파일은 8 MiB까지 읽고 같은 파일 descriptor의 bounded snapshot을 파싱한다.
  파일·상위 디렉터리 교체를 검사한다. 같은 권한으로 로컬 상태를 악의적으로 조작하는
  프로세스에 대한 sandbox를 제공하는 것은 아니다.
- 부분 실패 seat의 exit를 보존하고 오류로 반환한다. 조회는 모델 호출·캐시 쓰기를 하지 않는다.
  과거 실행은 기존 preview와 원본 파일 조회를 유지한다. 후속 literal 검색은 아래에 기록한다.

Strands의 [offloader 소스](https://github.com/strands-agents/harness-sdk/blob/c56b7dea985c0c596ab021c46d3d4d15b4c35b67/strands-py/src/strands/vended_plugins/context_offloader/plugin.py)는
reference 기반 재조회와 줄 범위를 제공한다. [Deep Agents의 context 설계](https://www.langchain.com/blog/context-management-for-deepagents)도
큰 내용을 보관하고 필요한 부분만 가져오는 방향을 뒷받침한다. OMS에는 기존 artifact가 있으므로
저장 계층을 복제할 이유가 없다.

검증은 기존 MCP suite에 큰 결과의 중간 근거, 전체 페이지 합산과 원본의 일치,
다국어 byte 경계, 변경·삭제·링크·다른 실행·크기 제한과 실패 seat를 추가한다.
이는 조회 정확성 검사이며 모델의 근거 선택 품질이 개선됐다는 증거는 아니다.

## 2. 위임 설정의 권한과 출처를 설명하기

OMS는 `task-assignment.py`, model routing, provider contract와 immutable autopilot receipt를
이미 갖고 있다. 특히 collaboration auto의 Codex 경로는 Astra/Sol/Luna와 fallback을 검사한다.
이는 Strands 설정 DSL로 교체할 대상이 아니다.

참고할 부분은 모델에 노출하는 선택지와 소유자가 고정한 설정의 명확한 분리다.
먼저 기존 진단 결과에 `requested → resolved → selection source → enforcement boundary`를
충분히 설명하는지 확인한다. 빠진 경우에만 기존 routing receipt/doctor 출력을 확장한다.
모델·fallback·reasoning·context·허용 경로를 별도 설정 파일에 중복 저장하지 않는다.

예: `Codex model = gpt-6-sol, source = collaboration default, alternatives = Astra/Sol/Luna`.
provider 내부 도구 권한은 실제 실행기가 강제하는 것과 OMS가 사후 검출하는 것을 나눠 표시한다.
네이티브 앱의 모든 하위 에이전트까지 이 정책이 강제된다고 주장해서는 안 된다.

Strands는 일부 off-enum 입력에 기본값으로 돌아가는 테스트도 있다.
OMS에서 허용하지 않은 모델·권한 요청은 기존처럼 명시적으로 거절해야 한다.
[위임 테스트](https://github.com/strands-agents/harness-sdk/blob/c56b7dea985c0c596ab021c46d3d4d15b4c35b67/harness-py/tests/test_subagent.py),
[OMS assignment](../scripts/lib/task-assignment.py), [OMS routing](../scripts/lib/model-routing.sh).

검증: 기존 routing/assignment suite에서 명시 값 우선, 승인 범위 축소,
금지 fallback, resume 중 설정 변경을 확인한다. 이미 같은 증거가 있다면 코드 변경 없이 유지한다.

## 3. 품질과 비용을 같은 작업에서 비교하기

OMS에는 [runtime benchmark](../scripts/lib/oms_runtime/benchmark.py),
[skill evaluation](../custom-skills/oms-agent-harness/references/skill-lifecycle.md),
artifact telemetry와 acceptance evidence가 있다. 폐기한 semantic-eval 엔진을 부활시키거나
새 평가 서비스를 상시 실행할 필요는 없다.

Strands Evals는 output과 trajectory를 구분하고, Harness Optimizer는 성공/실패 trace를
대조해 후보를 만든다. OMS에는 이 절차를 작은 고정 실험으로 가져오는 것이 적절하다.
[평가 구조](https://strandsagents.com/docs/user-guide/evals-sdk/how-evaluation-works/),
[Harness Optimizer](https://strandsagents.com/blog/introducing-harness-optimizer/),
[Anthropic의 agent 평가 지침](https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents).

제안하는 초기 fixture는 긴 오류 로그, 중간에 핵심 근거가 있는 리뷰,
다중 seat의 일부 실패, 비밀정보가 포함된 결과, 삭제·변경된 artifact,
중단 후 재개 작업이다. 단위 테스트는 기존 suite에 넣고 모델 기반 A/B는 별도 명시 실행으로 둔다.

비교 조건을 task/source SHA, fixture digest, model, effort, provider/CLI 버전,
설정 digest, acceptance로 고정한다. 한 쌍에서 한 설계만 바꾸고 baseline과 treatment를
독립 실행한다. 실제 모델 비교는 반복 횟수와 호출 상한을 실행 전에 정한다.
prompt 수정에 사용하지 않은 holdout 사례를 분리한다.

측정은 검증된 정답률, 누락 근거, 잘못된 완료 선언, 잘못된 거절,
반복 읽기, wall time, provider가 보고한 token/cost와 그 누락률로 나눈다.
수동 라벨과 자동 판정도 구분한다. `prompt_bytes` 감소만으로 token·청구액 절감이라 하지 않는다.
기존 benchmark의 집계 차이는 인과적 A/B의 대체물이 아니다.

채택 조건: 권한/데이터 보호 실패가 없고, fixture 정답이 보존되며,
대표 작업에서 추가 재조회 비용을 포함한 개선이 관찰될 것. 유의미한 개선이 없으면
현재의 단순한 결과 경로를 유지한다. LLM judge 점수만으로 patch admission을 통과시키지 않는다.

## 그대로 도입하면 충돌하는 부분

- **숫자 복사:** Strands 발표는 6개 benchmark에서 28% 비용 감소를 보고한다.
  OMS의 workload와 다른 결과이며 재현하지 않았다. source의 1,500/750-token offload
  설정이나 발표의 85% compaction 임계값을 OMS byte 예산에 그대로 옮기지 않는다.
  [발표와 측정 조건](https://strandsagents.com/blog/introducing-strands-harness/),
  [factory 상수](https://github.com/strands-agents/harness-sdk/blob/c56b7dea985c0c596ab021c46d3d4d15b4c35b67/harness-py/src/strands_harness/agent.py).
- **비용 상한:** Strands token 한도는 다음 loop 시작 전에 검사하여 한 turn의 초과가 가능하다.
  OMS의 CLI 밖 측정은 더 제한적이다. 인증된 강제 수단 없이 엄격한 금액/토큰 상한을 약속하지 않는다.
  [lifecycle 계약](https://strandsagents.com/docs/user-guide/sdk/agents/lifecycle-controls/),
  [OMS 경계](COMPONENTS.md#durable-operations-and-optional-frontends).
- **자동 메모리/최적화:** 기본 background extraction과 자동 prompt rewrite를 활성화하면
  기록 권한·모델 선택·추가 호출 비용과 충돌할 수 있다. 현재 메모리 정책을 유지하고 후보는 검토 가능하게 둔다.
- **프롬프트 중복:** Strands의 행동 지침을 global-AGENTS에 덧붙이면 기존 정책과 겹친다.
  외부 콘텐츠의 특정 태그를 신뢰하도록 하는 규칙도 그대로 복사하지 않는다.
  [Strands prompt](https://github.com/strands-agents/harness-sdk/blob/c56b7dea985c0c596ab021c46d3d4d15b4c35b67/harness-py/src/strands_harness/prompt.py).
- **격리 착각:** Strands harness 기본 shell은 host 실행 경로다. 별도 Strands Shell은
  userspace 가상 환경으로 임의 host 실행을 그대로 대체하지 않는다. hard resource containment와
  적대적 다중 tenant에는 별도 OS 격리가 필요하다고 문서도 명시한다.
  [Shell 보안 경계](https://strandsagents.com/docs/user-guide/shell/security/).
- **재개와 exactly-once 혼동:** session 복원만으로 외부 효과 중복이 방지되지 않는다.
  OMS의 intent/reconcile·정확한 patch 승인과 단일 writer 경계를 보존한다.
  Strands 기본 session store도 분산 lock을 제공하지 않는다고 명시한다.
  [Strands 동시성](https://strandsagents.com/docs/user-guide/harness/configure/checkpoints-and-memory/),
  [LangGraph persistence](https://docs.langchain.com/oss/python/langgraph/persistence).
- **버전 안정성:** Python/TypeScript release와 기능은 일치한다고 가정할 수 없다.
  문서 예제 모델과 조회한 HEAD의 기본 모델도 달라, 이 보고서는 모델 기본값을 채택 근거로 삼지 않는다.
  [versioning](https://strandsagents.com/docs/user-guide/harness/versioning/).

Anthropic의 장기 실행 사례는 작은 단위의 작업, 재개 자료, 실제 완료 검증을 강조한다.
OMS의 plan·receipt·acceptance가 이미 같은 문제를 다루므로 별도 progress 파일이나
자동 commit 규칙을 추가하지 않는다.
[장기 실행 harness 사례](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents).

## 반영한 변경과 검증 경계

`oms_peer_result`는 원본 artifact와 기존 인자를 유지하면서 기존 UTF-8 head/tail 함수를
재사용한다. 각 seat의 label·exit와 separator 예산을 먼저 확보하여 뒤 seat의 결론이
최종 합산 단계에서 잘리지 않게 한다. framing 자체가 몫보다 크면 `inline_omitted`로
명시하고 artifact·exit를 metadata에 보존한다.

잘린 결과에는 `answer_truncated`와 `answer_details`를 추가한다. 각 항목의 `answer_bytes`와
`answer_sha256`는 기존 parser가 정규화·추출한 **답변** 기준이며, 원본 파일 전체의 크기·hash가
아니다. `preview_bytes`는 생략 marker를 포함하고 seat label은 제외한다. digest는 인증이나
내용 검증을 뜻하지 않는다. 본문이 같아도 생략된 중간이 바뀌면 digest와 result cursor가 바뀐다.
짧은 결과는 기존 응답 형식을 유지한다. 답변 본문의 60,000-byte 한도와 JSON 전체 크기는 다르다.

부모가 확인한 현상:

- 초기 로컬 ask artifact 287개를 기존 parser로 읽어 34개에서 20KB, 29개에서 60KB를 넘는
  답변을 추출했다. 초기 토론에서 이를 모두 완료 답변이라고 표현한 것은 부정확했다.
  최종 재집계는 artifact 290개, 비어 있지 않은 답변과 exit가 있는 225개, 그중 exit 0인
  193개이며, 답변 60KB 초과는 29개였다. 실제 판단 오류율이나 대표 분포의 측정은 아니다.
- 기준 코드의 105KB 합성 답변은 끝의 `Recommendation`을 누락하고 60,012 bytes를 반환했다.
- 변경 후 합계 약 660KB인 한국어·이모지 3-seat fixture에서 세 시작·결론을 모두 보존하고
  preview 본문은 60,000 bytes 이하였다. 중간 근거 전체를 복원하는 개선은 아니다.
- 기존 `tests/state-surfaces-smoke.sh`에 단일/다중 seat, CRLF, 원본 불변, 작은 응답,
  비정상 seat exit, 생략 영역 변경, 동일 cursor 재조회, framing 부족의 회귀 사례를 확장했다.
- Claude의 diff 검토는 구현 결함을 찾지 않았지만 중간 변경 시험이 크기 변화만으로도 통과할 수
  있음을 지적했다. 동일 byte 길이의 교체와 CRLF/LF 정규화만 바꾸는 사례를 추가하여 보완했다.

검증 결과는 아래에 기록한다. Strands 실행 성능·비용·답변 품질, 다른 OS에서의 실행은
로컬 synthetic/회귀 검사로 입증하지 않는다. 설치본 갱신·커밋·푸시는 이번 범위에 포함하지 않았다.

- 기존 MCP/state suite: 통과. 추가 회귀를 기준 코드에 적용하면 결론 누락으로 실패했다.
  테스트 준비 중 파일을 바꾼 한 실행은 무효화하고, 소스를 고정한 뒤 다시 통과를 확인했다.
- `bash scripts/check.sh --parallel`: 첫 실행 exit 1. PR 취소 fixture가 약 5초 안에 push
  시작 신호를 보지 못했다. 나머지 세 focused lane, 정적 검사, 610개 scripts smoke는 통과했다.
- 같은 소스로 `bash scripts/check.sh --focused-only --focused-lane 4/4`를 다시 실행하여
  exit 0을 확인했다. PR 취소와 최종 보강한 MCP/state 사례, 앞선 실패로 실행되지 못했던
  후속 검사까지 통과했다. 필수 stage 목록과 두 실행의 통과 기록을 대조해 65/65를 확인했다.
  첫 전체 명령 자체를 성공으로 기록하지 않는다. 병렬 부하에 따른 시작 지연은 가능한 설명이며,
  첫 실패의 정확한 원인은 재현·확정하지 못했다. CI나 다른 OS에서 새로 실행한 결과도 아니다.
- 당시 문서 상대 경로 8개: 누락 없음. 참고한 외부 URL을 본문에 기록했다.

후속 MCP 맥락 연결 검증:

- 기존 MCP/state suite를 다시 통과했다. 실제 detached consult 경로에서 기록한 메모리가
  모의 provider 입력에 도달함을 확인했다. 모델의 답변 품질을 측정한 검사는 아니다.
- consult/advise/ask의 생략·포함·제외 옵션과 실행 기록·목록을 검사했다. 잘못된 타입 및
  message/ack 옵션은 디렉터리·thread·provider를 생성하기 전에 거절됨을 확인했다.
- 기존 CLI 회귀 6개를 선택 실행하여 통과했다: 메모리 opt-in, agent-call 맥락 opt-in,
  outbound 거절 시 다른 seat로 우회하지 않음, 민감 메모리 제외, 질문 기반 recall,
  worker의 공유 메모리 쓰기 금지. 새 suite나 CI 작업은 추가하지 않았다.
- lint 전체(shellcheck, Bash 호환성, Python 문법, skill manifest)와 문서 참조 검사를 통과했다.
  이 후속 변경에 대해 전체 gate·원격 CI·설치 검증을 다시 실행한 것은 아니다.

후속 범위 재조회 검증:

- 기존 MCP/state suite 통과. 약 240 KB 한국어·이모지 답변의 모든 페이지를 합쳐 원본 답변과
  일치함을 확인하고, preview에서 빠진 중간 근거를 직접 조회했다. prompt는 반환되지 않았다.
- 변경·삭제·미완료·과거 실행·다른 실행의 식별자·문자 중간 offset·파일 크기 초과·실패 seat를
  검사했다. 이 Linux 실행에서는 파일/디렉터리 symlink와 hard link 거절도 통과했다.
- 기존 CLI 회귀 7개, debate-delta, provider-permissions-mcp-boundary, lint 전체와 문서 참조
  검사 통과. advise의 공백 포함 사용자 지정 저장 경로를 기존 fixture에 추가하고 재검했다.
- Claude의 읽기 전용 검토에서 비ASCII 파일명 거절과 macOS 테스트 경로 별칭 문제가 지적됐다.
  조회기의 안전한 파일명 규칙을 유니코드로 확장하고 fixture 경로를 정규화했다. 한국어 파일명으로
  MCP/state suite를 다시 통과했고 Python 문법·스킬 계약·문서 참조도 재검했다. 실제 macOS/Windows
  실행으로 확인한 결과는 아니며, 검토자의 플랫폼별 slug 생성 주장을 검증 완료로 취급하지 않는다.
- 8 MiB 제한은 prompt를 포함한 artifact 전체와 정규화한 답변 각각에 적용된다. 초과한 새 실행은
  preview도 제공하지 않고 명시적으로 오류를 반환한다. 매 페이지의 전체 파일 읽기·해시 비용을
  문서화했다. 모델 성능·과금 절감이나 효율적인 전체 파일 다운로드를 입증한 것은 아니다.
- Strands 설치·import·실행 백엔드·새 CI 작업을 추가하지 않았다. 전체 릴리스 gate, 원격 CI,
  커밋·푸시·설치는 이 범위 재조회 검증 기록 시점에는 실행하지 않았다.

## 2026-09-28: 취소·검색·관련 메모리·효과 측정

### 실제 취소와 상태 일치

이전 MCP Tasks 취소는 요청 파일만으로 `cancelled`를 반환했다. 새 POSIX 실행은 기존
`run-bounded.py`가 살아 있는 자신 소유의 provider process group을 종료한다. 요청 이후
시작하려는 provider 호출은 생성 전에 거절한다. 저장된 PID를 읽어 외부에서 신호를 보내지 않는다.
요청 접수는 `working`과 `cancellation_requested`로 나타내고, 실행기가 취소를 관측한 기록과
CLI 종료가 모두 있어야 `cancelled`가 된다. 정상 종료 뒤 늦게 온 요청만으로 완료를 바꾸지 않는다.
부분 결과는 남기며 자동 재호출은 없다. 과거 실행과 native Windows 실행은 취소를 명시적으로
거절한다. 원격 과금 중단이나 의도적으로 분리한 외부 job의 종료까지 보장하는 기능은 아니다.

### 답변 검색

`answer_ref`와 `answer_query`로 1–256 UTF-8 byte의 대소문자 구분 literal을 검색한다.
최대 8개 일치 위치와 주변 문맥, 범위 조회 인자를 반환한다. `next_search_arguments`로 다음
일치를 읽는다. regex는 실행하지 않으며 변경된 식별자·다른 실행·경로 거절은 범위 조회와 같다.

### 관련 메모리와 선택 근거

메모리 첨부가 활성화된 위임과 MCP 상담은 기본적으로 `relevant` 모드를 사용한다. 고정 메모와
질문에 관련된 recall만 전달하고 관련 없는 최근 summary는 제외한다. 명시적 환경 설정은 우선하며
일반 CLI의 기존 compact 기본값은 유지한다. 프로젝트에서 유효한 관련 기록을 찾지 못하면 global
recall을 시도한다. 기존 출처 변경 검사는 유지하고 선택 이유·source·event·기록 시점을 출력한다.
재개 상담도 현재 목표를 질문에 포함해야 하며, 모든 hook에 메모리를 자동 주입하거나 모델 기반
자동 기록을 추가한 것은 아니다. 기존 메모리 저장소와 접근 이력을 재사용한다.

### 고정 사례의 관측

기존 MCP suite의 같은 약 240 KB 답변에서 중간 근거를 찾았다. 한 로컬 실행에서 순차 범위 읽기는
15회·270,298 JSON bytes·0.0343초였고, 검색과 범위 읽기는 2회·18,013 JSON bytes·0.0046초였다.
두 경로 모두 기대한 근거를 얻었고 순차 합산은 원본 답변과 일치했다. 시간은 매번 출력하는 관측값이며
CI 통과 기준으로 삼지 않는다. 모델 호출·실제 업무 정확도·provider token이나 비용을 측정한 것은 아니다.
메모리 fixture는 관련 parser 기록이 남고 무관한 deployment 기록이 제외되는지 검사한다.
새 benchmark 서비스·테스트 suite·CI job 없이 기존 검사를 확장했다.

기능 통합 suite, 메모리·timeout 회귀 8개, 검토 후 보강한 경계 회귀 3개를 통과했다.
Claude의 읽기 전용 검토는 차단 결함을 찾지 않았으며, 부모는 신호 전 정상 종료 재확인,
receipt 기록 실패와 Python 누락의 명시적 오류, 메모리 출처 표기를 보완했다.
lint의 네 정적 검사 항목은 통과했으나 같은 체크아웃의 검토 기록이 갱신되어 전체 lint 명령은
상태 불변성 검사에서 실패했다. 이를 성공으로 세지 않으며 최종 전체 gate는 `oms land`가
분리된 커밋 작업 폴더에서 실행한다.

검증과 릴리스 결과는 이번 실행의 gate/landing receipt와 함께 확인한다. 이 문서는 릴리스 전
작성했으므로 원격 CI나 설치가 완료됐다는 증거로 사용하지 않는다.
