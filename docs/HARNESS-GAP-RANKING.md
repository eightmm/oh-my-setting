# OMS 부족 기능 순위: 다른 하네스·오케스트레이터·협업 도구 대비

작성: 2026-11-22. **COMPARISON / CANDIDATE RANKING ONLY**.
Source baseline: `76ff19f` (재검증 기준).

이 문서는 OMS를 "임의의 에이전트 하네스이자 에이전트 협업 도구"로 볼 때 다른 도구에는
있고 OMS에는 없거나 부분적인 기능을 한 줄로 세운 후보 목록이다. 병합해도 구현 약속,
기본값 변경, 새 작업 queue가 생기지 않는다. 범위 결정은 [feature scope](FEATURE-SCOPE.md)가,
남은 작업과 착수 조건은 [implementation queue](ORCHESTRATOR-WORK-QUEUE.md)가 계속 소유한다.
이 순위는 그 두 문서에 넣을 후보를 고르는 입력이다.

## 재검증 결과

첫 판은 README·PR·이슈 본문을 읽은 조사만으로 50개를 세웠다. 같은 날 저장소 소스와
이슈 상태로 다시 확인하니 그중 상당수는 공백이 아니었다.

- 근거로 든 이슈 7건(#267, #269, #273, #274, #275, #295, #322)은 이미 완료로 닫혀 있었다.
- 그 7건을 포함해 12개 항목은 이미 있었다. [이미 있는 것](#이미-있는-것-순위에서-제외)에 옮겼다.
- 6개 항목은 기존 범위 결정이 제외한 것이라 따로 묶었다.
- 남은 후보는 32개이고, 그중 여러 개는 열린 PR이나 이슈에서 진행 중이다.

외부 도구의 기능은 공개 문서·README 조사 범위이며 설치·실행·벤치마크는 하지 않았다.

## 읽는 법

- **확인**: `소스`는 baseline의 파일이나 이슈 상태에서 직접 확인했다는 뜻이다. `보고`는
  조사 결과이며 소스에서 다시 확인하지 못했다.
- **관계**: `정합`은 현재 범위 안에서 빠진 consumer나 강제를 채우는 일, `조건부`는
  [feature scope](FEATURE-SCOPE.md)가 요구하는 named workflow나 증거가 먼저 필요한 일이다.
- **진행**: 같은 일을 다루는 열린 PR·이슈다. 비어 있으면 확인된 진행이 없다.
- 순위 기준: (1) Main은 관리만 하고 Lead·Worker가 만드는 역할 분리, (2) `oms land` 시간,
  (3) token·비용 절약, (4) 임의 하네스·협업 방향, (5) 기존 owner 재사용 정도.
- AH-01~13은 [#313](https://github.com/eightmm/oh-my-setting/pull/313)의 work packet ID다.

## 다시 만들지 않을 것

비교 대상보다 OMS가 이미 강한 부분이다.

- criterion에 연결된 evidence, exact-SHA 검증, 전송 경계 이후 재전송 금지와 unknown 보존.
- Codex·Claude Code·AGY를 같은 계약으로 다루는 council과 Main 선택.
- land 전 scrubber·invariants, fail-closed secret scan.
- 백업 복원이 되는 install·update·uninstall, profile별 설치, doctor.
- Slurm·SSH, 데이터 split 누수 검사, 실험 계약, artifact view 같은 ML/HPC 지원.
- 근거 없는 capability 주장을 걷어내는 provider capability 표.

## 남은 후보 순위

### S: 역할 분리와 land 시간 (1–7)

| 순위 | 공백 | 가진 도구 | OMS 현황 | 확인 | 관계 | 진행 | 다음 PR 단위 |
|---|---|---|---|---|---|---|---|
| 1 | Main의 직접 저장소 수정을 runtime이 거절 | Claude Code PreToolUse 차단, Codex read-only sandbox | [Main policy](TEAM-ORCHESTRATION-MAIN-POLICY.md)는 프롬프트·스킬 계약 | 소스 | 정합 | [#337](https://github.com/eightmm/oh-my-setting/pull/337): Claude Code edit 도구 | ① Bash를 통한 쓰기 판정 ② Codex·AGY Main(상태 기록은 허용하는 제한 기동) ③ Room에 기록되는 1회 예외 |
| 2 | Native 배정·결과 bridge와 공통 task card | Claude Code agent teams의 공유 task list, Gas Town Beads | Plan에 `depends`·`ready`·claim·lease가 있음. native `--orchestrator --task`는 task/lease/result 바인딩을 만들지 않음(#285 P0-B) | 소스 | 정합(새 task DB는 비범위) | 최근 main의 native Lead 작업 | P0-B 단계 1(claim과 전송 intent)부터 |
| 3 | 배치 land 후보와 실패 시 bisect | Gas Town Refinery, Graphite 배치 | land는 통합 대상별 직렬 | 소스 | 조건부(GitHub merge queue 의존과 보호 규칙 우회는 비범위) | | ① 수락된 후보 N개를 한 통합 후보로 합쳐 한 번 검증 ② 실패 시 절반씩 나눠 원인만 보류 |
| 4 | land의 CI 대기 시간 | merge queue 계열의 병렬 CI | 전체 run이 성공해야 설치 증거가 됨. 2026-11-22 full run 한 건: Ubuntu job 5–8분, macOS job은 runner 대기 약 16분 뒤 8–13분, 합계 32분 | 소스(workflow, land CI 판정, 실측 1건) | 정합(CI와 설치 증거 요구는 유지) | #327(macOS 간헐 실패) | ① macOS 대기를 여러 run에서 계측 ② macOS 증거를 land가 기다리는 방식 결정. focused 분할은 병목이 아님 |
| 5 | Room 메시지를 MCP로 노출 | Claude Code agent teams mailbox | Room은 CLI에 있음. MCP에는 inbox·handoff만 있음 | 소스 | 조건부(AH-09의 named workflow로 한정) | #313 | Room 읽기·게시·ack 도구와 권한 검사 |
| 6 | OMS 측 하드 예산 집행 | 확실한 선례 없음 | soft admission만 있고 Team view는 hard cap을 unsupported로 표시 | 소스 | 조건부(provider 지출 상한으로 표현하지 않음) | | 상한 초과 시 새 위임을 거절하는 opt-in 정책 |
| 7 | 호출별 token 사용량을 Board에 표시 | — | 추적된 호출 합계는 있음. 선택한 호출·native child 단위 귀속은 미완 | 소스(이슈) | 정합 | #279 | 이슈의 acceptance 그대로 |

### A: 하네스 범위와 복구 (8–18)

| 순위 | 공백 | 가진 도구 | OMS 현황 | 확인 | 관계 | 진행 | 다음 PR 단위 |
|---|---|---|---|---|---|---|---|
| 8 | Worker sandbox 기본 적용과 네트워크 정책 | Codex 커널 sandbox | `OMS_WORKER_SANDBOX`는 opt-in | 소스(존재), 보고(기본값) | 조건부(AH-06은 별도 승인) | #313 | hook이 없는 transport부터 |
| 9 | native child 역할의 실효 권한 검증 | — | Codex 역할 파일의 sandbox 설정은 read-only를 보장하지 않음 | 소스(이슈) | 정합 | #277 | provider·계정별 fixture |
| 10 | 결정적 정체 감지와 attention 승격 | Gas Town Problems 뷰 | fail-ledger 정체 기록과 attention 알림 | 보고 | 조건부(AI 감독 계층과 자동 kill은 [비범위](CONTROL-PLANE-EVOLUTION.md#7-착수하지-않을-작업)) | | LLM 없이 무출력·반복 실패 판정 → attention |
| 11 | turn 단위 되돌리기 | Claude Code `/rewind` | 수동 `oms checkpoint`가 있음. Worker turn마다 자동 저장은 없음 | 소스 | 정합 | | attempt의 turn별 ref와 복원 |
| 12 | 충돌에서 integrator 배정까지 자동 연결 | Refinery, Graphite 자동 rebase | `integrator` 역할과 `oms land conflicts` | 소스 | 정합 | | 충돌을 integrator 작업 제안으로 기록 |
| 13 | ACP client | ACP 지원 에디터·에이전트 | 설계만([research follow-ups](RESEARCH-FOLLOWUPS.md)) | 소스 | 조건부 | #313(AH-04) | 선택형 transport |
| 14 | 범용 원격·격리 실행 profile | Codex cloud, Claude Code cloud 세션 | SSH·Slurm·Apptainer CPU는 있음. generic `remote`·`isolated`는 unavailable | 소스 | 조건부 | | SSH Worker transport 연결 |
| 15 | 외부 이벤트 trigger | Agent HQ 이슈 이벤트 | `oms tick schedule`은 승인된 autopilot의 시간 주기만 줌 | 소스 | 조건부(상주 서버는 비범위) | | CI 실패·리뷰 요청 관측을 검토 대기 작업으로 |
| 16 | Codex hook 실패 진단의 남은 부분 | — | callback trace sidecar가 있음. 관리 callback에 닿지 않은 provider 측 실패 구분이 남음 | 소스(이슈) | 정합 | #271 | 이슈의 acceptance 그대로 |
| 17 | 임의 CLI adapter 확대 | claude-squad, Vibe Kanban | custom adapter 경계 | 소스 | 조건부 | #319, #333, #334 | 진행 중인 conformance 뒤 |
| 18 | 이슈 라벨 polling과 자동 intake | Agent HQ | [`intent draft --from-issue`](INTENT-SOURCE-INTAKE.md)는 이슈 한 건 | 소스 | 조건부 | | 라벨 조회 → 검토 대기 draft |

### B: 뒤로 미뤄도 되는 것 (19–32)

| 순위 | 공백 | OMS 현황 | 확인 | 관계 | 제안 |
|---|---|---|---|---|---|
| 19 | Stacked PR land | 없음 | 소스 | 조건부(3위 뒤) | 순서 있는 후보 묶음 |
| 20 | 규칙을 Cursor·Copilot 포맷으로 내보내기 | CLAUDE.md·AGENTS.md 연결만 | 소스 | 조건부 | 생성 명령 |
| 21 | 리뷰 결과를 PR 코멘트로 게시 | `draft-pr comments`는 읽기만 | 소스 | 조건부(publication 승인) | 승인된 finding만 게시 |
| 22 | PR 코멘트에서 후속 작업 | `--land --auto-repair` opt-in | 소스 | 조건부(넓은 자동 채택은 deferred) | 관측 범위 확대 |
| 23 | 같은 작업 paired 평가 세트 | `harness-eval --repeat` | 소스 | 조건부 | 모델 교체 전후 비교 |
| 24 | 같은 구현의 N회 병렬 시도 | problem campaign | 소스 | 조건부 | campaign의 짧은 진입점 |
| 25 | 실험 추적 도구로 export | run ledger·capsule | 보고 | 조건부 | capsule → MLflow |
| 26 | GPU 가용량을 본 admission | slurm-lint의 GPU-hours 경고 | 소스 | 조건부 | 참고값으로만 |
| 27 | 세션을 다른 호스트에서 이어받기 | handoff, capsule | 소스 | 조건부 | 원격 resume |
| 28 | Main 교체 | `swap_main_host` unsupported | 소스 | 조건부 | CC-09 |
| 29 | usage ledger 증분 저장 | 전체 history를 다시 씀 | 소스 | 조건부(계측 먼저) | 계측 → compaction |
| 30 | 작업별 dev server 미리보기 | 없음 | 소스 | 조건부 | worktree별 포트 |
| 31 | CI 안에서 에이전트를 실행하는 template | 프로젝트용 CI template만 | 소스 | 조건부(CI는 작게) | 명시적 trigger template |
| 32 | 기간별 감사 요약 | command guard audit, attempt bundle | 소스 | 조건부 | 요약 명령 |

## 범위 결정이 먼저인 것

구현 PR보다 [feature scope](FEATURE-SCOPE.md) 수정이 먼저다.

| 항목 | 기존 결정 |
|---|---|
| 양방향 원격 승인(Slack·Telegram) | web·mobile 제어는 MVP 전제에서 제외. 단방향 `OMS_PANEL_ATTENTION_CMD`는 있음 |
| 로컬 web 보드 | 상주 HTTP 서버는 비범위. 오프라인 artifact view는 있음 |
| OpenTelemetry export | `otel-export`는 쓰이지 않아 제거됨. 이름 대응표는 남아 있음 |
| AI 감독 에이전트 | Mayor/Deacon/Witness 계층은 비범위 |
| 팀·다중 사용자 설정과 RBAC | 1인 로컬 전제 |
| native Windows | 지원 종료 |

## 이미 있는 것 (순위에서 제외)

첫 판이 공백으로 적었지만 재검증에서 존재를 확인한 항목이다.

| 첫 판의 항목 | 확인한 내용 |
|---|---|
| 패널 입력 반응성 | #267 완료 |
| 계정 바인딩과 routing 이유 | #269 완료 |
| council 좌석 설정화 | #273 완료, #318 진행 |
| Room 헤더와 주간 사용량 | #274 완료 |
| 리뷰 settle 사유 보존 | #275 완료 |
| 문서 경로 drift | #295 완료 |
| Codex Lead 긴 배정 가시성 | #322 완료 |
| 작업 카테고리로 모델 고르기 | `--workload routine`·`frontier`, account overflow, 명시적 model fallback |
| ML checkpoint 정책 검사 | `oms run checkpoints`가 latest·best만 남았는지 검사. template이 val 주기를 안내 |
| worktree·branch 정리 목록 | `oms cleanup`이 dry-run으로 worktree와 `oms/*` branch를 보고 |
| plugin marketplace | `.claude-plugin/marketplace.json` |
| flaky 확인 명령 | `oms ci-status flaky` |

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
