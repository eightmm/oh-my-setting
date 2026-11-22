# OMS 부족 기능 순위: 다른 하네스·오케스트레이터·협업 도구 대비

작성: 2026-11-22. **COMPARISON / CANDIDATE RANKING ONLY**.
Source baseline: `3416c2186f838f96ad13522dac39b1ff850d4b23`.

이 문서는 OMS를 "임의의 에이전트 하네스이자 에이전트 협업 도구"로 볼 때 다른 도구에는
있고 OMS에는 없거나 부분적인 기능을 한 줄로 세운 후보 목록이다. 병합해도 구현 약속,
기본값 변경, 새 작업 queue가 생기지 않는다. 범위 결정은 [feature scope](FEATURE-SCOPE.md)가,
남은 작업과 착수 조건은 [implementation queue](ORCHESTRATOR-WORK-QUEUE.md)가 계속 소유한다.
이 순위는 그 두 문서에 넣을 후보를 고르는 입력이다.

## 읽는 법과 한계

- 외부 도구의 기능은 2026-11-22에 공개 문서·README로 조사한 범위다. 설치·실행·부하·보안
  벤치마크는 하지 않았다. 구현이나 코드 재사용 전에 정확한 commit과 LICENSE를 다시 고정한다.
- **확인** 열의 `소스`는 위 baseline의 저장소 파일에서 직접 확인했다는 뜻이다. `보고`는
  PR·이슈 본문을 읽은 조사 결과이며 소스 트리에서 다시 확인하지 않았다.
- **관계** 열은 기존 결정과의 관계다. `정합`은 현재 범위 안에서 빠진 consumer나 강제를
  채우는 일, `조건부`는 [feature scope](FEATURE-SCOPE.md)가 요구하는 named workflow나
  증거가 먼저 필요한 일, `비범위`는 기존 문서가 제외한 일이라 범위 결정을 다시 해야 하는 일이다.
- 순위 기준: (1) Main은 관리만 하고 Lead·Worker가 만드는 역할 분리, (2) `oms land` 시간,
  (3) token·비용 절약, (4) 임의 하네스·협업 방향, (5) 기존 owner를 재사용할 수 있는 정도.
- AH-01~13은 [#313](https://github.com/eightmm/oh-my-setting/pull/313)의 work packet ID다.
  겹치는 항목은 새 설계를 만들지 않고 해당 packet을 가리킨다.

## 다시 만들지 않을 것

아래는 비교 대상보다 OMS가 이미 강한 부분이다. 후보에서 제외했다.

- criterion에 연결된 evidence, exact-SHA 검증, 전송 경계 이후 재전송 금지와 unknown 보존.
- Codex·Claude Code·AGY를 같은 계약으로 다루는 council과 Main 선택.
- land 전 scrubber·invariants, fail-closed secret scan.
- 백업 복원이 되는 install·update·uninstall, profile별 설치, doctor.
- Slurm·SSH, 데이터 split 누수 검사, 실험 계약, artifact view 같은 ML/HPC 지원.
- 근거 없는 capability 주장을 걷어내는 provider capability 표.

## 순위

### S: 역할 분리와 land 시간에 직접 걸리는 것 (1–6)

| 순위 | 부족 기능 | 가진 도구 | OMS 현황 | 확인 | 관계 | PR 단위 제안 |
|---|---|---|---|---|---|---|
| 1 | Main의 직접 저장소 수정을 runtime이 거절 | Claude Code PreToolUse hook의 호출 차단, Codex read-only sandbox | [Main policy](TEAM-ORCHESTRATION-MAIN-POLICY.md)가 관리 전용을 정했지만 프롬프트·스킬 계약이며 runtime 거절은 없다고 명시 | 소스 | 정합 | ① 팀을 운영하는 Main pane의 pre-tool guard에 저장소 쓰기 거절 추가(OMS 상태 명령은 허용) ② Codex·AGY Main은 read-only로 기동 ③ 사용자가 지시한 예외는 Room 기록과 1회성 허용 |
| 2 | Native 배정·결과 bridge와 공통 task card | Claude Code agent teams의 공유 task list, Gas Town Beads·Convoy, Vibe Kanban 카드 | Plan에 `depends`·`ready`·claim·lease가 있음. native `--orchestrator --task`는 텍스트만 보내고 task/lease/result 바인딩을 만들지 않음(#285 P0-B 미구현) | 소스 | 정합(새 task DB는 비범위) | ① native 배정을 기존 Plan task·lease에 바인딩 ② 결과를 Room/result writer로 회수 ③ 패널 task card가 같은 레코드를 표시 |
| 3 | 배치 land 후보와 실패 시 bisect | Gas Town Refinery, Graphite merge queue 배치 | land는 통합 대상별 직렬. 합친 결과를 다시 검증한다는 원칙은 [multi-host plan](MULTI-AGENT-MULTI-HOST-PLAN.md)에 있음 | 소스 | 조건부(GitHub merge queue 의존과 보호 규칙 우회는 비범위) | ① 수락된 후보 N개를 하나의 통합 후보로 합쳐 같은 tree로 한 번 검증 ② 실패하면 절반씩 나눠 원인 후보만 보류 ③ 패널에 대기열 표시 |
| 4 | CI 우선 land 경로 마무리 | GitHub merge queue의 merge_group CI, Graphite 병렬 CI | 같은 SHA의 CI를 설치 증거로 쓰고 affected 선택이 있음. `focused` job은 한 덩어리이고 macOS 간헐 실패(#327)가 gate를 막음 | 소스(workflow), 보고(#327) | 정합 | ① focused suite를 영역별 CI job으로 분할 ② 계측 상위 느린 테스트 정리 ③ land가 로컬 재실행 없이 같은 SHA CI 결과만 기다리는 경로 |
| 5 | Room 메시지를 MCP로 노출 | Claude Code agent teams mailbox | Room message·cursor·ack는 CLI에 있음. MCP 도구에는 inbox·handoff만 있고 Room 쓰기·읽기 도구가 없음 | 소스 | 조건부(모든 verb의 MCP화는 비범위. AH-09의 named workflow로 한정) | ① Room 읽기·게시·ack 도구(scrubber, 응답 상한 적용) ② 참가자 권한 검사 ③ Lead↔Worker 직접 질의로 Main 중계 감소를 계측 |
| 6 | OMS 측 하드 예산 집행 | 조사 범위에서 확실한 선례는 없음(Agent HQ는 session당 과금 단위만 확인) | campaign 예산은 soft admission이며 provider hard cap이 아니라고 명시. Team view는 `native_global_hard_cap`을 unsupported로 표시 | 소스 | 조건부(provider 지출 상한으로 표현하지 않음) | ① task·team·일 단위 상한을 넘으면 새 위임을 거절하는 opt-in 정책 ② 초과 직전 낮은 tier 제안(자동 승급·강등은 별도 승인) ③ unknown 사용량은 0이 아닌 부채로 유지 |

### A: token 절약과 임의 하네스 방향 (7–20)

| 순위 | 부족 기능 | 가진 도구 | OMS 현황 | 확인 | 관계 | PR 단위 제안 |
|---|---|---|---|---|---|---|
| 7 | 작업 카테고리 → 모델 tier 별칭과 선택 이유 출력 | oh-my-openagent의 category와 models 체인 | 역할 routing, tier 문서, account overflow와 명시적 model fallback이 있음. 카테고리 한 단어로 고르는 경로와 이유 설명은 부족(#269) | 소스(fallback), 보고(#269) | 조건부(universal router·자동 모델 승급은 비범위) | ① 기존 routing rule 위의 category 별칭 ② 선택·제외 이유 출력 ③ 하위 작업 기본 tier를 낮게 두는 profile |
| 8 | Worker sandbox 기본 적용과 네트워크 정책 | Codex 커널 sandbox | `OMS_WORKER_SANDBOX`는 opt-in | 소스(존재), 보고(기본값·네트워크) | 조건부(AH-06 기본값 변경은 별도 승인) | ① pre-tool hook이 없는 transport는 sandbox 자동 ② 네트워크가 필요 없는 경로의 차단 옵션 |
| 9 | 결정적 정체 감지와 attention 승격 | Gas Town Witness·Problems 뷰 | fail-ledger 정체 기록과 attention 알림이 있음. 주기적으로 확인해 승격하는 주체는 없음 | 보고 | 조건부(AI 감독 계층, 자동 kill·cold restart는 [비범위](CONTROL-PLANE-EVOLUTION.md#7-착수하지-않을-작업)) | ① LLM 없이 무출력 시간·같은 실패 반복을 판정 ② Main attention으로만 승격 ③ nudge·handoff는 사람이 누르는 키 |
| 10 | 편집 단위 checkpoint와 되돌리기 | Claude Code `/rewind` | worktree와 Git에 의존. turn 단위 되돌리기 명령 없음 | 소스 | 정합 | ① Worker turn마다 가벼운 Git ref ② attempt의 특정 turn으로 복원하는 명령 |
| 11 | 역할별 실효 권한 보고 | — | Codex 역할 파일의 sandbox 설정이 실제 read-only를 보장하지 않는다는 보고(#277) | 보고 | 정합 | ① doctor가 provider·역할별로 강제되는 것과 지침뿐인 것을 구분해 출력 ② read-only 역할 fixture |
| 12 | 충돌 시 integrator 자동 배정과 재검증 | Gas Town Refinery, Graphite 자동 rebase | `integrator` 역할과 routing이 있음. 충돌 감지에서 배정까지의 자동 연결은 미확인 | 소스(역할) | 정합(충돌 수정은 새 통합 후보로 재검증) | ① land 충돌을 integrator 작업으로 생성 ② 결과를 새 후보로 admit |
| 13 | 다중 계정 바인딩과 quota 기반 routing 설명 | — | native pane이 계정에 묶이지 않고 자동 routing이 최근 활동량으로 순위를 매긴다는 보고(#269) | 보고 | 정합 | ① 계정 home 바인딩 ② provider별 제외·순위 이유 |
| 14 | 패널 입력 반응성 | — | snapshot 수집 중 키 입력이 기다린다는 보고(#267) | 보고 | 정합 | ① 증분 원장 인덱스 ② 수집 중 마지막 프레임 유지 |
| 15 | 같은 구현의 N회 병렬 시도와 선택 | Codex cloud의 attempts, Agent HQ의 다중 에이전트 배정 | problem campaign이 명시적 verifier로 후보를 비교 | 소스 | 조건부(일반 코딩의 필수 단계로 만들지 않음) | ① campaign을 worktree N개 위임에 연결하는 짧은 진입점 ② 탈락 후보 정리 |
| 16 | ACP client | ACP를 지원하는 에디터·에이전트 | 설계만 있음([research follow-ups](RESEARCH-FOLLOWUPS.md), AH-04) | 소스 | 조건부(기존 CLI transport로 못 하는 named workflow 필요) | ① 선택형 ACP transport ② adapter conformance fixture 재사용 |
| 17 | 범용 원격·격리 실행 profile | Codex cloud, Claude Code cloud 세션, Copilot coding agent | SSH adapter·Slurm·Apptainer CPU는 있음. generic `remote`·`isolated` profile은 unavailable 보고 | 소스 | 조건부 | ① SSH Worker transport를 `remote` profile에 연결 ② 결과를 patch로 가져와 admit |
| 18 | 외부 이벤트·시간 trigger | Claude Code 예약 작업, Agent HQ 이슈 이벤트 | `oms tick` 예약 유지보수, 자동 update timer, bounded autopilot은 있음. 외부 이벤트로 작업을 시작하는 명령은 없음 | 소스 | 조건부(상주 서버는 비범위) | ① 기존 scheduler 감지를 재사용한 시간 trigger ② CI 실패 관측을 검토 대기 작업으로 기록 |
| 19 | flaky 테스트 자동 1회 재시도와 격리 목록 | merge queue 계열 도구 | `oms ci-status flaky`가 있음. 자동 재시도·격리 목록은 미확인 | 소스(명령) | 정합(required gate는 유지) | ① 알려진 flaky만 1회 재시도 ② 격리 목록과 만료일 |
| 20 | 양방향 원격 알림·승인 | oh-my-team의 Telegram·Slack, GitHub Mobile | `OMS_PANEL_ATTENTION_CMD` notifier가 있음(단방향) | 소스 | 비범위(web·mobile 제어는 MVP 전제에서 제외) | ① 내용 없이 식별자만 보내는 notifier preset ② 승인 응답은 범위 결정 후 |

### B: 있으면 좋지만 뒤로 미뤄도 되는 것 (21–35)

| 순위 | 부족 기능 | 가진 도구 | OMS 현황 | 확인 | 관계 | PR 단위 제안 |
|---|---|---|---|---|---|---|
| 21 | 이슈 라벨 polling과 자동 intake | Agent HQ, Conductor의 Linear·GitHub 흐름 | [`intent draft --from-issue`](INTENT-SOURCE-INTAKE.md)가 SHA 고정 이슈 한 건을 받음 | 소스 | 조건부 | 라벨 조회 → 검토 대기 draft 생성(자동 실행 없음) |
| 22 | provider별 hook 이벤트 coverage 표와 trace 확대 | Claude Code의 이벤트 종류 | Codex callback trace sidecar가 있음. provider 간 coverage 표는 AH-05 | 소스 | 정합 | coverage 표 → Claude·AGY trace |
| 23 | Stacked PR land | Graphite | 없음 | 소스 | 조건부(3위 뒤) | 순서가 있는 후보 묶음을 배치 land에 연결 |
| 24 | 임의 CLI adapter 확대 | claude-squad, Vibe Kanban | custom adapter 경계가 있음. conformance 예제는 #319 | 소스 | 조건부(named workflow와 fixture 필요) | OpenCode·Gemini CLI·Cursor CLI adapter를 하나씩 |
| 25 | 규칙을 다른 포맷으로 내보내기 | rulesync, ruler | CLAUDE.md·AGENTS.md 연결만 있음 | 소스 | 조건부 | Cursor rule·Copilot instructions 생성 명령 |
| 26 | PR 코멘트에서 후속 작업 시작 | Agent HQ의 에이전트 멘션 | `--land --auto-repair` opt-in 수리가 있음 | 소스 | 조건부(넓은 자동 채택은 deferred) | 수리 대상 관측을 PR 코멘트로 확대 |
| 27 | 리뷰 결과를 PR 코멘트로 게시 | Copilot code review, CodeRabbit | 로컬 peer-review·council | 보고 | 조건부(게시는 publication 승인 필요) | 승인된 finding만 게시 |
| 28 | skill·역할 eval 세트 | Agent HQ식 결과 비교 | paired evaluation은 deferred 설계 | 소스 | 조건부(같은 작업 비교와 verifier 정의 필요) | 작은 시나리오 세트와 모델 교체 전후 비교 |
| 29 | 읽기 전용 로컬 web 보드 | Vibe Kanban, Conductor | tmux 패널, dashboard, 오프라인 artifact view | 소스 | 비범위(상주 HTTP 서버, web 제어) | 정적 snapshot export까지만 |
| 30 | 문서 경로 drift 검사 | — | 백틱 경로가 검사되지 않는다는 보고(#295) | 보고 | 정합 | 문서 reference 검사에 백틱 경로 추가 |
| 31 | ML checkpoint 정책 lint | — | latest+best 정책은 산문 지침 | 보고 | 정합(ML 정책은 template·contract에 둠) | 저장 호출 패턴과 val 주기 검사 |
| 32 | 실험 추적 도구로 export | W&B, MLflow | run ledger·capsule·실험 계약 | 보고 | 조건부 | capsule → MLflow export |
| 33 | GPU·Slurm 자원을 본 admission | — | Slurm 대기·요약 | 보고 | 조건부(cluster scheduler가 되지 않음) | 가용량 조회를 위임 admission의 참고값으로 |
| 34 | OpenTelemetry export | — | 로컬 telemetry | 소스 | 조건부(기존 로컬 증거 우선) | opt-in OTLP exporter |
| 35 | plugin marketplace 게시 | Claude Code plugin marketplace | plugin 디렉터리와 skill manifest | 소스 | 조건부 | OMS skill을 marketplace 형식으로 묶기 |

### C: 낮은 우선순위 또는 보류 (36–50)

| 순위 | 부족 기능 | OMS 현황 | 확인 | 관계 | 제안 |
|---|---|---|---|---|---|
| 36 | 세션을 다른 호스트에서 이어받기 | local handoff, portable capsule | 소스 | 조건부 | handoff를 원격 호스트에서 resume |
| 37 | Main 교체(swap) | native host의 `swap_main_host`는 unsupported | 소스 | 조건부(구체적 handoff 요구와 lifecycle 증거 필요) | CC-09 host 작업 |
| 38 | worktree 수명주기 뷰와 orphan 정리 | lease GC, cleanup | 소스 | 정합 | 정리 대상 dry-run 목록 |
| 39 | 작업별 dev server 미리보기 | 없음 | 보고 | 조건부 | worktree별 포트 할당 |
| 40 | prompt injection 방어 강화 | 숨은 유니코드 표시, 내용은 권한이 아니라는 규칙 | 보고 | 정합 | 외부 텍스트 격리 표시와 도구 권한 축소 |
| 41 | council 좌석 설정화 | #273, #318 진행 중 | 보고 | 정합 | catalog 기반 좌석 해석 |
| 42 | usage ledger 증분 저장 | 전체 bounded history를 다시 씀 | 소스 | 조건부(retained bytes와 갱신 비용 계측 먼저) | 계측 → compaction 설계 |
| 43 | 한 단어 "끝까지" 실행 trigger | goal-drive, plan-run | 보고 | 조건부 | 승인된 goal에만 붙는 skill |
| 44 | 리뷰 settle 사유 보존 | 버그 보고(#275) | 보고 | 정합 | 사유를 event 참조로 전달 |
| 45 | Codex Lead 긴 배정 가시성 | 버그 보고(#322) | 보고 | 정합 | 정책 축약 후 전송 |
| 46 | Room 헤더와 주간 사용량 표시 | 보고(#274) | 보고 | 정합 | 패널 표시 수정 |
| 47 | CI 안에서 에이전트 실행 template | 없음 | 보고 | 조건부(CI는 작게 유지) | 명시적 trigger workflow template |
| 48 | 감사 보고서 export | 이벤트 원장 | 보고 | 조건부 | 기간별 감사 요약 명령 |
| 49 | 팀·다중 사용자 공유 설정과 RBAC | 1인 로컬 | 보고 | 보류 권장 | — |
| 50 | native Windows | 지원 종료, 거절 | 소스 | 제외 확정 | — |

## 권장 순서

1. 1위와 4위를 먼저 한다. 둘 다 기존 guard와 workflow를 고치는 일이고 효과를 바로 잴 수 있다.
2. 3위와 19위를 묶는다. land 시간에 남은 가장 큰 덩어리다.
3. 2위와 5위로 Main이 배정과 수락만 하는 흐름을 닫는다. 그다음 6위와 7위로 token 사용을 묶는다.
4. `비범위`로 표시한 9·20·29위는 구현 PR이 아니라 [feature scope](FEATURE-SCOPE.md) 수정 PR이 먼저다.

각 항목은 한 PR에 한 단계씩, 최신 main 기준으로 올린다. install·update·uninstall, guard,
scrubber 계약 테스트와 CI 요구는 어느 항목에서도 줄이지 않는다.

## 외부 출처

| 도구 | 확인한 내용 | 출처 |
|---|---|---|
| Claude Code agent teams | 공유 task list와 teammate 간 메시지, experimental이며 기본 꺼짐 | [agent teams](https://code.claude.com/docs/en/agent-teams) |
| Graphite | stack을 아는 merge queue, 배치와 bisection | [stack-aware merge queue](https://graphite.com/blog/the-first-stack-aware-merge-queue) |
| GitHub Agent HQ | 이슈를 Copilot·Claude·Codex에 배정 | [Agent HQ](https://github.blog/news-insights/company-news/pick-your-agent-use-claude-and-codex-on-agent-hq/) |
| oh-my-openagent | category 위임과 models 체인 | [configuration](https://github.com/code-yeongyu/oh-my-openagent/blob/dev/docs/reference/configuration.md) |
| Gas Town | Refinery, Witness, Problems 뷰 | [control-plane 참고 대장](CONTROL-PLANE-REFERENCES.md) |
| rulesync | 여러 도구의 rule 파일 생성 | [rulesync](https://github.com/jpcaparas/rulesync) |
| oh-my-team | Telegram·Slack 원격 제어 | [oh-my-team](https://github.com/erkandogan/oh-my-team) |

Codex CLI의 sandbox·cloud attempts, Claude Code의 hook 차단과 `/rewind`, Vibe Kanban,
claude-squad, Conductor는 각 공식 문서와 README 조사 결과를 요약했다. Vibe Kanban과
Conductor는 운영 지속성이 불확실하다는 보도가 있어 장기 기준선으로 삼지 않는다.
