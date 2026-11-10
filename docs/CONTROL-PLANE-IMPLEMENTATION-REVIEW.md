# OMS 최근 구현 리뷰와 UX 의견 조정

검토일: 2026-11-09 (KST). 최초 기준 main: `ff3d8e3ec16cc1ca835f5fc5968445e9657a273b`.
추가 변경 확인·게시 기준: `e055c4d0ca3ccd8b5c127b70dd85cd959a0b6282`.
상태: **리뷰/후속 작업 제안. 런타임 수정 또는 기능 완료 보고가 아니다.**
기존 [기반 계획](CONTROL-PLANE-EVOLUTION.md)과 [UX 설계](CONTROL-PLANE-UX-AND-FEATURES.md)를 대체하지 않는다.

2026-11-10의 현재 구현·열린 PR 대조와 다음 작업은
[비교 조사 후속 구현·검증 작업](INTEGRATION-FOLLOWUP-20261110.md)을 참고한다.
이 문서의 과거 snapshot과 구분하며, 새 계획 자체가 기능 완료를 뜻하지 않는다.

## 1. 실제로 확인한 작업과 관측 한계

조회 당시 열린 PR은 0개였고 #14와 #15는 병합돼 있었다.
원격 branch는 main과 이전 문서 branch 2개였다. 별도 신규 기능 PR이나
Claude 로컬 세션·미푸시 변경은 확인하지 못했다. 같은 GitHub 계정의 commit만으로
작성한 모델을 식별할 수 없으므로 아래를 “Claude의 확인된 발언”으로 읽지 않는다.
사용자가 언급한 병행 작업과 충돌하지 않도록, 게시된 구현과 기존 내 제안의 차이를 검토했다.

`20ce1be..ff3d8e3`의 변경 목록은 9 commits / 17 files였다.
목록 전체를 확인했지만 모든 파일을 전수 감사하거나 전체 suite를 실행한 것은 아니다.
집중 범위는 다음 4개 commit의 patch와 관련 현재 함수·문서·fixture다.

| 게시된 변경 | 판단 |
|---|---|
| [61e918d — continuation 실행 저장소 복원](https://github.com/eightmm/oh-my-setting/commit/61e918db5eb25992237685ce056e44fdfa286422) | 방향에 동의. 다만 state/source 분리와 충돌하는 비교 대상은 R1에서 수정 제안 |
| [2532130 — Antigravity 오류 보존/분류](https://github.com/eightmm/oh-my-setting/commit/25321307c4bfa8c46aa8fef9742fcfa17e6a779c) | 원문 원인 보존과 제한된 fallback에 동의. stop reason의 과잉 분류는 R2 |
| [7b645e0 — Python 3.11 block limit 대응](https://github.com/eightmm/oh-my-setting/commit/7b645e070e24384a07df521b276f0a9322c1ebda) | ExitStack 교체에 동의. AST 보조검사는 실제 3.11 컴파일 검증의 대체물이 아님 |
| [eee5fa5 — 비-core Bash 코드 정리](https://github.com/eightmm/oh-my-setting/commit/eee5fa5ebc9fb8b38282f72bc32dfe9b83daf719) | 런타임 최소 버전 안에서 단순화하는 방향에 동의. contributor 문서 불일치는 R3 |

검토 도중 main에 batch 1109d의 4 commits / 14 changed files가 추가되어
아래 세 기능 commit의 patch와 관련 현재 함수를 추가로 확인했다.
R1의 panel_dispatch, R2의 peer-conversation, R3의 기여 문서는 추가 변경에 포함되지 않아
해당 지적은 게시 기준에서도 남아 있다. 새 기능 PR이 아니라 main에 게시된 변경이었다.

| 추가 변경 | 동의하는 점과 남은 검토 |
|---|---|
| [f29d2eb — ? Keys footer/대비](https://github.com/eightmm/oh-my-setting/commit/f29d2eb295231faa2263f996dfa933bd65b81ed9) | 좁은 화면에서 ? Keys를 우선하고 NO_COLOR 검사를 추가한 점에 동의. 실제 접근성/사용성 측정과는 구분 |
| [a2d2bf3 — exit-0 non-answer 표시](https://github.com/eightmm/oh-my-setting/commit/a2d2bf3d2aa8713c9fa41ab114ad0b754129df22) | 프로세스 성공과 쓸 수 있는 답변을 분리한 점에 동의. 최신 라운드 선택의 빈틈은 R5 |
| [dfdd7fe — coverage matrix/중복 세션 거부](https://github.com/eightmm/oh-my-setting/commit/dfdd7fe001d0bb8aadf77d9c48e2fa55908db4d8) | fixture-only/미검증 표시와 CP-01 세 조건 기록을 인정. warm-cache의 중복 감지는 R4 |

기존 CP-01의 새 완료 표시를 되돌리지 않는다. 다만 문서 정리의 완료는 native 실행
검증의 완료가 아니다. 현재 matrix가 밝힌 Codex의 collector/renderer 차이는
새 전역 discovery보다 먼저 확인할 작은 UX 개선 후보다.

새 모델 호출, provider 실행, 실제 연구 저장소 변경, GPU/Slurm 제출은 하지 않았다.
아래 재현은 Python 3.13.5와 임시 Git 저장소에서 **현재 함수의 해당 부분을 그대로 옮겨**
실행한 최소 재현이다. 전체 OMS import/CLI/integration 실행과 구별한다.

## 2. 구체적인 수정 요청

### R1 · 높음: continuation이 state repo와 execution repo를 같은 Git 저장소로 가정

근거: [panel_dispatch.py L540–636](https://github.com/eightmm/oh-my-setting/blob/ff3d8e3ec16cc1ca835f5fc5968445e9657a273b/scripts/lib/panel_dispatch.py#L540-L636),
[별도 state/execution 계약](https://github.com/eightmm/oh-my-setting/blob/ff3d8e3ec16cc1ca835f5fc5968445e9657a273b/docs/TERMINAL-PANEL.md#state-and-execution-repositories).

`continuation()`은 `OMS_ROOM_REPO`가 있으면 `repo`를 상태 저장소로 바꾼 다음,
기록된 실행 경로를 `_recorded_execution_repository(repo, recorded)`에 넘긴다.
이 helper는 실행 경로의 `--git-common-dir`가 그 상태 저장소와 같아야 통과시킨다.
문서상 상태와 실행 저장소는 분리할 수 있으므로, 별도 Git 저장소/clone의 정상적인
실행 worktree도 `--continue`에서 `--repo`를 생략하면 거부된다.

최소 재현: 독립적인 `state`와 `source` Git 저장소를 만들고 source의 detached worktree를
`worker`로 만들었다. 현재 helper에 `(source, worker)`를 넘기면 통과하지만
`(state, worker)`는 `not a worktree root of this repository`로 거부됐다.
기존 추가 fixture는 같은 home/common-dir 경로를 검사하며, 이 분리 구성을 다루지 않는다.

**권장 수정:** 상태 저장소는 room/owner/attempt 증명의 기준으로 유지하고, 실행 저장소는
원래 launch의 신뢰 가능한 실행 identity에 대조한다. `git-common-dir` 확인을 없애거나
기록된 경로를 무조건 신뢰하는 수정은 반대한다. 필요한 provenance가 없는 legacy 기록은
명시적 `--repo` 확인을 유지한다. 현재 명시적 `--repo` 경로는 별도 동작이므로
“모든 continuation이 실패한다”거나 데이터 손상/권한 우회가 재현됐다는 뜻은 아니다.

수락 범위: 같은 repo의 worktree, 별도 state/source, 별도 clone, 명시적 override,
삭제·교체된 경로, 다른 repo로 바뀐 경로, 잘못된 launch/room 소유권.
기존 `tests/operator/dispatch_results.py`의 continuation fixture를 확장한다.

### R2 · 중간: 원인 불명 오류도 stream_truncated로 확정되고 표시 상한이 분류를 바꿈

근거: [peer-conversation.py L57–112](https://github.com/eightmm/oh-my-setting/blob/ff3d8e3ec16cc1ca835f5fc5968445e9657a273b/scripts/lib/peer-conversation.py#L57-L112),
[provider error scanner](https://github.com/eightmm/oh-my-setting/blob/ff3d8e3ec16cc1ca835f5fc5968445e9657a273b/scripts/lib/peer-common.sh#L3160-L3275).

`agy_turn_error()`는 오류 문자열을 240자로 먼저 자른 뒤 token-limit 패턴을 검사한다.
그 패턴이 아니면 실제 원인이 무엇이든 `reason=stream_truncated`를 기록한다.

| Synthetic 입력의 error | 현재 stop reason | 리뷰 판단 |
|---|---|---|
| `The stream was interrupted.` | `stream_truncated` | 정상 대조군 |
| `Your response exceeded the output token limit` | `max_tokens` | 정상 대조군 |
| `something else broke` | `stream_truncated` | 근거 없는 원인 확정 |
| `authentication failed` | `stream_truncated` | 인증 오류를 전송 중단으로 오인 |
| `"x" * 241 + " output token limit"` | `stream_truncated` | 표시를 위한 truncation 때문에 token-limit 분류 누락 |

**권장 수정:** 신뢰 가능한 typed status/code가 있으면 우선 사용하고, 이미 제한된
provider envelope의 오류 필드에서 원인을 분류한 뒤 표시 문자열을 별도로 자른다.
확인된 중단/idle timeout과 token limit만 해당 reason에 매핑하고 나머지는
기존 계약에 맞는 unknown/generic error로 남긴다. 새 reason 도입은 consumer와 schema를 함께 점검한다.

중요한 구분: 현재 별도 fallback scanner는 Antigravity의 알려진 두 transient 문구만
`provider_down`으로 분류한다. **Unknown 오류에서 실제 fallback/재시도가 발생했다는
재현은 아니다.** 이 보수적인 경계는 유지하며 stop reason과 UI 설명의 정확성을 고친다.
기존 `tests/scripts-smoke.sh`의 agy error fixture에 unknown reason, long error,
명시적 실패·정상 turn·permission·본문에 인용된 오류 문구 사례를 보강한다.

### R3 · 낮음: Bash 최소 버전이 기여 문서와 실제 규칙에서 다름

[AGENTS.md](https://github.com/eightmm/oh-my-setting/blob/ff3d8e3ec16cc1ca835f5fc5968445e9657a273b/AGENTS.md)는
Bash 5.0+를 요구하지만 [CONTRIBUTING.md](https://github.com/eightmm/oh-my-setting/blob/ff3d8e3ec16cc1ca835f5fc5968445e9657a273b/CONTRIBUTING.md)의 Style은 Bash 3.2다.
최근 비-core 코드 정리는 Bash case conversion 등을 사용한다.
이는 새 기능 제안이 아니라 현재 contributor 안내의 불일치다.

런타임 5.0+와 설치 bootstrap/core lifecycle에 남겨야 할 호환성 범위를 명시하고
CONTRIBUTING/help/CI 안내를 맞추는 작은 문서 수정이 필요하다.
이 리뷰 PR은 지원 버전이나 installer 기본값을 변경하지 않는다.

### R4 · 중간: 중복 세션 거부가 cold cache에서만 동작

근거: [현재 _main_transcript](https://github.com/eightmm/oh-my-setting/blob/e055c4d0ca3ccd8b5c127b70dd85cd959a0b6282/scripts/lib/panel_chats.py#L154-L168),
[dfdd7fe의 fixture](https://github.com/eightmm/oh-my-setting/commit/dfdd7fe001d0bb8aadf77d9c48e2fa55908db4d8).

신규 수정은 처음 탐색할 때 같은 session ID가 두 project folder에 있으면 읽지 않는다.
하지만 `_transcripts[consumer]`가 이미 있으면 탐색 자체를 하지 않는다.
새 fixture는 중복 파일을 만든 뒤 캐시를 명시적으로 지워 cold 경로만 확인한다.

최소 재현은 실제 임시 project folder로 수행했다. unique log를 한 번 찾아 캐시한 뒤
다른 folder에 같은 ID의 log를 추가하면 기존 경로를 그대로 반환한다.
캐시를 지운 뒤에는 같은 디스크 상태에서 None을 반환한다.
Native index 자체는 고정된 enrolled ID를 반환하는 stub을 사용했다.

**권장 수정:** 동일한 bounded discovery snapshot에서 uniqueness와 cache validity를
함께 확인하거나 folder/index generation 변화로 무효화한다. 매 행마다 전체 HOME를
다시 훑는 수정은 피하고 cap 때문에 탐색이 불완전하면 unique라고 단정하지 않는다.
따뜻한 캐시 뒤 중복 생성/제거, 파일 교체, 계정/home 변경, 부분 탐색을 기존 NI fixture에 넣는다.
이 재현은 읽기 대상 선택의 불일치이며 credential 유출이나 권한 탈취를 입증한 것이 아니다.

### R5 · 높음: 최신 토론 라운드가 non-answer이면 이전 성공 라운드를 최신처럼 선택

근거: [panel_debate._seats](https://github.com/eightmm/oh-my-setting/blob/e055c4d0ca3ccd8b5c127b70dd85cd959a0b6282/scripts/lib/panel_debate.py#L129-L149),
[tab_body 선택/요약](https://github.com/eightmm/oh-my-setting/blob/e055c4d0ca3ccd8b5c127b70dd85cd959a0b6282/scripts/lib/panel_debate.py#L245-L294).

Round 1에 정상 답변, round 2에 `answer_quality=blocked, exit=0`가 있으면
`_seats()`는 `rounds={1: answer}`, `quality={2: blocked}`를 만든다.
하지만 상세의 latest 식은 rounds가 하나라도 있으면 max(rounds)만 사용해서 **1**을 고른다.
목록도 `not rounds and quality`일 때만 non-answer를 표시하므로 이전 답변이 남아 있으면
최근 실패를 알리지 않는다. Header의 rounds/answered 집계도 같은 누락을 점검해야 한다.

현재 `_seats` 함수와 latest 표현식을 실행해 `valid -> blocked`는 1,
`blocked -> valid`는 2가 선택됨을 확인했다. 전체 터미널 렌더링을 실행한 것은 아니다.

**권장 수정:** 관측된 최신 라운드와 마지막 사용 가능한 답변 라운드를 분리한다.
최신은 rounds와 quality의 key 합집합에서 정하고, 과거 답변은
“이전 유효 답변, round 1”처럼 명시적으로만 연다. 같은 round의 repair 성공 여부는
artifact 순서/provenance로 판단하고, unrelated seat/model의 결과를 덮어쓰지 않는다.
기존 RB fixture에 valid→blocked/truncated/empty, non-answer→valid,
same-round repair 성공/실패, 일부 seat만 새 round인 사례를 추가한다.
실제 review gate가 우회됐다는 재현은 아니며 현재 결과를 잘못 이해하게 하는 UI 결함이다.

### 확인한 개선을 되돌릴 이유가 없는 부분

`ExitStack`을 이용한 fixture 단순화는 유지한다. 새 AST nesting 검사는 빠른 보조
검사로 유용하지만 실제 Python 3.11 실행 검사를 제거할 근거가 아니다.
[Python 공식 AST 문서](https://docs.python.org/3.11/library/ast.html#ast.parse)도
`feature_version`의 파싱이 해당 버전과 동일하다고 보장하지 않는다.
이 환경에는 Python 3.13.5만 있어 native 3.11 compile을 실행하지 않았다.
별도 실패 재현 없이 이 부분을 확정된 런타임 결함으로 분류하지 않는다.

## 3. 구현과 의견이 다른 지점, 그리고 내 이전 제안의 수정

다음은 정답/오류 판정과 구별되는 설계 제안이다. 게시된 코드가 표현한 선택을
비교한 것이며 Claude가 사적으로 어떤 주장을 했는지는 확인하지 못했다.

| 쟁점 | 현재 구현 또는 이전 제안 | 이번 권고 | 성격 |
|---|---|---|---|
| 안전한 재개 | 상태 저장소와 실행 저장소의 common-dir를 비교 | 상태 authority와 실행 provenance를 분리해 모두 검증 | R1의 기능 결함과 연결 |
| 실패 설명 | token limit 이외는 stream_truncated | 확인된 원인만 구체화하고 unknown 보존 | R2의 진단 정확도 |
| 세션 캐시 | 최초 unique이면 경로를 계속 재사용 | cached와 currently unambiguous를 분리해 무효화 조건 검증 | R4의 identity 일관성 |
| 최신 답변 | 정상 답변이 있는 최신 round를 먼저 선택 | 최신 시도 상태와 마지막 유효 답변을 별도 표시 | R5의 UX 정확도 |
| 기본 Work 화면 | Main/role 관계를 graph/tree로 표시 | Graph를 유지하되 task·다음 주체 중심 목록과 비교 실험 | 사용성 가설, 일방 교체 아님 |
| 구현 단위 | CP/UX/NF의 120개 조건이 문서에 누적 | 조건은 추적표로 보존하고 실제 PR은 작은 사용자 여정 하나씩 | 내 이전 계획의 규모 조정 |
| Return Brief bookmark | #15에서 첫 버전 memory-only 제안 | 같은 세션 MVP는 유지하되 재시작 복귀가 필요하면 최소 cursor의 명시적 로컬 저장 검토 | 내 제안 보완, 아직 승인/구현 아님 |
| 캐시/상태 | 두 번째 authoritative store를 금지 | authority 금지와 파생 UI cache 금지를 혼동하지 않음. 재생성·삭제 가능한 cache는 계측 후 판단 | 내 표현 명료화 |

Return Brief 저장 후보는 repo/room lineage, schema, bounded event cursor 정도다.
Transcript·prompt·credential을 저장하지 않고 bookmark가 approval/acceptance를 만들지 않는다.
Rebuild/retention gap에서는 “기준 불완전”을 표시한다. 이 작은 저장도 새 동작이므로
사용자 범위·privacy·retention 검토 없이 기본 활성화하지 않는다.

UI 우선순위는 `Why Blocked + Action Inbox` 한 경로를 먼저 완성하는 쪽으로 좁힌다.
현재 managed records를 이용해 task → 원인 → 담당 → 근거 이동을 검사한다.
그다음 Task Inspector의 diff/evidence 왕복, 이후 Return Brief 순이다.
Research Compare는 비교할 artifact 계약이 준비된 뒤 별도 진행한다.
새 웹/SSH/controller나 글로벌 native discovery를 첫 UX 개선의 전제조건으로 만들지 않는다.

## 4. 병행 작업자에게 넘길 실행 순서

1. **R4/R5 경계 수정:** 방금 추가된 ambiguity/non-answer 기능의 warm-cache와 여러 라운드
   fixture부터 확장한다. helper 통과만이 아니라 board/result 표시와 함께 검증한다.
2. **R1 수정 PR:** state/source 분리 fixture를 먼저 만들고 정확한 source identity를 검증한다.
   새 CLI/schema가 필요하면 legacy 거부 조건을 함께 설계한다. 단순 guard 삭제는 하지 않는다.
3. **R2 수정 PR + R3 문서 정리:** 분류와 display truncation을 분리하고 unknown/fallback 경계를
   보존한다. R3는 별도 작은 문서 변경으로 분리해도 된다.
4. **첫 UX PR:** Why Blocked + Action Inbox만 구현한다. 대상 task가 바뀌지 않고,
   사용자에게 필요한 결정과 Main 처리를 구분하며, 근거로 돌아갈 수 있게 한다.

리뷰 요청에는 base/head SHA, 수정 범위, 실제 실행한 case, 미검증 범위,
남은 판단을 적는다. 별도 모델 reviewer를 사용했다면 실제 기록으로 확인되는
harness/model provenance만 기재하며 GitHub 작성자 이름을 모델 독립성 증거로 쓰지 않는다.

다른 작업자의 branch를 force-push하거나 같은 파일에 경쟁 patch를 올리지 않는다.
후속 구현이 올라오면 이 리뷰 기준 SHA와 새 head의 diff를 다시 대조한 뒤 R1–R5를 닫는다.
댓글이나 문서만으로 완료 체크를 하지 않고 최종 merge/install은 기존 권한과 검증을 따른다.
이 문서는 상시 모니터링이나 Claude 세션으로의 직접 전달을 뜻하지 않는다.

## 5. 재현 결과와 검증 범위

최소 재현 12개를 실행했다: worktree 관계 2개, Antigravity 오류 매핑 5개,
세션 cache 선택 3개, 토론 round 선택 2개. 앞의 7개는 ff3d8e3의 해당 함수,
뒤의 5개는 e055c4d의 해당 함수/표시 표현식으로 실행했다.
함수 범위는 `panel_dispatch.py`의 `_git_path`/`_recorded_execution_repository`와
`peer-conversation.py`의 `ProviderTurnError`/`_bounded`/`agy_turn_error`다.
Git은 실제 임시 저장소와 worktree를 만들었고 모델·network 호출은 없었다.
추가 재현은 `_main_transcript`, `_seats`, `nonanswer_quality`와 tab_body의 latest 표현식이다.
Index/provider/문자열 표시 helper는 synthetic stub으로 대체했다.
이 결과는 R1/R2/R4/R5의 특정 경로 재현이며 수정 후 회귀 테스트 통과가 아니다.

후속 구현자는 위 source 함수들을 현재 checkout에서 직접 로드하고 다음 입력을 재현한다.
전체 production 모듈을 임의 실행하는 대신 기존 fixture 경로로 옮겨 검증한다.

```text
state = independent git repository
source = independent git repository
worker = detached worktree of source
_recorded_execution_repository(source, worker) -> accepted (control)
_recorded_execution_repository(state, worker)  -> rejected (R1)

agy_turn_error(status=ERROR, error="something else broke") -> stream_truncated
agy_turn_error(status=ERROR, error="authentication failed") -> stream_truncated
agy_turn_error(status=ERROR, error="x"*241 + " output token limit") -> stream_truncated

_main_transcript(unique) -> one, caches one
_main_transcript(after duplicate created) -> one (warm cache)
_main_transcript(after cache clear) -> None (same disk, ambiguity detected)

_seats(round1 valid, round2 blocked) -> rounds={1}, quality={2}
current latest expression -> 1 (expected latest observed round: 2)
```

컨테이너에서 GitHub DNS 조회가 해결되지 않아 전체 checkout, canonical
`scripts/check.sh --affected`, `oms land`를 실행하지 못했다.
문서 게시에는 GitHub connector를 사용하며 runtime 코드는 바꾸지 않는다.
PR의 문서 CI가 성공하더라도 native provider/TUI/전체 integration 검증으로 표현하지 않는다.
