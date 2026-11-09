# OMS 패널·관측·제어 확장 작업계획

작성일: 2026-11-09 (KST)
기준: [`20ce1be`](https://github.com/eightmm/oh-my-setting/commit/20ce1be31fb5175f4d431d9e98f53557a3219a0b), v0.7.0
상태: **제안 / 문서 전용. 아래 구현 항목은 모두 미완료이며, 이 문서의 병합은 기능 구현·설치·권한 확대 승인이 아니다.**

## 1. 목표와 경계

현재 OMS의 단일 Main, native CLI 대화 pane, 로컬 상태, 격리 worktree,
독립 리뷰, evidence freshness 및 verified landing을 유지한다.
외부 프로젝트를 통째로 채택하는 대신, 실제 실행 세션을 더 정확하게 관찰하고
사용자가 Attention → 상세 → 다음 조치로 이동하는 비용을 줄인다.

이번 PR의 산출물은 현황 대조, 작업 명세, 선행 관계, 수락 조건,
테스트·계측 계획, 후속 PR 분할, 참고 프로젝트의 채택/보류 판단이다.
런타임 코드, 공개 CLI, 기본 설치 설정, provider 권한 및 `.oms` 데이터는 변경하지 않는다.
기존 [안정화 계획](STABILIZATION-PLAN.md)의 S1–S6와 PR #11–#13은 완료된 선행 작업이다.
이번 목록으로 다시 미완료 처리하지 않는다.

### 현황 정정: native 관측이 전혀 없는 것은 아니다

`oms dashboard`는 의도적으로 OMS-managed 기록만 보여준다. 그러나 panel에는 이미
Claude/Codex native subagent와 사용량을 제한적으로 읽는 경로가 있다.
따라서 “native discovery를 처음부터 새로 구현”하는 계획은 부정확하다.
**기존 읽기 경로의 범위·identity·신선도·부분 실패를 명시하고 확장하는 것이 출발점이다.**

| 영역 | 기준 코드/문서에서 확인한 현재 기능 | 남은 작업의 성격 |
|---|---|---|
| Native 관측 | `panel_chats.py`: `native_index`, `_read_owned`, `native_advisors`, `_codex_native_agents`, `native_subagents`; 등록된 Main과 연결된 메타데이터/사용량 읽기 | 기존 adapter 재사용; 외부 세션 범위 확대는 opt-in |
| Native 상태의 의미 | `room_view.py`: native 행은 room participant가 아니며 최근 파일 기록을 lifecycle 증거로 취급하지 않음 | 관측 신호와 실제 liveness의 구분을 일관되게 노출 |
| 대시보드 | `dashboard_projection.py`의 managed-only coverage, null/unknown 및 collection 실패 표시 | panel과 dashboard의 범위를 구분; coverage 문구를 단순 삭제하지 않음 |
| 패널 | Graph/Tree, Attention, Messages, Plan, 결과 reader, native chat navigation | 없는 화면을 새로 만들기보다 검색·필터·연결·조작 일관성 개선 |
| 사용량 | `panel_metrics.py`, `panel_chats.py`의 native 사용량, 증분 캐시·읽기 상한 | 집계 범위·중복·부분값·신선도 및 진단 UX 강화 |
| 복구 | Main restart/swap, stopped worker settle/salvage, room lineage | 기존 경로 재사용; Pi/Agy resume은 ownership 확인 전 계속 거부 |
| 검증·완료 | Typed runtime, criterion evidence, task/plan lineage, 독립 리뷰, admission/landing | 관측 데이터가 검증·승인으로 승격되지 않도록 회귀 보호 |
| 터미널 호스트 | tmux/inline 및 별도 선택형 Herdr host 관련 계약 | 기존 adapter의 기능 차이를 점검; 제거된 pane-control 계층 부활 금지 |
| 연구·HPC | 기존 runtime backend 및 job-digest 등 제한된 관찰 도구 | Job/attempt/실험 근거의 읽기 전용 연결; 새 scheduler 금지 |

주요 확인 경로: [panel_chats.py](../scripts/lib/panel_chats.py),
[room_view.py](../scripts/lib/room_view.py), [panel_state.py](../scripts/lib/panel_state.py),
[dashboard_projection.py](../scripts/lib/dashboard_projection.py),
[패널 계약](TERMINAL-PANEL.md), [대시보드 계약](DASHBOARD.md), [runtime 계약](OMS-RUNTIME.md).
이는 해당 코드·문서의 정적 확인이며 전체 기능을 실사용 검증했다는 뜻은 아니다.

## 2. 보존할 설계 원칙

1. **권위 있는 상태는 하나다.** 기존 task/plan/room/receipt writer를 유지한다.
   별도 authoritative DB, 상주 orchestrator, 새 작업 큐, 병렬 Main을 만들지 않는다.
2. **관측은 권한이 아니다.** `observed`, 프로세스 종료, 모델의 완료 발언,
   토큰 사용량, 화면의 green 상태는 acceptance·review·landing 증거가 아니다.
3. **기본 범위를 넓히지 않는다.** 현재 명시적으로 연결된 세션 읽기는 유지하되,
   다른 프로젝트·호스트·전체 HOME의 발견이나 transcript 상세 접근은 별도 opt-in이다.
4. **Native tool loop를 복제하지 않는다.** Harness, inference provider/model,
   session owner, reviewer의 실제 모델 계열을 구분한다.
5. **작업을 멈췄다고 지우지 않는다.** Dirty worktree·중단된 로그·검토 전 patch를 보존한다.
   시간 초과·연결 실패는 restart, kill, cleanup, lease reclaim의 허가가 아니다.
6. **비용과 개인정보를 추가하지 않는다.** 기본 경로에 유료 모델 호출, 외부 connector,
   raw transcript의 Journal/프롬프트/원격 전송, 상시 polling을 추가하지 않는다.
7. **기존 계약을 몰래 바꾸지 않는다.** 공개 CLI, schema, 설치/default, dependency,
   provider-home 접근 및 네트워크 노출 변경은 별도 계약 검토와 승인 대상으로 남긴다.

## 3. 제안하는 최소 구조

```text
existing native hooks / bound session metadata / terminal identity
                         |
          bounded, provider-specific observation adapters
                         |
              disposable in-memory read projection
                         |
existing .oms records ----+----> terminal panel / dashboard
                         |              (separate coverage labels)
                         +----> optional read-only SSH / web views

all mutations -> existing OMS parent front doors -> unchanged gates/receipts
```

Projection의 필드 이름은 **설계안**이며 새로운 public schema가 아니다.
최소 정보는 provider/harness, opaque session reference, host/account scope,
관측 출처, 관측 시각과 만료 여부, 수집 오류, `managed/observed` 구분,
명시적으로 확인된 parent/attempt 관계, 상태 신호의 강도 및 사용량 집계 범위다.
자연어 요약으로 identity를 만들거나 PID/cwd/mtime만으로 관계를 확정하지 않는다.

상태의 최소 구분은 `unknown`, `recent_activity`, `running_confirmed`,
`waiting_input`, `waiting_permission`, `terminal_confirmed`, `unreachable`다.
이는 기존 attempt state를 대체하지 않는다. 파일이 최근 수정됐다는 사실만으로
`running_confirmed`, transcript가 없다는 이유만으로 `terminal_confirmed`를 만들지 않는다.

## 4. 작업 목록과 우선순위

P0/P1은 로컬 MVP, P2는 독립적인 선택형 확장, P3는 보류된 권한 확장이다.
우선순위는 보안 심각도가 아니라 구현 순서다. 모두 미구현 계획이며 CP-13은 보류다.
CP-16/17은 별도 거대 테스트·문서 PR이 아니라 각 구현 PR에 함께 적용한다.

| ID | 우선 | 작업 | 선행 조건 | 기본 범위 |
|---|---|---|---|---|
| CP-01 | P0 | 현황·capability·coverage 계약 확정 | 없음 | 기존 구현 정리 |
| CP-02 | P0 | 세션 identity, liveness, managed/observed 경계 | CP-01 | 기존 guard 강화 |
| CP-03 | P0 | 안전한 provider 관측 확장 | CP-02, CP-18 | 신규 발견은 opt-in |
| CP-04 | P0 | 증분 수집·파서·캐시·부하 상한 | CP-03 | 기존 collector 재사용 |
| CP-05 | P0 | 일관된 projection과 중복 제거 | CP-02, CP-04 | 기존 JSON 호환 |
| CP-06 | P1 | 검색·필터·Graph/Tree·키보드 UX | CP-05 | 로컬 패널 |
| CP-07 | P1 | Attention·알림·이벤트 대기 | CP-05, CP-06 | 외부 알림 기본 없음 |
| CP-08 | P1 | Timeline·토큰·quota·진단 관측 | CP-04, CP-05 | 추정/측정 분리 |
| CP-09 | P1 | 재개·복구·worktree 수명주기 | CP-02, CP-05 | 소유권 guard 유지 |
| CP-10 | P1 | 결과·리뷰·evidence 탐색 | CP-05, CP-06 | 기존 gates 유지 |
| CP-11 | P2 | 다중 호스트 읽기 전용 관측 | CP-02, CP-05, CP-16 | 별도 opt-in/승인 |
| CP-12 | P2 | 로컬 읽기 전용 웹 뷰 | CP-05, CP-06, CP-16 | 별도 ADR/승인 |
| CP-13 | P3 | 원격·모바일 typed control | CP-09, CP-11, CP-12 | **기본 보류** |
| CP-14 | P2 | 연구/HPC job–attempt 연결 | CP-05 | 읽기 전용 |
| CP-15 | P2 | 터미널 backend capability parity | CP-02, CP-06, CP-09 | 선택형 Herdr 유지 |
| CP-16 | P0 | 회귀 시나리오·부하·계측 기준 | CP-01 | 각 PR에 포함 |
| CP-17 | P1 | 이행·rollback·문서·설치 증빙 | CP-01, 해당 구현 | 각 PR에 포함 |
| CP-18 | P0 | 참고 구현·라이선스·채택 기록 | 없음 | 외부 코드 도입 전 |

### CP-01 — 현황·capability·coverage 계약

`docs/TERMINAL-PANEL.md`, `docs/DASHBOARD.md`, provider registry와 기존 테스트를
대조한다. 기능을 `implemented`, `partial`, `unsupported`, `unknown`으로 나누고,
문서 확인·fixture 검증·native 실행 검증을 별도 칸으로 둔다.

- [x] Claude/Codex/Pi/Agy별 launch, observe, subagent, usage, attach, resume, stop 지원 근거를 기록한다.
- [x] dashboard의 managed-only와 panel의 native 읽기 범위를 별도로 명시한다.
- [x] 등록 Main, 연결된 subagent, 외부 세션, 다른 repo/host의 기본 visibility를 확정한다.
- [ ] 기존 S1–S6와 이미 있는 UI/collector를 신규 구현 항목으로 중복 등록하지 않는다.

### CP-02 — identity·liveness·권한 경계

수정 후보: `panel_chats.py`, `panel_state.py`, `room_view.py`와 기존 process/room identity helper.
Identity는 provider/account/host/session namespace를 포함한다. Live control에는
session binding과 가능한 process generation을 재확인하고, 모호하면 거부한다.

- [ ] 같은 cwd, session label 또는 PID를 재사용한 다른 세션이 합쳐지지 않는다.
- [ ] mtime 기반 최근 활동과 실제 liveness, disconnected와 ended를 구분한다.
- [x] native/observed 행 추가가 room participant, task owner, lease 또는 acceptance를 만들지 않는다.
- [ ] 탐색 후 실행 사이 target 교체, room rollover, session-ID 충돌 시 잘못된 pane에 입력하지 않는다.

### CP-03 — provider 관측 확장

`native_index`, `native_subagents`, `_read_owned`, `hook_state.py`,
`pi-oms-extension.mjs`를 먼저 재사용한다. Claude/Codex 연결 세션의 parity부터 맞추고,
Pi/Agy는 실제 native 계약으로 확인되는 신호만 추가한다. 전체 머신 발견은 별도 opt-in이다.

- [ ] Opt-in이 꺼져 있으면 기존 repo/room 범위와 provider-home 접근 범위가 넓어지지 않는다.
- [ ] read-only 관측이 native 설정·기록·인증 파일·provider 실행 상태를 변경하지 않는다.
- [x] 다른 UID, symlink/hardlink, 특수 파일, path traversal, 읽는 동안 교체된 파일을 안전하게 거부한다.
- [ ] 중단·손상·미지원 포맷은 source별 degraded/unsupported로 표시하며 다른 provider 관측은 계속된다.

### CP-04 — 증분 파서·공유 수집·부하 상한

기존 offset/inode 캐시와 읽기 상한을 확장한다. UI 수만큼 같은 native log를
다시 스캔하지 않는다. 메모리 내 파서 상태를 기본으로 하며, 영속 캐시는
별도 근거·privacy/retention 계약 없이는 추가하지 않는다.

- [x] Append, partial line, truncate, rotate, inode 교체, 매우 긴 JSONL 줄, 알 수 없는 event를 검사한다.
- [ ] 파일·줄·전체 refresh byte 수, entry 수와 wall-time 예산이 명시되고 starvation 없이 동작한다.
- [x] 동일 event 재수집·재시작으로 token/child 수가 중복 증가하지 않는다.
- [ ] 변화 없는 warm refresh의 파일 읽기·subprocess 수와 RSS를 측정하고 baseline 대비 회귀를 공개한다.

### CP-05 — projection 통합과 호환성

`panel_state.py`, `room_view.py`, `dashboard_projection.py`를 확장하되,
공유 collector와 표면별 coverage를 분리한다. 한 세션이 여러 출처에 등장하면
identity 증거로만 연결하고, 관측 누락을 성공이나 0으로 보정하지 않는다.

- [ ] Managed 행과 연결된 observed 행의 중복 표시·token 이중 계산이 없다.
- [ ] Dashboard 기존 schema-1 필드의 타입·null 의미·exit status를 유지한다.
- [ ] Source 실패·truncation·staleness·omitted 수가 화면과 기계 출력 모두에 남는다.
- [ ] 관측 전후 `.oms` authority와 approval/receipt bytes가 동일하고 native 결과는 검증 완료 수에 들어가지 않는다.

### CP-06 — 패널 탐색 UX

`panel_view.py`, `room_view.py`, 기존 board/reader를 재사용한다.
Repo/role/provider/status/source별 검색·필터, 선택 유지, graph dependency와 결과 연결을
개선한다. 별도 전역 프로젝트 manager나 새 task DAG는 만들지 않는다.

- [ ] Graph/Tree/Attention/Results 사이 전환 후 동일 task/attempt 선택과 scroll 위치가 유지된다.
- [ ] 검색·필터 해제 시 항목이 사라진 것으로 오인되지 않으며 숨겨진 수를 표시한다.
- [ ] 키보드만으로 주요 흐름이 가능하고 F5/F9/F12 등 기존 key contract와 충돌하지 않는다.
- [ ] 좁은 터미널, resize, 긴 한글/CJK, 색상 미지원, mouse/keyboard, terminal escape 입력 fixture를 통과한다.

### CP-07 — Attention·알림·이벤트 대기

기존 inbox를 재사용해 승인 대기, 실패, 장기 정체, 연결 손실을 구별한다.
관측 alert와 authority-bearing inbox 항목을 섞지 않는다. 모델 heartbeat나
외부 메신저를 기본으로 추가하지 않는다.

- [ ] 같은 사건의 재수집·재시작으로 알림이 반복되지 않고 resolve/reopen 구분이 유지된다.
- [ ] 인위적인 clock으로 stall threshold와 fresh/expired를 검증하며 긴 실제 sleep을 쓰지 않는다.
- [ ] 로컬 알림은 명시적 opt-in이며 prompt·명령·경로·비밀값을 표시하지 않는다.
- [ ] 대기는 bounded timeout/취소를 지원하고 timeout이 worker 실패·재실행·새 승인으로 처리되지 않는다.

### CP-08 — 실행 Timeline·사용량·진단

`panel_metrics.py`, `panel_chats.py`, `artifact-telemetry.py`의 기존 집계를 우선 사용한다.
도구 실행 시간, 대기 원인, 오류 반복을 local metadata로 표현한다.
비싼 모델이 로그를 읽어 매 refresh 요약하는 기능은 만들지 않는다.

- [ ] Requested model, served model, harness 및 출처·시각·불명 상태가 구분된다.
- [ ] Input/output/cache-read/cache-write·부분 집계·parent 포함 범위를 명시하고 중복 합산하지 않는다.
- [ ] API 환산 추정치, 실제 provider 보고 비용, 구독 quota를 분리하며 token에서 quota 비율을 계산하지 않는다.
- [ ] 반복 tool/stall 진단은 advisory이며 자동 kill/retry/provider fallback을 유발하지 않는다.

### CP-09 — 재개·복구·worktree 수명주기

기존 restart/swap/settle/salvage/cleanup front door를 연결한다.
세션 복사로 context를 승계했다고 가정하거나, 타 provider로 native history를 복제하지 않는다.

- [ ] Ctrl-C, pane loss, Main exit, 중단된 worker, 재시작 후 stale binding의 복구 시나리오를 검사한다.
- [ ] Dirty worktree·partial patch·실패 결과는 보존되고 active landing/lease 중 위험한 전환이 거부된다.
- [ ] Pi/Agy resume은 native identity·ownership·capability 검증이 없으면 기존대로 거부하고 fresh restart와 구분한다.
- [ ] 복구 전후 sole-Main·room lineage·주간 quota swap·live-worker update deferral 계약이 유지된다.

### CP-10 — 결과·리뷰·evidence 연결

기존 Results/Events/Diff/Transcript 및 runtime evidence projection을 연결한다.
Reviewer finding과 해당 patch/attempt/criterion으로 바로 이동할 수 있게 하되,
새 finding authority store나 별도 review engine은 만들지 않는다.

- [ ] Worker exit, verify pass, reviewer verdict, owner acceptance, commit/push/CI/landing을 별도 단계로 표시한다.
- [ ] Task/plan 교체, 같은 HEAD의 파일 변경, 후속 continuation 뒤에는 오래된 증거를 현재 성공으로 표시하지 않는다.
- [ ] Findings가 잘리면 생략 수와 원본 근거를 제공하고 모든 쟁점이 해결됐다고 표시하지 않는다.
- [ ] 기존 독립 모델 계열·verifier 불변·review/admission gate를 UI 조작으로 우회할 수 없다.

### CP-11 — 다중 호스트 읽기 전용 관측 (선택형)

명시적으로 선택한 호스트의 제한된 snapshot만 SSH로 읽는 adapter를 먼저 검토한다.
기존 SSH 신뢰 설정을 사용하고 credentials 배포, 자동 호스트 발견,
원격 arbitrary shell, 중앙 DB·mesh controller는 추가하지 않는다.

- [ ] Host allowlist·known-host 검증·명령 인자 검증·호스트별 timeout/byte/concurrency cap이 있다.
- [ ] Host와 boot/process generation이 다른 동일 session/PID를 혼동하지 않는다.
- [ ] Unreachable·clock skew·old snapshot은 명시되며 마지막 상태를 live로 꾸미지 않는다.
- [ ] 기본 snapshot에는 raw transcript·명령 인자·절대 경로·credential이 없고 원격 변경 명령은 불가능하다.

### CP-12 — 로컬 읽기 전용 웹 뷰 (선택형)

먼저 별도 ADR로 실제 소비자와 dependency 비용을 검토한다.
명시적으로 실행하는 process-lifetime 뷰만 고려하며 상주 daemon/자동 시작/기본 포트 개방은 없다.
TUI와 같은 제한된 projection을 쓰고 브라우저에서 provider 로그를 직접 읽지 않는다.

- [ ] Loopback-only, 정확한 Host/Origin 검증과 짧은 수명 인증을 적용하고 URL/log에 credential을 남기지 않는다.
- [ ] 보이는 데이터와 redaction·범위·partial 상태가 TUI와 같고 모든 mutation endpoint가 없다.
- [ ] DNS rebinding, cross-origin, XSS/terminal escape, path traversal 및 연결별 rate/byte 제한을 테스트한다.
- [ ] 종료하면 listener·child가 남지 않으며 설치·방화벽·업데이트 기본값은 바뀌지 않는다.

### CP-13 — 원격·모바일 typed control (기본 보류)

CP-11/12의 통과만으로 승인하지 않는다. 별도 threat model과 사용자 승인이 있어야
추진한다. 단순 임의 command box, tmux send-keys API, 승인 버튼 자동화는 채택하지 않는다.

- [ ] 허용 action 목록과 authority/대상/만료가 명시된 요청만 기존 parent front door로 전달한다.
- [ ] Approval은 정확한 task/attempt/patch digest/actor/expiry에 묶이고 replay·CSRF·이중 실행을 거부한다.
- [ ] 응답 손실 후 무조건 재전송하지 않으며 idempotency와 결과 receipt로 상태를 확인한다.
- [ ] 기본 feature-off이고 worker·observed 세션은 승인 권한을 얻지 못하며 merge/release는 별도 권한으로 남는다.

### CP-14 — 연구/HPC job–attempt 연결

기존 runtime backend와 `oms job-digest` 결과를 재사용한다.
필요한 것은 관측·근거의 연결이지 Slurm 제출/재제출/취소를 수행하는 새 scheduler가 아니다.

- [ ] Cluster/job identity와 attempt/experiment reference를 명시적으로 연결하고 job 번호 재사용을 구분한다.
- [ ] Queue/running/terminal 상태와 연구 결과의 검증·승격 상태를 분리한다.
- [ ] 가짜 squeue/sacct/log 출력으로 pending·timeout·OOM·preemption·partial log·unreachable을 검사한다.
- [ ] 실제 GPU·Slurm 제출 없이 완료 가능한 fixture를 제공하며 live 검증 부재를 명시한다.

### CP-15 — 터미널 backend parity

tmux를 교체하지 않는다. 기존 inline/선택형 Herdr host에서 지원되는 launch,
read-only navigation, focus, reconnect를 capability별로 점검한다.
제거된 Herdr pane-control/A2A bridge를 새 authoritative controller로 되살리지 않는다.

- [ ] Host별 지원/미지원과 process persistence 대 session reconstruction을 구분해 표시한다.
- [ ] Pane create/attach/focus/resize/close가 session identity를 확인하며 다른 pane을 종료하지 않는다.
- [ ] Host 설치가 없거나 protocol이 달라지면 명확히 거부하고 지원하지 않는 기능을 우회 호출하지 않는다.
- [ ] Linux/WSL2/macOS 계약을 유지하고 새 binary·native Windows 지원·자동 설치를 기본 요구로 추가하지 않는다.

### CP-16 — 회귀·계측·fixture (모든 구현 PR)

가짜 provider, 임시 HOME/repo, private PTY/tmux, deterministic clock을 사용한다.
새 suite를 무조건 추가하지 않고 기존 canonical case를 확장한다.
측정 숫자는 목표·계획과 실제 결과를 분리한다.

- [ ] 각 기능에 정상·실패·권한 경계 case와 변경 전 재현 또는 unmet-contract fixture가 연결된다.
- [ ] Provider/인터넷/유료 추론/GPU 없이 핵심 계약을 검증하고 socket/PTY 불가는 pass가 아닌 미검증으로 기록한다.
- [ ] 기준/후속 SHA, fixture 규모, bytes read, subprocess 수, CPU/RSS, update latency, test runtime을 함께 기록한다.
- [ ] 민감한 실제 transcript 대신 synthetic fixture를 쓰고 중복 테스트·CI lane 증가의 필요성을 설명한다.

### CP-17 — 단계적 이행·문서·rollback (모든 구현 PR)

한 작업에 하나의 canonical writer를 유지한다. 새 관측은 좁은 scope에서 시작하고,
선택형 기능을 끄면 기존 동작으로 돌아가야 한다. 공개 계약이나 기본값 변경은 따로 승인받는다.

- [ ] 문서·help·실제 capability가 일치하고 제안된 기능을 지원 기능으로 선전하지 않는다.
- [ ] 오래된 reader/state를 위한 호환성 검사와 feature-off/rollback 절차가 있다.
- [ ] 기존 install/update/repair/uninstall 및 같은-SHA CI·full-proof 설치 계약을 유지한다.
- [ ] 구현 SHA, 정확한 검사 범위, native 미검증 항목, 후속 blocker를 PR에 기록한다.

### CP-18 — 참고 구현·채택·라이선스 기록

[참고 프로젝트 대장](CONTROL-PLANE-REFERENCES.md)을 사용한다.
Reference는 설계 비교이지 의존성 채택 승인이나 성능 보증이 아니다.

- [ ] 가져올 아이디어마다 원본 repo/파일/commit 또는 release와 검토일을 기록한다.
- [ ] 코드 복사 전 해당 버전의 LICENSE/NOTICE·귀속·의존성을 확인하고 불명확하면 복사하지 않는다.
- [ ] Stars/홍보성 생산성 수치 대신 OMS에 필요한 기능과 실패 경계로 비교한다.
- [ ] 유지보수 중단·제약 있는 source license·미검증 후보는 별도 표시하고 자동 설치 대상에 넣지 않는다.

## 5. 후속 PR 분할과 착수 순서

아래 I1–I11은 제안된 구현 묶음이며 실제 생성된 PR 번호가 아니다.
현재 PR은 문서만 올린다. CP-16/17의 검증·문서는 각 묶음 안에서 완료한다.

| 묶음 | 범위 | merge 전 확인 | 이번 단계의 판단 |
|---|---|---|---|
| I1 | CP-01/02/18: coverage·identity·채택 근거 | 기존 native 관측/sole-Main 회귀 | 먼저 |
| I2 | CP-03/04: bounded provider adapter | opt-in, parser 실패, privacy, 상한 | I1 다음 |
| I3 | CP-05: 통합 projection | schema 호환, 중복, authority 무변경 | I2 다음 |
| I4 | CP-06/07: 탐색·Attention | private PTY와 좁은 화면·키보드 | I3 다음 |
| I5 | CP-08/10: telemetry·evidence 상세 | 비용/부분값/증거 신선도 | I3 이후 병행 가능 |
| I6 | CP-09: 안전한 복구 | dirty worktree, rollover, ownership | I3 이후 병행 가능 |
| I7 | CP-15: 선택형 host parity | native host 또는 명시적 미검증 | 필요 시 독립 PR |
| I8 | CP-14: 연구/HPC 연결 | fake Slurm, no-submit 보장 | 필요 시 독립 PR |
| I9 | CP-11: SSH 읽기 전용 | transport 경계·host identity | 별도 승인 후 |
| I10 | CP-12: 읽기 전용 웹 | ADR·web threat model·opt-in | 별도 승인 후 |
| I11 | CP-13: 원격 mutation | 별도 사용자 승인·security review | 보류 |

로컬 MVP 완료는 I1–I6의 수락 조건과 해당 CP-16/17 증빙으로 판단한다.
I7–I11이 보류돼도 MVP를 미완료로 보지 않는다. 반대로 문서 병합이나 UI 스크린샷만으로
어느 구현 항목도 완료 처리하지 않는다.

## 6. 테스트 연결과 측정 계획

기존 section 이름은 [operator runner](../tests/operator/runner.py)를 기준으로 한다.
아래는 **후속 구현자가 실행할 명령**이며 이번 문서 PR에서 실행했다는 뜻이 아니다.

| 작업 | 우선 확장할 기존 검사 |
|---|---|
| CP-01–05 | `core/native-integration`, `core/dashboard`, `shared-cache` |
| CP-06/07 | `core/visual-and-controls`, `core/room-and-board-rendering`, `native-pty` |
| CP-08 | `core/native-integration`, `core/dashboard`, `dispatch-results`의 관련 case |
| CP-09/15 | `worker-leases`, `room-rollover`, `salvage`, `native-pty` |
| CP-10 | `auto-review`, `dispatch-results`, 기존 runtime evidence 회귀 |
| CP-11/12/13 | 기존 경계 fixture 재사용; transport 미커버 경로만 새 bounded case 추가 |
| CP-14 | 기존 job-digest/runtime backend fixture; fake Slurm |

```bash
bash tests/operator-tools-smoke.sh --list-sections
bash tests/operator-tools-smoke.sh --section core/native-integration
bash tests/operator-tools-smoke.sh --section core/dashboard,shared-cache
bash tests/operator-tools-smoke.sh --section core/visual-and-controls,core/room-and-board-rendering
bash tests/operator-tools-smoke.sh --section auto-review
bash scripts/check.sh --affected --changed-from BASE --changed-to HEAD
```

계측 workload는 synthetic session 1/10/50/100개, 표본 JSONL 10 MiB/100 MiB,
idle·append·rotate·malformed·unreachable 조건으로 시작한다. 구현 전 baseline을 기록하고,
각 refresh의 hard cap을 설정한다. 최초 cold scan과 warm update, 전체 역사와 연결 세션을
분리한다. 구체적인 성능 합격치는 baseline 후 고정하며 좋은 결과만 보고 threshold를 바꾸지 않는다.

기존 file/read cap을 완화하지 않고 개선할 수 있는지 먼저 측정한다.
macOS/WSL2/native provider UI 검증을 못 했다면 별도 미검증으로 남긴다.
실제 연구 업무, paid inference, GPU/Slurm 제출은 이 계획의 필수 완료 조건이 아니다.

## 7. 착수하지 않을 작업

새 orchestration framework로 교체, 두 번째 DB/daemon, 여러 Main, 자동 swarm 확대,
새 reviewer engine, 글로벌 transcript 수집, raw log의 프롬프트 주입, token 기반 quota 추정,
자동 API 결제 전환, 기본 원격 포트 노출, 무조건 재시도·kill·cleanup, 자동 merge/release,
provider history 복사, 새로운 외부 메신저/Notion 동기화는 이 계획의 기본 범위가 아니다.
미지원 resume이나 unknown identity를 추측으로 통과시키지 않는다.

## 8. 구현자·리뷰어 인계

구현자는 현재 main과 이 기준 SHA의 차이를 먼저 대조하고 I1부터 가장 작은 미완료
묶음 하나를 선택한다. 테스트를 먼저 확장하고 기존 helper를 재사용한다.
각 PR에 CP ID, 범위/비범위, base/head SHA, source anchors, 변경 전후 fixture,
실행 명령과 결과, 미검증 환경, 계약 변경 여부 및 rollback을 기록한다.

리뷰어는 특히 다음 경계를 확인한다: observed → authority 승격 금지,
mtime → liveness 오인 금지, collector 실패 → zero/success 변환 금지,
identity 확인과 실제 제어 사이 race, 비용·quota 이중 집계, native log 개인정보,
같은 HEAD의 변경 뒤 stale evidence, sole-Main 및 기존 landing gate 보존.
체크박스는 해당 구현과 검증 근거가 연결된 뒤에만 변경한다.
