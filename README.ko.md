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

터미널에서 **`oms`를 실행하면 공통 컨트롤 패널**이 열린다. 시작 방법과
보드 사용법은 아래에, 자세한 설명은 [패널 안내](docs/TERMINAL-PANEL.md)에
정리했다.

하네스의 나머지 도구와 `.oms/` 상태, 업데이트·점검은 agent가 관리한다.
아래의 하네스 명령은 agent가 실행할 명령이다.

## 컨트롤 패널

패널은 현재 프로젝트용 tmux 컨트롤 창을 열고, 작업방마다 실행 중인 main을
최대 하나만 둔다(기본은 Claude Code, 명시적 대안은 Pi). main 창에서 공간이
충분하면 보드가 왼쪽에, 좁으면 CLI 위에 놓인다. 오른쪽 reader에서는 선택한
worker·advisor·review·result를 연다.

`oms`로 시작한다. Pi는 `OMS_PANEL_MAIN=pi oms` 또는 Pi 시작 키(`n` 다음 `2`)로
선택한다. `oms panel --launch claude|pi`로 main을 직접 시작할 수도 있다.
tmux가 없으면 inline 메뉴를 사용한다. 한 작업방에서 main이 여러 개 실행 중이면
dispatch를 거부한다. 추가 main을 `oms panel --release-main ATTEMPT`로 해제하거나
교체를 마친 뒤 다시 실행한다.

| 보드 영역 | 표시 내용 |
|---|---|
| Goal | plan 목표와 검증 완료 작업 수 |
| Attention | 답변 필요, 실패, 결정 대기, 보류/worker 종료, 읽지 않은 메일, 전달 실패 등 사용자 조치가 필요한 항목 |
| Activity tree | main과 하위 호출, 취소·guard 중단·시간 초과·검증 실패 같은 실제 종료 상태 |
| Session log | 스크롤할 수 있는 활동 로그 |
| 하단 탭 | Detail, Plan, Debate, Messages, Past, Attention. 종료된 호출이 있을 때 Past 표시 |

| 키 | 동작 |
|---|---|
| 화살표 / Enter | 항목을 이동하고 선택 내용을 열거나 펼친다 |
| `f` | 선택한 전체 결과를 오른쪽 reader에서 연다 |
| `a` | advisor에게 묻는다 |
| F9 | 채팅과 보드 사이의 초점을 바꾼다 |
| `m` | Work, Overview, Focus 모드를 순환한다 |
| F12 | 패널 전체 키 목록을 표시한다 |
| `[` / `]` | Detail 탭이 선택된 상태에서 session log를 이전/다음 방향으로 스크롤한다 |

main은 역할별로 범위가 정해진 호출을 위임한다. 쓰기 역할은 worker, builder,
refactorer, documenter, integrator, performance-engineer다. 읽기 역할은
researcher, advisor, architect, critic, reviewer, verifier, security-auditor,
release-engineer다. 라우팅은 특정 제공자에 역할을 고정하지 않고 사용 가능한
제공자의 light, routine, deep, frontier 역량 tier를 따른다. reviewer, verifier,
security-auditor, critic은 가능하면 main과 다른 제공자를 우선한다. 역할은 별도
main 인스턴스가 아니라 호출의 임무를 뜻한다.

```bash
oms panel --dispatch worker --purpose implement --access write --brief-file B --verify CMD --task-id T --label L
oms panel --dispatch verifier --access read --prompt '요청 범위에 맞는지 검증한다.'
oms panel --council --prompt '두 선택지를 비교한다.'
oms panel --finalize --task-id T --summary-file SUMMARY --outcome completed
```

목표와 작업은 `oms agent-plan`에 기록한다. main은 `--finalize`로 결과를 남기며,
작업은 `oms land`를 통해 반영한다.

자세한 내용은 [컨트롤 패널 안내](docs/TERMINAL-PANEL.md)를 참고한다.

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

실험 기능인 [Codex 하네스 실행기](docs/CODEX-HARNESSES.md)는 기존 Codex 구독
로그인으로 Codex CLI·pi·Claude Code의 UI와 도구를 유지하며 OpenAI 모델을
사용한다. 예: `oms harness claude --model gpt-6.1-sol`.

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
