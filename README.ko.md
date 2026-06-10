# oh-my-setting

서버마다 agent 규칙, skills, 프로젝트 템플릿을 같은 방식으로 맞춘다.

[English](README.md)

## 로컬 우선 Agent

oh-my-setting은 agent 작업을 기본적으로 로컬과 shell에서 보이게 유지한다:

- MCP server, app connector, plugin connector tool은 사용하지 않는다.
- 로컬 파일, shell command, `git`, `gh` CLI를 우선 사용한다.
- Multi-agent review도 로컬 기반으로만 수행한다: Codex, Claude Code, Antigravity CLI.
- 로컬 multi-agent 도구가 없으면 single-agent review로 진행하고 한계를 명시한다.

## 빠른 시작

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
cd /path/to/project
~/.oh-my-setting/scripts/apply-project-template.sh auto .
```

그 다음 [Agent 시작 문구](#agent-시작-문구)에서 맞는 문구를 agent에 붙여넣는다.

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
~/.pi/agent/AGENTS.md
~/.oh-my-setting/local/machine.md
```

현재 설치 상태 확인:

```bash
~/.oh-my-setting/scripts/status.sh
```

로컬 checkout 업데이트 + symlink 재연결 + doctor 재실행:

```bash
~/.oh-my-setting/scripts/update.sh           # git pull + tools + link + doctor
~/.oh-my-setting/scripts/update.sh --no-tools --no-doctor
```

현재 repo의 tracked staged + unstaged diff를 세 모델로 review:

```bash
~/.oh-my-setting/scripts/multi-agent-review.sh \
  --prompt "Review the current diff for bugs, regressions, missing tests, and unsafe operations."
```

branch를 base ref 기준으로 review (PR 스타일):

```bash
~/.oh-my-setting/scripts/multi-agent-review.sh \
  --base origin/main \
  --prompt "Review this branch against origin/main."
```

Provider는 병렬 실행되며 provider별 timeout이 적용된다(`OMS_MULTI_AGENT_TIMEOUT`, 기본 `5m`). provider별 artifact와 `_synthesis-*.md` 종합본이 `.omc/artifacts/review/`에 저장된다. `--synthesize [codex|claude|antigravity]`(기본 `claude`)를 넘기면 단순 연결 대신 모델이 작성한 합성(Consensus/Must-fix/Optional/Disagreement)이 종합본에 추가된다. wrapper는 sanitized diff/status context를 로컬 Codex, Claude Code, Antigravity CLI로 보내며, secret path와 secret-like 추가 라인은 외부 review 전에 제외한다.

개념 질문을 세 모델에 묻기:

```bash
~/.oh-my-setting/scripts/multi-agent-ask.sh \
  --prompt "Compare RAG and fine-tuning tradeoffs for this project."
```

provider별 artifact와 `_synthesis-*.md` 종합본이 `.omc/artifacts/ask/`에 저장된다. `--repo-context`나 `--diff`를 넘기지 않으면 repo context는 붙이지 않는다.

## 프로젝트 적용

자동 감지:

```bash
~/.oh-my-setting/scripts/apply-project-template.sh auto .
```

직접 선택:

```bash
~/.oh-my-setting/scripts/apply-project-template.sh general .
~/.oh-my-setting/scripts/apply-project-template.sh ml .
~/.oh-my-setting/scripts/apply-project-template.sh slurm .
```

동작:

- `AGENTS.md`, `CLAUDE.md`에 managed block 추가/갱신
- `PROJECT.md` 없으면 생성
- `ml` 프로젝트는 표준 ML 문서 템플릿을 `docs/`에 스캐폴딩 (기존 파일은 덮어쓰지 않음)
- `ml` 프로젝트는 `.gitignore`에 `data/`, `outputs/`, `checkpoints/`, `wandb/`, `runs/`, `.venv/` 보장
- managed block 밖의 기존 내용은 덮어쓰지 않음
- Slurm 머신의 ML 프로젝트는 `ml` + 별도 `slurm` 규칙 적용

제거:

```bash
~/.oh-my-setting/scripts/remove-project-template.sh all .
```

제거는 managed block만 삭제한다. `PROJECT.md`와 스캐폴딩된 `docs/` 파일은 사용자 내용이 있을 수 있어 의도적으로 남겨둔다.

감지만:

```bash
~/.oh-my-setting/scripts/detect-project-style.sh .
```

모든 agent가 같은 프로젝트 규칙을 보는지 검증:

```bash
~/.oh-my-setting/scripts/project-doctor.sh .
```

`AGENTS.md`/`CLAUDE.md` managed block이 서로 다르거나, 현재 템플릿 대비
오래됐거나, `PROJECT.md`가 없으면 실패한다. draft `PROJECT.md`, ML 문서
스캐폴드 누락, `.gitignore` 항목 누락은 경고. `update.sh` 후 실행하면
템플릿 재적용이 필요한 프로젝트를 찾을 수 있다.

## Agent 시작 문구

새 프로젝트:

```text
Use the local oh-my-setting project workflow. Do not code yet.

Start a new project by creating only the safe skeleton, then interview me to
fill PROJECT.md before implementation.

Success criteria:
- clarify goal, users, non-goals, interface, data, paths, commands, risks, and verification
- write or update PROJECT.md with confirmed answers
- wait for confirmation before source code, dependency, API, data, or compute changes
- report changed files and checks
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

```bash
mkdir my-project
cd my-project
~/.oh-my-setting/scripts/apply-project-template.sh ml .
```

기대되는 agent 흐름:

1. 안전한 skeleton만 생성
2. 인터뷰
3. `PROJECT.md` 작성/확인
4. 확인 후 코드 작성

ML 프로젝트 기본:

- `uv sync`
- local `.venv`
- `uv run ...`
- 머신 스냅샷은 compute, GPU/CUDA, Slurm, memory, environment 차이가 작업에 영향을 줄 때만 참고

`apply-project-template.sh ml`은 표준 문서 템플릿도 `docs/`에 스캐폴딩한다
(`DATA.md`, `MODEL.md`, `TRAINING.md`, `EVALUATION.md`, ...). 프로젝트가
구체화되면서 채워 나가면 되고, 기존 파일은 덮어쓰지 않는다.

## 로컬 스냅샷

머신 스펙:

```bash
~/.oh-my-setting/scripts/write-machine-snapshot.sh
```

저장 위치:

```text
~/.oh-my-setting/local/machine.md
```

Codex, Claude Code, Antigravity, Pi, `gh` 같은 로컬 agent CLI 경로도 감지되면
함께 기록한다.

Slurm 스펙:

```bash
~/.oh-my-setting/scripts/generate-slurm-skill.sh
```

저장 위치:

```text
~/.oh-my-setting/custom-skills/slurm-hpc/references/cluster.generated.md
```

Slurm raw 출력까지 저장:

```bash
OH_MY_SETTING_SLURM_WRITE_RAW=1 ~/.oh-my-setting/scripts/generate-slurm-skill.sh
```

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
