# oh-my-setting

Codex, Claude Code, Antigravity에 같은 규칙·스킬·워크플로를 모든 머신에
깔아주는 설정. 설치 후 모든 사용은 coding agent와의 대화로 이루어진다 —
터미널에 직접 칠 명령이 없다.

[English](README.md)

## 설치

유일한 shell 단계:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

새로 설치된 CLI를 못 찾으면 새 shell을 한 번 연다. 이후는 전부 agent가 실행한다.

## 시작

아무 디렉토리에서나 — 빈 디렉토리든, 진행 중이던 코드든 — coding agent를
열고 말한다:

```text
이 프로젝트 시작하자.
```

agent가 상태를 감지해서 라우팅한다:

- 빈 디렉토리 → spec 인터뷰 → `PROJECT.md` → 템플릿 → 안전한 skeleton → doctor
- 기존 repo → 코드 먼저 읽고 템플릿 적용, `PROJECT.md`는 코드에서 채우고
  빈칸만 인터뷰
- 진행 중 프로젝트 → `PROJECT.md` 읽고 doctor 실행, 상태와 다음 스텝 보고

spec이 확정되기 전에는 코드를 짜지 않는다.

## 이렇게 말하면 된다

프로젝트:

```text
이 프로젝트 시작하자.
oh-my-setting ml 템플릿 적용해줘.        # 또는: general, slurm
oh-my-setting project doctor 돌려줘.
```

리뷰·자문 (세 로컬 모델 병렬):

```text
현재 diff를 multi-agent review로 검토해줘.
이 diff를 gated multi-agent review로 pass/fail 판정해줘.
이 diff에 ML pre-training 리뷰 게이트 돌려줘.
Claude에 직접 호출하지 말고 이 diff용 review prompt만 export해줘.
이 Claude 답변을 artifact index에 import해줘.
debate 1라운드로 세 모델에게 물어봐줘: vector DB와 pgvector 중 뭐가 맞을까?
codex에게 위임해줘: scripts/train.py에 입력 검증 추가.
```

재사용 코드 source:

```text
내 GitHub profile 보고 재사용할 equivariant GNN 코드 찾아봐.
flowfrag/equivariant.py를 flowfrag-equivariant로 등록해줘.
flowfrag-equivariant를 이 프로젝트로 가져와줘.
```

실험 (ML):

```text
이 훈련을 run ledger 통해서 실행해줘, note는 "lr sweep".
이 eval의 metrics.json을 ledger row에 기록해줘.
최근 ledger 10개 보여줘.
Slurm job 12345 끝날 때까지 기다렸다가 digest해서 보고해줘.
이 분자 데이터셋 split에 leakage 없는지 훈련 전에 확인해줘.
이 실험을 run 전에 가설주도 형태로 정리해줘.
metric val_auc/scaffold 기준 registered research run으로 실행해줘.
이 가설이랑 실험 설계를 훈련 전에 세 모델로 공격해줘.
```

메모리·핸드오프:

```text
이 repo에서는 완료 주장 전에 scripts/check.sh fast 돌린다는 걸 기억해줘.
이 repo pin으로 저장해줘: 현재 작업은 dataloader 리팩터.
active task packet 보여줘.
```

유지보수:

```text
oh-my-setting 설치 상태 확인해줘.
oh-my-setting 업데이트하고 doctor 다시 돌려줘.
skill picker 중복 항목 고치고 legacy oh-my-setting 링크 정리해줘.
oh-my-setting 연결 해제해줘.              # 또는: 완전히 제거해줘
```

## 구성 요소

아래 전부 코딩 agent가 필요할 때 알아서 호출해 쓴다 — 채팅으로 의도만 말하면
agent가 맞는 스크립트나 skill을 고른다. 직접 실행할 일은 없다.

| 영역 | 기능 | 하는 일 |
|---|---|---|
| 프로젝트 | Start router + spec 인터뷰 | 빈 디렉토리/기존 repo/진행 중 상태를 감지하고 단계별 인터뷰로 `PROJECT.md`를 확정한 뒤에만 코드 작성 |
| 프로젝트 | 템플릿 (`apply-project-template.sh`) | general/ml/slurm managed rule block; ml은 docs 스캐폴드, `check.sh` 검증 계약, `ml_smoke.py` one-batch 계약까지 |
| 프로젝트 | Project doctor (`project-doctor.sh`) | 세 agent가 같은 규칙·spec 상태·스캐폴드를 보는지 검증; ML 구조 drift 경고 (root에 흩어진 파일, git 추적 데이터, `src/` 레이아웃 누락) |
| Multi-agent | Review (`multi-agent-review.sh`) | 세 로컬 모델이 diff를 병렬 리뷰; ML pre-training 게이트(`--ml`), debate 라운드, finding별 verdict, `--gate` one-command pass/fail verdict |
| Multi-agent | Ask (`multi-agent-ask.sh`) | 같은 질문을 세 모델에 보내 독립 의견 수집; debate 라운드와 가설 design-attack 프리셋 |
| Multi-agent | Delegate (`multi-agent-delegate.sh`) | 격리된 git worktree에서 write 작업 실행·검증 후 리뷰 가능한 patch 회수; `--apply`는 clean tree에서만 |
| Multi-agent | 단일 agent 라우터 (`agent-run.sh`) | 프롬프트 하나를 한 provider로: 읽기 질문은 call, write 작업은 delegate worktree로 라우팅 |
| Multi-agent | Export/import handoff (`--export-only`, `import-agent-result.sh`) | 세션에서 다른 agent CLI를 직접 호출할 수 없을 때 provider prompt를 로컬 artifact로 기록; 답변은 같은 artifact index로 import되며 동일한 outbound 민감정보 게이트를 통과 |
| Multi-agent | Change guard (`change-guard.sh`) | live dirty tree를 snapshot하고, 기존 dirty file을 건드리거나 선언한 path scope를 벗어나면 경고 |
| Multi-agent | Patch admission (`patch-admit.sh`) | delegated patch를 임시 worktree에 적용해 applies cleanly → shell/python/json syntax → verification contract ladder로 ADMIT/REJECT |
| Multi-agent | Artifact index (`artifact-index.sh`) | 모든 cross-agent 실행이 `.oms/artifacts/` + JSONL index에 기록 — list, latest, prune |
| Multi-agent | 안전장치 (내장) | 외부 CLI 호출 전 outbound prompt scrub (credential, key, 머신/cluster 상세 감지 시 호출 차단); 주입 context는 fence; diff와 debate quote는 sanitize |
| 코드 source | Registry (`code-source.sh`) | 신뢰하는 재사용 파일(개인 model block 등) 로컬 registry; 이름으로 현재 프로젝트에 fetch |
| 코드 source | GitHub fetch (`github-source.sh`) | `gh` 경유 profile/discover/fetch; 기본 overwrite 금지, provenance는 `.oms/code-sources.jsonl`에 기록 |
| 실험 | Run ledger (`run-ledger.sh`) | 학습 실행 wrapper: pre-flight `check.sh` 게이트, 중복 실행 경고, run당 한 줄씩 `docs/EXPERIMENTS.jsonl`에 기록, `--metrics`로 eval 스칼라 저장 |
| 실험 | Research runner (`research-runner.sh`) | registered research run: 가설·사전등록 metric·baseline을 launch 전에 기록, 종료 후 verdict |
| 실험 | Job digest (`job-digest.sh`) | 긴 로그나 Slurm job을 compact digest로 압축; `--wait`로 job 종료까지 대기 |
| 실험 | ML context (`agent-ml-context.sh`) | spec·ledger tail·config를 담은 compact ML digest를 cross-agent 호출에 첨부 |
| 실험 | Cluster 스냅샷 | 머신 스냅샷과 생성형 Slurm reference skill로 agent가 로컬 하드웨어·큐를 파악 |
| 실험 | 도메인 skill | `ml-training`(optimizer/LR/DDP 기본값), `chem-bio-ml`(split, leakage, metric), `research-method`(falsifiable 가설 루프), `slurm-hpc` |
| 메모리 | 공유 메모리 (`agent-memory.sh`) | `.oms/memory/`의 compact cross-agent 사실 저장; 민감 내용은 기록 시점에 거부 |
| 메모리 | Task 핸드오프 (`agent-task.sh`) | `.oms/task/current.md` active task packet으로 세 agent 누구든 같은 작업을 이어감; close 시 결론이 메모리로 승격 |
| 유지보수 | Install / update / doctor | 한 줄 설치로 같은 규칙·skill을 세 agent에 symlink; doctor가 링크·도구·manifest sync 검증 |
| 유지보수 | Skill 위생 (`skill-doctor.sh`, `cleanup.sh`) | 세 agent의 skill picker 중복/누락 항목 진단; cleanup은 알려진 legacy oms/backup symlink만 제거(기본 dry-run, 일반 파일·플러그인은 건드리지 않음) |
| 유지보수 | Auto-update (`auto-update.sh`) | systemd timer 또는 cron; check-only 또는 apply 모드 (fast-forward + relink) |
| 유지보수 | Backup / unlink / uninstall | 변경 전 agent 설정 스냅샷; 교체했던 것을 복원하는 깔끔한 제거 |

## 참고

- 로컬 우선: MCP server, app connector, plugin connector tool 사용 안 함.
- 토큰, API key, private data, cluster/머신 상세는 commit하지 않는다.
- agent가 실행하는 스크립트는 `~/.oh-my-setting/scripts/`에 있다 — 투명성과
  복구용 문서화일 뿐, 직접 실행할 일은 없다.

## Star

도움이 됐다면: [github.com/eightmm/oh-my-setting](https://github.com/eightmm/oh-my-setting)
