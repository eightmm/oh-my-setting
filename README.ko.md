<p align="center">
  <img src="docs/assets/banner.svg" alt="oh-my-setting — Codex, Claude Code, Antigravity를 위한 하나의 control plane" width="100%">
</p>

<p align="center">
  <a href="https://github.com/eightmm/oh-my-setting/actions/workflows/test.yml"><img src="https://github.com/eightmm/oh-my-setting/actions/workflows/test.yml/badge.svg?branch=main" alt="CI"></a>
  <img src="https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-0ea5e9" alt="Linux, macOS, Windows">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-22c55e" alt="MIT license"></a>
</p>

<p align="center"><a href="README.md">English</a></p>

**oh-my-setting**은 Codex, Claude Code, Antigravity가 같은 규칙, 상태, plan,
증거, handoff를 공유하며 한 프로젝트에서 일하게 한다. 누가 하던 일이든
다른 agent가 이어받을 수 있다.

터미널에서 **`oms`를 실행하면 공통 컨트롤 패널**이 열린다. 코덱스·클코를
선택 실행하고 공통 상태를 보거나 상대 제공자에게 조사·구현·검토를 맡길 수
있다. 설치된 tmux가 있으면 대화 옆에 상태 화면이 유지되고, 없으면 메뉴로
돌아오는 방식이다. [패널 사용법](docs/TERMINAL-PANEL.md).

하네스의 나머지 도구와 `.oms/` 상태, 업데이트·점검은 agent가 관리한다.
아래의 하네스 명령은 agent가 실행할 명령이다.

## 설치

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

| 호스트 | 필요한 것 |
|---|---|
| Linux, WSL (glibc) | Bash, curl. Git·tar·gzip·find는 없으면 설치한다. **Python 불필요.** |
| macOS | Bash, curl, Command Line Tools(`xcode-select --install`). **Python 불필요.** |
| Windows (Git Bash) | Git, Python 3.9+, Node 기반 agent를 고르면 네이티브 Node.js |

기본 `core` 프로필은 하네스와 코딩 agent 하나를 설치한다. Alpine 같은 musl
기반 Linux는 지원하지 않는다. 프로필과 Windows 참고 사항은
[docs/INSTALL.ko.md](docs/INSTALL.ko.md)에 있다.

### 설치하면 바뀌는 것

- **전역 agent 규칙.** `~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`,
  `~/.gemini/AGENTS.md`가 OMS 규칙을 가리키는 링크가 된다. 기존 파일은
  `<파일>.backup.<타임스탬프>`로 옮겨지고 제거할 때 돌아온다.
- **agent 설정.** Claude Code에는 훅, 상태 줄 HUD, Opus 5.5의 `high` effort
  기본값(직접 정한 값이 없을 때만)이 들어간다. Codex에는 OMS 플러그인, 하단
  표시줄, 몇 가지 사용량 기본값이 들어간다. Antigravity에는 OMS 플러그인이
  들어간다. Claude Code와 Codex에는 OMS MCP 서버가 등록된다.
- **홈 디렉터리의 파일.** 세 CLI용 `oms-*` 스킬, `~/.local/bin`의 `oms` 명령
  (셸 PATH에 추가된다), `~/.local/share/oh-my-setting` 아래의 전용 Python.
- **매일 업데이트.** 수정 사항이 없는 체크아웃은 하루 한 번 fast-forward된다.
  원하지 않으면 설치 명령의 끝을 `| bash -s -- --no-auto-update`로 바꾼다.

### 업데이트와 제거

agent에게 oh-my-setting을 업데이트하거나 제거해 달라고 하면 된다. agent가
`oms update`나 `oms uninstall`을 실행한다. 제거하면 바꿨던 파일이 복원되고
agent CLI는 그대로 남는다. 릴리스별 변경은
[마이그레이션 노트](docs/MIGRATION-0.7.md)에 있다.

## 동작 방식

<p align="center">
  <img src="docs/assets/how-it-works.svg" alt="사용자는 아무 코딩 agent에게 말하고, Codex·Claude Code·Antigravity는 규칙·상태·council·위임·검증된 착지를 담은 oms 층을 공유하며 저장소에서 일한다" width="100%">
</p>

코딩 agent를 아무 디렉터리에서나 열고(빈 디렉터리, 진행 중 프로젝트, 기존
repo) 이렇게 말한다.

```text
이 프로젝트 시작해줘.
```

빈 디렉터리는 짧은 spec 인터뷰를 거쳐 `PROJECT.md`와 템플릿을 만든다. 기존
repo는 먼저 코드를 읽고 빈 곳만 묻는다. 진행 중 프로젝트는 상태와 다음 할
일을 보고한다. `PROJECT.md`를 확정하면 검토한 작업, 검사, Draft PR까지 맡길
수 있다. 생성된 작업은 스스로 승인되지 않고, merge와 release는 사용자가
정한다.

## 이렇게 말하면 된다

```text
이 프로젝트 시작해줘.
지금 diff를 peer review 해줘.
세 모델에게 토론 1라운드로 물어봐: vector DB와 pgvector 중 뭐가 나아?
codex에게 맡겨줘: scripts/train.py에 입력 검증 추가.
확정된 PROJECT.md를 구현부터 Draft PR까지 진행해줘.
학습 전에 이 데이터셋의 group split 누수를 확인해줘.
Slurm job 12345가 끝나면 로그를 요약해서 알려줘.
oh-my-setting 업데이트하고 doctor 다시 돌려줘.
```

## 들어 있는 것

| | |
|---|---|
| **공유 규칙** | 세 CLI가 함께 쓰는 전역 규칙 하나와 `general`·`ml`·`slurm` 프로젝트 템플릿 |
| **Council** | 서로 독립적인 다중 모델 리뷰와 토론. 판정 좌석은 읽기 전용의 제한된 시야로 본다 |
| **위임** | 격리된 worktree 작업자, 반영 전 심사, 하나의 착지 경로. 재귀 위임은 없다 |
| **상태와 handoff** | Work Journal, attention inbox, compaction 전 handoff, 실패 ledger |
| **검증된 착지** | 게이트 한 번, 트리가 그대로일 때만 push, CI 뒤 그 커밋으로 설치 갱신 |
| **ML과 HPC** | 재현 가능한 실행, 누수 검사, Slurm과 GPU 대기열 도우미 |
| **Typed runtime** | 작업 계약, 증거 범위, 제한된 context, 이식 가능한 capsule([docs/OMS-RUNTIME.md](docs/OMS-RUNTIME.md)) |

agent가 알아서 골라 쓴다. 전체 목록은 [docs/COMPONENTS.md](docs/COMPONENTS.md)에 있다.

## 참고

- **로컬 우선.** 기본은 로컬 파일과 CLI다. connector는 요청할 때만 쓴다.
- **비공개 정보는 커밋하지 않는다.** 토큰, 개인 데이터, 머신 정보는 git에
  들어가지 않고, 프로젝트별 agent 파일은 로컬에서만 무시 처리된다.
- **되돌릴 수 있다.** `oms uninstall`이 바꿨던 파일을 복원한다.

도움이 됐다면 [GitHub](https://github.com/eightmm/oh-my-setting)에 ⭐ 하나 부탁드린다.
