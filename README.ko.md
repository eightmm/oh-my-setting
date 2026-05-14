# oh-my-setting

LLM agent 설정, skills, 프로젝트별 `AGENTS.md` 템플릿을 서버마다 동일하게 배포한다.

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

## 설치되는 것

- 전역 지침: `~/.codex/AGENTS.md`, `~/.claude/CLAUDE.md`, `~/.gemini/GEMINI.md`, `~/.pi/agent/AGENTS.md`
- custom skills: Codex/Claude/Pi/공용 skills 경로에 symlink
- 도구: Node, `uv`, Claude Code, Codex, Gemini CLI, Pi Agent, caveman
- Slurm 정보: `sinfo`가 있으면 자동 생성
- 출력 스타일: 연결된 모든 agent에 caveman-ultra 전역 규칙 적용

## 업데이트

```bash
cd ~/.oh-my-setting
git pull --ff-only
./scripts/link.sh
./scripts/doctor.sh
```

## 프로젝트별 적용

자동 감지:

```bash
~/.oh-my-setting/scripts/apply-project-template.sh auto /path/to/project
```

직접 선택:

```bash
~/.oh-my-setting/scripts/apply-project-template.sh general /path/to/project
~/.oh-my-setting/scripts/apply-project-template.sh ml /path/to/project
~/.oh-my-setting/scripts/apply-project-template.sh slurm-ml /path/to/project
```

동작:

- 기존 `AGENTS.md`/`CLAUDE.md`/`GEMINI.md`는 덮어쓰지 않는다.
- 파일 끝에 `oh-my-setting` managed block만 추가/갱신한다.
- 기본으로 `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`를 생성/갱신한다.
- `PROJECT.md`가 없으면 생성한다.
- 새 프로젝트/큰 작업은 agent가 인터뷰 -> `PROJECT.md` 작성/확인 -> 코딩 순서로 진행한다.
- Codex, Claude, Gemini, Pi에서 사용할 수 있다.

제거:

```bash
~/.oh-my-setting/scripts/remove-project-template.sh all /path/to/project
```

감지만:

```bash
~/.oh-my-setting/scripts/detect-project-style.sh /path/to/project
```

## 템플릿

- `general`: 일반 프로젝트
- `ml`: ML 프로젝트
- `slurm-ml`: Slurm/HPC 기반 ML 프로젝트

파일:

```text
templates/project-general-AGENTS.md
templates/project-ml-AGENTS.md
templates/project-slurm-ml-AGENTS.md
```

## 주요 스크립트

```text
install.sh                         전체 설치
scripts/install-tools.sh           Node/uv/agent CLI 설치
scripts/link.sh                    전역 설정 symlink
scripts/doctor.sh                  설치 상태 확인
scripts/backup.sh                  기존 설정 백업
scripts/apply-project-template.sh  프로젝트 지침 추가/갱신
scripts/remove-project-template.sh 프로젝트 managed block 제거
scripts/detect-project-style.sh    프로젝트 유형 감지
scripts/generate-slurm-skill.sh    로컬 Slurm 클러스터 정보 생성
```

## Slurm

설치 시 `sinfo`가 있으면 로컬 Slurm reference를 자동 생성한다. 수동 갱신:

```bash
~/.oh-my-setting/scripts/generate-slurm-skill.sh
```

`custom-skills/slurm-hpc/references/cluster.generated.md`를 생성한다.
생성된 클러스터 정보는 git에 올리지 않는다.

자동 생성을 끄려면:

```bash
OH_MY_SETTING_GENERATE_SLURM=0 \
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

## Secrets

토큰/API key는 commit하지 않는다. 필요한 변수 이름만 `.env.example`에 둔다.
