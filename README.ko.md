# oh-my-setting

서버마다 agent 규칙, skills, 프로젝트 템플릿을 같은 방식으로 맞춘다.

[English](README.md)

## 로컬 우선 Agent

oh-my-setting은 agent 작업을 기본적으로 로컬과 shell에서 보이게 유지한다:

- MCP server, app connector, plugin connector tool은 사용하지 않는다.
- 로컬 파일, shell command, `git`, `gh` CLI를 우선 사용한다.
- Multi-agent review도 로컬 기반으로만 수행한다: Codex, Claude Code, Antigravity CLI.
- 로컬 multi-agent 도구가 없으면 single-agent review로 진행하고 한계를 명시한다.

설치 이후의 모든 사용은 coding agent와의 대화로 이루어진다. 설치된 규칙과
스킬이 agent에게 어떤 로컬 스크립트를 실행할지 알려주므로, 스크립트를 직접
호출할 일은 없다.

## 빠른 시작

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

그 다음 coding agent를 열고 둘 중 하나를 말한다:

```text
여기서 새 프로젝트 시작하자.                    # 빈 디렉토리: 인터뷰 -> PROJECT.md -> 템플릿 -> skeleton -> doctor
oh-my-setting 프로젝트 템플릿 적용해줘.         # 기존 repo
```

새 프로젝트 시작은 전체 플로우가 채팅 안에서 돈다: spec 인터뷰, `PROJECT.md`,
템플릿, 안전한 skeleton, doctor 검증 — shell에 직접 칠 명령이 없다. 더 자세한
시작 문구는 [Agent 시작 문구](#agent-시작-문구) 참고.

## 설치

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

기존 설치는 최신 checkout으로 갱신한 뒤 설정을 계속 진행한다.
처음 nvm을 설치한 뒤 새 CLI를 못 찾으면 새 shell을 연다.
Installer가 관리하는 Node는 Node 20 이상을 사용한다.

옵션:

```bash
OH_MY_SETTING_STAR_PROMPT=0       # GitHub star prompt 생략
OH_MY_SETTING_GENERATE_MACHINE=0  # 머신 스냅샷 생략
OH_MY_SETTING_GENERATE_SLURM=0    # Slurm 스냅샷 생략
OH_MY_SETTING_INSTALL_TOOLS=0     # Node/uv/agent CLI 설치 생략
OH_MY_SETTING_REQUIRE_TOOLS=0     # CLI 누락으로 doctor 실패 처리 안 함
OH_MY_SETTING_DIR=/path/to/dir    # 설치 경로
```

로컬 installer를 실행할 때는 `--no-star`가 `OH_MY_SETTING_STAR_PROMPT=0`과 같다.

설치 경로:

```text
~/.codex/AGENTS.md
~/.claude/CLAUDE.md
~/.gemini/AGENTS.md
~/.oh-my-setting/local/machine.md
```

`~/.gemini/AGENTS.md`는 Antigravity의 global customizations root다. 스킬도
`~/.gemini/antigravity/skills/`에 링크되어 세 agent 모두 같은 규칙과 스킬을
읽는다.

상태 확인, 업데이트, 오래된 skill 링크 정리는 agent에게 요청한다:

```text
oh-my-setting 설치 상태 확인해줘.
oh-my-setting 업데이트하고 doctor 다시 돌려줘.
오래된 oh-my-setting skill 링크 정리하고 $skill 중복 고쳐줘.
oh-my-setting skill doctor 돌려줘.
```

설치 이후의 일반 사용은 chat-first다. 사용자는 요청만 하고, agent가
`status.sh`, `doctor.sh`, `cleanup.sh`, `skill-doctor.sh` 같은 로컬 스크립트를
실행한다. 스크립트 경로는 투명성과 복구용으로 문서화하며, 사용자가 직접 실행할
것을 기대하지 않는다.

## Shared Harness Memory

Codex, Claude Code, Antigravity는 제품별 memory 저장소가 서로 다르다.
oh-my-setting은 그 private store를 직접 합치지 않고, 세 agent가 모두 읽는
harness 소유 memory 파일을 둔다.

- 프로젝트 memory: `.oms/memory/shared.md`
- 전역 memory: `~/.oh-my-setting/local/agent-memory.md`
- 안전장치: credential, private key, 로컬 머신 경로, cluster detail,
  프로젝트 private path처럼 보이는 note는 append 단계에서 거부한다.
- 반드시 지켜야 하는 규칙은 여전히 `AGENTS.md`, repo 문서, script, hook에
  둔다. Shared memory는 soft recall이다.

agent에게 이렇게 요청한다:

```text
이 repo에서는 완료 주장 전에 scripts/check.sh fast 돌린다는 걸 기억해줘.
이 repo shared harness memory 보여줘.
memory에 넣기 전에 저장할 내용 요약해줘.
```

provider 하나만 호출할 수도 있다. agent가 읽기 전용 호출과 격리된 쓰기 위임 중 고른다:

```text
Codex만 불러서 이 계획 평가해줘.
Claude Code만 불러서 이 focused fix 구현하고 patch로 받아와줘.
Antigravity만 불러서 이 구현 방향 검토해줘.
```

agent는 내부적으로 `agent-memory.sh`, `agent-run.sh`를 실행한다. `agent-run.sh`는
읽기 전용 질문은 `agent-call.sh`, 쓰기 작업은 `multi-agent-delegate.sh`로 라우팅해
격리된 git worktree에서 patch로 회수한다.

## Multi-Agent 워크플로

기본값이 반대인 워크플로 두 개:

| | review | ask |
|---|---|---|
| 목적 | diff 검증 (게이트) | 질문 탐색 (자문) |
| repo context | 기본 첨부 | 기본 생략 (요청 시 첨부) |
| 응답 계약 | Findings / Risks / Missing tests / Recommendation | Answer / Tradeoffs / Risks / Recommendation |
| 고유 기능 | base ref 기준 리뷰, ML 체크리스트, 합성, debate | debate |
| 실패 의미 | 리뷰 게이트 실패 (변경 차단) | 독립 의견 수 부족 |

merge나 훈련 전에는 review, 무엇을 만들지 결정할 때는 ask.

예시 문구:

```text
현재 diff를 multi-agent review로 검토해줘.
이 branch를 origin/main 기준으로 multi-agent review 해줘.
이 diff에 ML pre-training 리뷰 게이트 돌려줘.
세 모델에게 물어봐줘: 이 프로젝트는 vector DB와 pgvector 중 뭐가 맞을까?
debate 1라운드로 세 모델에게 물어봐줘: 이 프로젝트에서 RAG와 fine-tuning 비교.
```

ML 게이트는 GPU 시간 태우기 전에 침묵형 ML 버그(leakage, split 무결성, loss,
eval mode, 재현성, DDP)를 diff에서 검사한다. 체크리스트는 모든 reviewer
프롬프트에 자동 주입된다.

Provider는 병렬 실행되며 provider별 timeout이 적용된다
(`OMS_MULTI_AGENT_TIMEOUT`, 기본 `5m`). provider별 artifact와
`_synthesis-*.md` 종합본이 review는 `.oms/artifacts/review/`, ask는
`.oms/artifacts/ask/`에 저장된다. 종합본은 기본적으로 모델이 작성한
합성(Consensus/Must-fix/Optional/Disagreement)이다. debate(1-3라운드)는 각
provider가 다른 모델들의 직전 답변을 보고 증거 기반으로 비판하고 자기 입장을
수정한 뒤 합성한다 — 고위험 diff에서 false positive 제거에 유용. 라운드별
artifact는 `*-rN.md`로 저장되고, 비용은 provider × (1+라운드) 호출로 늘어나며
1-2라운드가 보통 적정선. debate 라운드는 답변만 교환한다 — repo context는
1라운드 프롬프트에만 붙는다. sanitized diff/status context가 로컬 Codex,
Claude Code, Antigravity CLI로 전달되며, secret path와 secret-like 추가
라인은 외부 review 전에 제외된다.

다른 agent에게 쓰기 작업 위임:

```text
codex에게 위임해줘: scripts/train.py에 입력 검증 추가.
검증은 `uv run pytest tests/`로.
```

worker는 격리된 git worktree에서 실행되며 메인 트리 수정·commit·push 불가.
artifact(로그 + HEAD 기준 `.patch`)는 `.oms/artifacts/delegate/`에 저장된다.
호스트 agent가 대화 컨텍스트로부터 brief(Task/Context/Constraints/Files/
Success criteria)를 작성하고, 회수된 patch를 같이 리뷰한 뒤 승인 후에만
적용한다.

## 검증·실험 도구

ML 프로젝트는 `scripts/check.sh` 검증 계약을 받는다(ml 템플릿이 스캐폴딩).
`fast`는 CPU 전용 60초 미만 — agent가 완료 주장 전 실행. `gpu`는 짧은 GPU
smoke로 Slurm 머신에서는 srun으로 감싼다. 위임된 worker는 계약이 있으면
worktree 안에서 `check.sh fast`를 기본 실행한다.

실험은 run ledger를 통해 실행 — 모든 agent가 "뭘 이미 시도했는지" 기억:

```text
이 훈련을 run ledger 통해서 실행해줘, note는 "lr sweep".
최근 ledger 10개 보여줘.
```

행(git SHA, dirty-diff hash, Slurm job id, exit code, duration)이
`docs/EXPERIMENTS.jsonl`에 누적된다. 명령줄이 그대로 기록되니 인자에 secret
넣지 말 것.

긴 훈련/Slurm 로그는 raw로 붙이지 말고 digest 요청:

```text
outputs/train.log digest 해줘.
Slurm job 12345랑 로그 digest 해줘.
```

## 프로젝트 적용

프로젝트 안에서 agent에게 요청한다:

```text
oh-my-setting 프로젝트 템플릿 적용해줘 (자동 감지).
oh-my-setting ml 템플릿 적용해줘.        # 또는: general, slurm
oh-my-setting 프로젝트 규칙 제거해줘.
oh-my-setting project doctor 돌려줘.
```

적용 시 동작:

- `AGENTS.md`, `CLAUDE.md`에 managed block 추가/갱신
- `PROJECT.md` 없으면 생성
- `ml` 프로젝트는 표준 ML 문서 템플릿을 `docs/`에 스캐폴딩 (기존 파일은 덮어쓰지 않음)
- `ml` 프로젝트는 `.gitignore`에 `data/`, `outputs/`, `checkpoints/`, `wandb/`, `runs/`, `.venv/` 보장
- managed block 밖의 기존 내용은 덮어쓰지 않음
- Slurm 머신의 ML 프로젝트는 `ml` + 별도 `slurm` 규칙 적용

제거는 managed block만 삭제한다. `PROJECT.md`와 스캐폴딩된 `docs/` 파일은
사용자 내용이 있을 수 있어 의도적으로 남겨둔다.

project doctor는 `AGENTS.md`/`CLAUDE.md` managed block이 서로 다르거나, 현재
템플릿 대비 오래됐거나, `PROJECT.md`가 없으면 실패한다. draft `PROJECT.md`,
ML 문서 스캐폴드 누락, `.gitignore` 항목 누락은 경고. oh-my-setting 업데이트
후 실행하면 템플릿 재적용이 필요한 프로젝트를 찾을 수 있다.

## Agent 시작 문구

새 프로젝트:

```text
Use the local oh-my-setting project workflow. Do not code yet.

Start a new project: interview me to fill PROJECT.md, and after I confirm it,
bootstrap the project (template, safe skeleton, doctor) in one go.

Success criteria:
- clarify goal, users, non-goals, interface, data, paths, commands, risks, and verification
- write PROJECT.md and wait for my confirmation
- after confirmation: apply the matching template, scaffold the safe skeleton, run the project doctor
- wait for separate confirmation before feature code or anything beyond the confirmed spec
- report template type, changed files, and doctor result
```

기존 프로젝트:

```text
Read local project files first. Start by inspecting AGENTS.md/CLAUDE.md,
PROJECT.md if present, README, pyproject/configs, and git status. Do not edit yet.

Goal: understand this existing project and propose the smallest safe next step
to onboard or continue it with oh-my-setting rules.

Report:
- project type and current structure
- setup/test/run commands you can infer
- missing or draft PROJECT.md fields
- risks before editing
- recommended next prompt
```

## ML 프로젝트

빈 디렉토리를 만들고, 그 안에서 agent를 열고 말한다:

```text
여기서 새 ML 프로젝트 시작하자.
```

기대되는 agent 흐름:

1. 인터뷰
2. `PROJECT.md` 작성/확인
3. ml 템플릿 적용 + 안전한 skeleton 스캐폴딩 + project doctor
4. 확인 후 코드 작성

ML 프로젝트 기본:

- `uv sync`
- local `.venv`
- `uv run ...`
- 머신 스냅샷은 compute, GPU/CUDA, Slurm, memory, environment 차이가 작업에 영향을 줄 때만 참고

ml 템플릿은 표준 문서 템플릿도 `docs/`에 스캐폴딩한다 (`DATA.md`, `MODEL.md`,
`TRAINING.md`, `EVALUATION.md`, ...). 프로젝트가 구체화되면서 채워 나가면
되고, 기존 파일은 덮어쓰지 않는다.

## 로컬 스냅샷

installer가 생성하며, 다시 만들 때는 agent에게 요청한다:

```text
머신 스냅샷 다시 생성해줘.
Slurm cluster 스냅샷 다시 생성해줘.        # raw 출력 포함이 필요하면 같이 말한다
```

저장 위치:

```text
~/.oh-my-setting/local/machine.md
~/.oh-my-setting/custom-skills/slurm-hpc/references/cluster.generated.md
```

머신 스냅샷에는 Codex, Claude Code, Antigravity, `gh` 같은 로컬 agent CLI
경로도 감지되면 함께 기록된다.

## 연결 해제

oh-my-setting symlink를 제거하고 최신 `*.backup.TIMESTAMP` 파일이 있으면
자동으로 복원한다:

```bash
~/.oh-my-setting/scripts/unlink.sh
```

현재 oh-my-setting checkout을 가리키는 symlink만 제거한다. 일반 파일이나
다른 곳을 가리키는 symlink는 건드리지 않는다.

연결 해제 미리보기:

```bash
OH_MY_SETTING_DRY_RUN=1 ~/.oh-my-setting/scripts/unlink.sh
```

## 제거

symlink 제거(=`unlink.sh`)와 옵션으로 checkout 디렉토리 삭제:

```bash
~/.oh-my-setting/scripts/uninstall.sh           # unlink만
~/.oh-my-setting/scripts/uninstall.sh --purge   # checkout도 삭제 (확인 prompt)
~/.oh-my-setting/scripts/uninstall.sh --purge --yes --dry-run
```

`--purge`는 `$HOME`이나 `/` 삭제를 거부한다. `install-tools.sh`가 깐 nvm, uv, CLI 바이너리는 제거하지 않는다.

## 안전

토큰, API key, private data, 생성된 cluster 상세, 로컬 머신 상세는 commit하지 않는다.

## Star

도움이 됐다면:

```bash
gh api --method PUT /user/starred/eightmm/oh-my-setting
```
