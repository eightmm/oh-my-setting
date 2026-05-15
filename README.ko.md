# oh-my-setting

서버마다 agent 규칙, skills, 프로젝트 템플릿을 같은 방식으로 맞춘다.

[English](README.md)

## 설치

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

검토 후 실행:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh -o /tmp/oh-my-setting-install.sh
less /tmp/oh-my-setting-install.sh
bash /tmp/oh-my-setting-install.sh
```

도구 설치 없이 설정만 연결:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | \
  OH_MY_SETTING_INSTALL_TOOLS=0 bash
```

## 로컬 우선 Agent

- MCP server, app connector, plugin connector tool은 사용하지 않는다.
- 로컬 파일, shell command, `git`, `gh` CLI를 우선 사용한다.
- Multi-agent review도 로컬 기반으로만 수행한다: Codex, Claude Code, Gemini, Pi CLI.
- 로컬 multi-agent 도구가 없으면 single-agent review로 진행하고 한계를 명시한다.

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

- `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`에 managed block 추가/갱신
- `PROJECT.md` 없으면 생성
- managed block 밖의 기존 내용은 덮어쓰지 않음
- Slurm 머신의 ML 프로젝트는 `ml` + 별도 `slurm` 규칙 적용

제거:

```bash
~/.oh-my-setting/scripts/remove-project-template.sh all .
```

감지만:

```bash
~/.oh-my-setting/scripts/detect-project-style.sh .
```

## ML 시작

```bash
mkdir my-project
cd my-project
~/.oh-my-setting/scripts/apply-project-template.sh ml .
```

그 다음 agent에게 프로젝트 시작을 요청한다. 기대 동작:

1. 안전한 skeleton만 생성
2. 인터뷰
3. `PROJECT.md` 작성/확인
4. 확인 후 코드 작성

ML 프로젝트 기본:

- `uv sync`
- local `.venv`
- `uv run ...`
- 머신 스냅샷: `~/.oh-my-setting/local/machine.md`

## 로컬 스냅샷

머신 스펙:

```bash
~/.oh-my-setting/scripts/write-machine-snapshot.sh
```

저장 위치:

```text
~/.oh-my-setting/local/machine.md
```

Codex, Claude Code, Gemini, Pi, `gh` 같은 로컬 agent CLI 경로도 감지되면
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

## 업데이트

```bash
cd ~/.oh-my-setting
git pull --ff-only
./scripts/link.sh
./scripts/write-machine-snapshot.sh
./scripts/doctor.sh
```

## 연결 해제

oh-my-setting symlink를 제거하고 최신 `*.backup.TIMESTAMP` 파일이 있으면
자동으로 복원한다:

```bash
~/.oh-my-setting/scripts/unlink.sh
```

현재 oh-my-setting checkout을 가리키는 symlink만 제거한다. 일반 파일이나
다른 곳을 가리키는 symlink는 건드리지 않는다.

먼저 확인:

```bash
OH_MY_SETTING_DRY_RUN=1 ~/.oh-my-setting/scripts/unlink.sh
```

## 설치 옵션

```bash
OH_MY_SETTING_INSTALL_TOOLS=0      # 설정만 연결
OH_MY_SETTING_GENERATE_MACHINE=0  # 머신 스냅샷 생략
OH_MY_SETTING_GENERATE_SLURM=0    # Slurm 스냅샷 생략
OH_MY_SETTING_DIR=/path/to/dir    # 설치 경로
```

## 설치 경로

```text
~/.codex/AGENTS.md
~/.claude/CLAUDE.md
~/.gemini/GEMINI.md
~/.pi/agent/AGENTS.md
~/.oh-my-setting/local/machine.md
```

## Secrets

토큰, API key, private data, 생성된 cluster 상세, 로컬 머신 상세는 commit하지 않는다.

## Star

도움이 됐다면:

```bash
gh repo star eightmm/oh-my-setting
```
