# OMS 기능 확장: 추가 비교, 구현, 후속 작업

검토일: 2026-11-09. OMS 비교 기준 `e055c4d`, 게시 기준 `012607c`.
[기존 계획](CONTROL-PLANE-EVOLUTION.md), [UX/NF 설계](CONTROL-PLANE-UX-AND-FEATURES.md),
[기존 비교와 정정](CONTROL-PLANE-REFERENCES.md)을 대체하지 않는다.
이번에는 패널 디자인보다 **무슨 정보를 재사용하고 어떤 동작을 보장하는가**를 비교했다.

## 이번에 실제로 구현한 것

[PR #22](https://github.com/eightmm/oh-my-setting/pull/22)는 저장된 context pack의
byte budget/debt/omission/중복 source 후보를 검사하고 두 pack을 비교하는 구현이다.
모델·원본 source·bundle을 읽지 않고 기존 ContextManifest만 읽는다.
코드와 15개 모듈 테스트, 기존 context smoke의 실제 CLI 통합 테스트를 함께 제출했다.
이는 NF-03의 CLI 기반 첫 조각이며 panel 전체, 현재 source freshness 검증 또는
실험 결과의 통계적 비교 완료를 뜻하지 않는다. 병합/설치는 하지 않았다.

로컬 모듈 테스트는 Python 3.13.5에서 실행했다. Parser는 실제 코드를 사용하되
미수집 dependency의 상수를 stub한 제한 검증이다. GitHub DNS 실패로 전체 checkout과
canonical suite/실제 runtime CLI를 로컬에서 실행하지 못했다. 등록한 CLI 통합 검사는
정확한 PR head의 CI 결과로 확인해야 하며 모듈 성공과 구분한다.

## 1. 추가 비교의 근거와 채택 판단

공식 README/문서와 OMS의 관련 source·public front door를 대조했다.
외부 제품 설치·테스트·성능·운영 안전성 검증은 하지 않았다. 외부 코드를 복사하지 않았고
dependency도 추가하지 않았다. 아래 URL은 이동하는 기본 브랜치/문서다.
코드를 재사용하는 후속 작업은 exact commit와 LICENSE/NOTICE를 먼저 고정한다.
제품의 홍보성 절감률·성능 수치는 OMS 목표나 실측 근거로 쓰지 않는다.

| 1차 출처 | 문서에서 확인한 핵심 | OMS에 이미 있는 기반 | 추가할 가치 / 가져오지 않을 동작 |
|---|---|---|---|
| [Entire CLI](https://github.com/entireio/cli) | Agent session을 Git commit과 연결해 검색 가능한 개발 이력으로 남김 | checkpoint, artifact-index, attempt/landing receipts | 변경→작업→근거의 탐색. 전체 transcript를 Git에 넣거나 기본 push하는 흐름은 도입하지 않음 |
| [RTK](https://github.com/rtk-ai/rtk/tree/develop), [README](https://github.com/rtk-ai/rtk/blob/develop/README.md) | 모델에 전달되는 shell 출력의 축약·필터링, 실패와 개수 중심 표현 | bounded context compiler, 로그 캡처/요약 | 먼저 입력 크기/누락을 계측. 이후 raw 근거를 보존한 선택형 출력 축약. 검증 입력 변경·API proxy/인증 재배선은 제외 |
| [Emdash](https://github.com/generalaction/emdash), [worktree workflow](https://github.com/generalaction/emdash/blob/main/agents/workflows/worktrees.md) | task별 worktree, issue→agent, diff/PR/CI, local/SSH | scratch-worktree, dispatch, scope, land, remote target metadata | workspace readiness와 출처 있는 task intake. 새 IDE·자동 credentials 복사·새 scheduler는 제외 |
| [Toad](https://github.com/batrachianai/toad) | 여러 agent를 연결하는 terminal/ACP client | native CLI pane, provider registry, capability 검사 | 지원 capability와 미지원 이유를 명시하는 adapter 계약만 참고. native 대화를 새 UI로 재구현하지 않음 |
| [Agent Task Handoff](https://github.com/zed601/agent-task-handoff) | 전체 chat 대신 unfinished task·결정·시도·다음 단계 인계 | session-handoff, journal, capsule export/diff | 최신성/누락/계약 차이를 보여주는 인계 preview. 기존 exporter를 또 만들지 않음 |
| [Flightdeck](https://github.com/flightdeckhq/flightdeck) | agent observability와 control, provider별 관측/집행 경계 | usage, quota, attempt identity, authority gates | 관측 가능한 budget과 실제 중단 권한을 구별. 관측 행을 자동 kill하는 watchdog은 제외 |
| [GitHub Unified Agent Session Spec](https://github.github.com/gh-aw/specs/unified-agent-session-specification/) | 여러 source의 event/usage/diagnostic을 정규화하며 source별 가용성을 구분 | lifecycle/artifact events, provider parser | missing/unknown을 보존한 작은 공통 event 어휘. 전체 규격 채택·raw session 상시 저장은 제외 |

Entire/RTK/Emdash는 기록·context·workspace의 다른 문제를 푼다. 하나를 OMS 대체품으로
통째로 설치하지 않는다. Toad와 RTK는 라이선스 구성이 단순한 MIT라는 가정을 하지 않는다.
이번 독립 구현은 외부 코드/asset 복사를 포함하지 않으며 법률적 재사용 허가를 주장하지 않는다.

## 2. 실제 구현과 후속 작업을 구분한 작업표

아래 F-ID는 이번 비교의 좁은 실행 묶음이다. 새로운 framework 8개나 전부 동시에
구현할 계획이 아니다. 기존 CP/UX/NF 번호에 연결해 중복된 상태 저장소를 만들지 않는다.

| ID | 우선 | 작업 | 기존 연결 | 현재 상태 |
|---|---|---|---|---|
| F1 | P0 | Context pack inspect/compare | NF-03, CP-08 | 코드/테스트 제출: #22, 전체 panel 미구현 |
| F2 | P1 | 변경의 출처와 근거를 거슬러 찾기 | CP-10, NF-06 | 후속 구현 |
| F3 | P1 | 인계 자료의 최신성·누락 preview | NF-01/03, CP-09 | 후속 구현 |
| F4 | P1 | 손실을 표시하는 선택형 tool-output digest | CP-04/08 | 비교 fixture 먼저, 실제 치환 기본 off |
| F5 | P1 | Provider event의 unknown-preserving 정규화 | CP-02/03/08 | 기존 parser에서 작은 범위부터 |
| F6 | P2 | Workspace readiness·복구 preflight | NF-05, CP-09 | 기존 dry-run 확장 |
| F7 | P2 | 관측 budget과 enforcement capability 구분 | CP-08 | advisory 먼저; 자동 중단 보류 |
| F8 | P2 | 외부 issue를 검토 가능한 task 초안으로 변환 | CP-01, 기존 PROJECT/plan | 명시 source의 dry-run 초안부터 |

### F1 — Context inspector/compare

첫 command는 `oms runtime context-inspect MANIFEST [--compare OTHER]`다.
원본 데이터를 다시 읽지 않고 기록된 budget/debt/digest를 요약한다.
동일 source SHA도 서로 다른 부분을 선택했을 수 있으므로 절감량으로 환산하지 않는다.

후속 panel 연결의 완료 기준: exact artifact가 선택됨, 기록 시점과 불명 상태 노출,
bounded 화면에서 상세 이동, observer 데이터가 acceptance로 승격되지 않음.
명시 manifest 식별과 기존 result reader를 재사용하고 새 캐시/DB는 필요성을 측정한 뒤 판단한다.

### F2 — Why this change: source → attempt → review/evidence

최소 소비자는 “이 변경은 어떤 작업과 검증을 거쳐 들어왔나?”다.
입력은 선택된 commit/patch/artifact digest, 출력은 이미 기록된 연결의 bounded graph다.
Git author 이름으로 작성 모델을 추정하거나 유실된 관계를 LLM으로 보충하지 않는다.

수락 조건: 관계 없음과 조회 실패 구별, same-path 다른 task generation 혼동 금지,
rewrite/삭제/foreign receipt는 미확인 표시, read-only·no transcript export 보장.
기존 artifact-index/landing/lineage fixture를 확장한다. 자동 Git commit annotation이나
checkpoint branch push는 이 작업에 포함하지 않는다.

### F3 — Handoff freshness preview

새 handoff 포맷 대신 기존 `session-handoff`/capsule의 기록과 현재 계약의 차이를
보여준다. 재개를 실행하지 않고 이전 목표·필요 자료·현재 달라진 조건·누락을 설명한다.

수락 조건: plan/task lineage 교체, 같은 HEAD의 미커밋 변경, missing artifact,
retention gap을 검사한다. 과거 verified가 현재 verified로 승계되지 않아야 한다.
Provider native history를 복사하거나 다른 모델의 hidden reasoning을 전송하지 않는다.
새 cursor 영속 저장이 필요하면 UI cache/retention 검토를 별도 변경으로 분리한다.

### F4 — Loss-aware output digest

실제 출력 축약 전에 synthetic test/lint/build 로그에서 byte 수와 오류 보존을 비교한다.
첫 구현은 raw capture에 붙는 선택형 파생 digest다. 원문 digest/ref, 읽은 범위,
생략량, exit code, 실패 목록을 보존한다. 원문을 덮어쓰지 않는다.

수락 조건: 정상/실패/ANSI/한글/매우 긴 한 줄/partial tail에서 중요한 실패와 exit
code가 유지되고, parser가 모르면 raw bounded fallback을 쓴다. Verifier의 입력·검증
기록은 digest로 대체하지 않는다. 축약률은 실제 bytes와 명시한 tokenizer 추정으로
구분하고 구독 quota나 청구 절감률로 바꾸지 않는다. API proxy·모델 라우팅 변경은 없다.

### F5 — Normalized events with provenance

기존 provider parser의 일부 event를 공통 read model로 정리한다.
requested/served model, tool start/result, observed timestamp, source parser version,
usage의 cumulative/delta 범위를 분리한다. 공급자가 보고하지 않은 값은 unknown이다.

수락 조건: duplicate/out-of-order/partial event, tool 결과 누락, cumulative token
재보고, unknown error를 검사한다. Parse 성공과 작업 성공은 다르다. 오류 reason은
원문을 제한한 뒤 의미를 분류하고 display truncation 때문에 원인이 바뀌지 않아야 한다.
#16의 R2와 충돌하는 경쟁 parser를 만들지 말고 그 수정을 선행/재사용한다.

### F6 — Workspace readiness preflight

기존 dispatch dry-run이 선택한 repo/base/worktree/권한/검증 명령을 한 번에 설명한다.
새 worktree를 만들지 않고 준비 상태와 필요한 조치를 구분하며 source/state repo
분리를 유지한다. 설정 파일 복사는 명시적으로 승인한 목록만 고려한다.

수락 조건: dirty source, 사라진 base, 동시 생성, 다른 common-dir/clone, stale lease,
미지원 backend를 검사한다. preview와 실행 사이 identity/HEAD가 바뀌면 실행 경로에서
재검증한다. #16 R1과 기존 source provenance 검사를 우회하지 않는다.

### F7 — Budget visibility before control

먼저 reported token/cost/quota·freshness·집계 범위를 읽기 전용으로 보여준다.
관측만 되는 provider와 실행을 실제로 제어할 수 있는 provider를 구분한다.
기본 budget 때문에 자동 kill하거나 구독에서 paid API로 전환하지 않는다.

수락 조건: 가격 미상은 0이 아님, parent/child 중복 집계 방지, offline/stale quota 표시,
승인 없는 route/stop 변경 없음. 실행 중 hard-stop은 실제 native capability·owned
process group·수동 승인·partial artifact 보존 계약이 준비된 별도 PR에서만 검토한다.

### F8 — Issue-to-contract draft

사용자가 지정한 issue/문서의 특정 revision을 bounded input으로 받고,
목표·scope·수락 조건·미정 질문을 기존 PROJECT/plan의 **초안**으로 표현한다.
외부 문서의 명령문은 입력 데이터이며 권한이 아니다. 자동 claim/dispatch는 없다.

수락 조건: source revision 변경, 중복 intake, 링크의 prompt injection,
존재하지 않는 repo/권한 scope를 거부/표시한다. 초안 미리보기는 state를 쓰지 않고
실제 등록은 기존 parent writer·review/CAS를 재사용한다. connector 자동 설치·새 tracker
서비스·백그라운드 polling은 도입하지 않는다.

## 3. 병행 구현과 다음 PR 분할

이번 작업 시작 시 Claude Code 표기 #18–#21이 열려 있었고, 게시 기준까지의 main
변경에는 #16 리뷰 문서와 #17 비교 보강, BEL/대기 이유/tmux counts/tick fixture가
반영됐다. `e055c4d..012607c` 변경 목록을 대조해 F1의 파일이 겹치지 않음을 확인했다.
#17의 tmux 옵션/needs/heartbeat 비교를 다시 새 작업으로 포장하지 않는다.

권장 다음 순서는 **#22 실제 runtime 통합 검증 → F1 reader 연결 → F2 또는 F3 한
사용자 여정 → F4 synthetic 계측 → 나머지 capability별 좁은 구현**이다.
각 실제 PR은 범위, base/head, 기존 helper, 정상·실패·권한 fixture와 rollback을 적는다.
서로의 branch를 수정하거나 같은 기능을 별도 engine으로 구현하지 않는다.

이 문서의 병합은 후속 기능 구현 완료가 아니다. 기존 120개 조건을 또 일괄 완료 처리하지
않으며, 실제 구현 PR/검증 근거가 연결된 조각만 완료 기록한다. 연구/GPU/Slurm·유료
provider 실행·사용자 설치·자동 merge는 이번 비교와 F1 구현에 필요하지 않다.
