# oh-my-setting

Codex, Claude Code, Antigravity가 어느 머신에서든 같은 규칙, 같은 skill, 같은
agent 하네스를 쓰게 하는 설정 하나.

**직접 실행할 일은 없다.** 설치 명령 하나만 직접 치고, 그 뒤로 하네스는
사용자 것이 아니라 agent 것이다. 도구(`oms ...`)는 agent가 작업 중에 부르고,
상태(`.oms/`)는 agent가 쓰고, 설정 작업 — 업데이트, 상태 점검, 다른 agent CLI에
필요한 권한까지 — 도 agent에게 부탁하는 일이다. 이 문서에 명령이 보이면 그건
agent가 실행할 것을 보여주는 것이다.

[English](README.md)

## 설치

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

하네스와 함께 조율 대상 CLI까지 설치한다 — Claude Code, Codex, Antigravity,
Node(nvm 경유), uv, GitHub CLI, Notion CLI. root 권한은 필요 없고, 설치할 수
없는 머신에서는 `--no-tools`를 붙인다. 대화형 터미널에서는 설치기가
`gh auth login`과 `ntn login`의 브라우저 로그인을 실행하고 Work Journal의
Notion 대상을 자동 발견하며, 비대화형 설치는 후속 명령을 알려주고 계속
진행한다. Claude Code에는 로컬 status-line HUD(컨텍스트, 리셋 카운트다운이
붙는 plan 사용량, 비용)도 기본으로 설치된다.

| 호스트 | 필요 | 관리 파일 |
|---|---|---|
| Linux, WSL | Bash 3.2+, Git, Python 3.9+ (또는 uv) | symlink |
| macOS | 기본 Bash 3.2, Git, Python 3.9+ (또는 uv) | symlink |
| Windows Git Bash | Git, Python 3.9+; Node 22+와 provider CLI는 각자의 Windows 설치기로 | 검증된 사본 |

예외 상황 — Windows 사본 모드, Antigravity headless 권한, Notion data source
지정 — 은 [docs/COMPONENTS.md](docs/COMPONENTS.md)와
[docs/WORK-JOURNAL.md](docs/WORK-JOURNAL.md)에 있다.

## 시작

코딩 agent를 아무 디렉터리에서든 열고 — 빈 디렉터리, 진행 중 프로젝트, 기존
repo — 이렇게 말한다:

```text
이 프로젝트 시작해줘.
```

agent가 상태를 판별해 분기한다. 빈 디렉터리는 spec 인터뷰 → `PROJECT.md` →
템플릿 → doctor로, 기존 repo는 코드를 먼저 읽고 빈 곳만 인터뷰하며, 진행 중
프로젝트는 상태와 다음 할 일을 보고한다.

## 이렇게 말하면 된다

```text
이 프로젝트 시작해줘.
oh-my-setting ml 템플릿 적용해줘.                 # 또는 general, slurm
지금 diff 피어 리뷰 돌려줘.
세 모델에게 debate 한 라운드로 물어봐: vector DB냐 pgvector냐?
이거 codex한테 위임해줘: scripts/train.py에 입력 검증 추가.
이 split 정책 다른 에이전트한테 물어보고 thread 유지해줘.
학습 전에 이 데이터셋 group split leakage 확인해줘.
런 돌리기 전에 가설 기반 실험으로 정리해줘.
Slurm job 12345 끝나면 로그 digest해서 보고해줘.
oh-my-setting 업데이트하고 doctor 다시 돌려줘.
```

## 구성 요소

전부 agent가 필요할 때 알아서 집어 쓴다. 기능마다 문 하나씩이고, 전체
카탈로그는 `oms list`, 문서는 [docs/COMPONENTS.md](docs/COMPONENTS.md)다.

- **프로젝트 설정** — start router, spec 인터뷰, `general`/`ml`/`slurm`
  템플릿, 세 agent가 같은 규칙을 읽는지 검증하는 doctor, 로컬에서만 숨겨지는
  agent 파일
- **다른 에이전트에게 묻기** — 이어지는 peer thread, 세 모델 council과
  프로젝트 자체 검사가 뒤를 받치는 리뷰 게이트, 결정 시점 advisor, 발신 전
  민감정보 scrub
- **쓰기 위임** — 격리 worktree 위임, admission 사다리, 단일 변이 경계,
  해시 동결 executor soul과 워커 권한 지문
- **Agent 상태와 핸드오프** — 일간 요약과 선택적 Notion mirror가 있는 Work
  Journal, compaction 직전 세션 핸드오프, 공유 메모리, 검증이 실제로 실행되는
  task packet, 반복 실패 시 advisor를 지목하는 fail-ledger
- **ML과 HPC** — run spine과 재현 캡슐, 사전등록 가설 run, 데이터셋 leakage
  manifest, Slurm reconcile과 GPU 큐
- **Provider와 모델** — capability 프로브, fast/balanced/deep 티어 라우팅과
  폴백, family 다양성 진단
- **유지보수** — 롤백 가능한 트랜잭션 업데이트, doctor, 하나의 전체 검증
  게이트와 보호 브랜치용 빠른 pre-push 모드

skill은 세 계층으로 붙는다: 어디서나 같은 범용 5개, 필요한 명령이 있는
머신에만 링크되는 머신 조건 skill(`slurm`, `gpu-workstation`), 그리고 저장소의
`.oms/skills/`에 포지되는 프로젝트 skill.

## 참고

- 로컬 우선: connector는 명시적으로 요청했거나 로컬 자료로 답할 수 없을 때만.
- 토큰, private data, cluster/머신 상세는 commit하지 않는다 — 프로젝트별
  agent 파일은 설계상 git 밖에 있으므로, 새로 clone하면 템플릿을 다시 적용한다.
- 스크립트는 `~/.oh-my-setting/scripts/`에 있고 `oms <tool>`로 부른다
  (`oms list`가 카탈로그 출력). 투명성과 복구용 문서화일 뿐이다.

## Star

도움이 됐다면: [github.com/eightmm/oh-my-setting](https://github.com/eightmm/oh-my-setting)
