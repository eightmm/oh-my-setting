# oh-my-setting 설치

한 줄 설치 명령과 설치하면 바뀌는 것은 [README](../README.ko.md)에 있다. 이
문서는 호스트 요구 사항, capability 프로필, 업데이트의 세부 내용을 다룬다.
[English](INSTALL.md)

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

## 호스트

| 호스트 | 필요한 것 | 관리 파일 |
|---|---|---|
| Linux, WSL (glibc) | Bash 3.2+, curl. Git·tar·gzip·find는 없으면 설치한다(이때 root나 sudo가 필요하다). Python 불필요 | symlink |
| macOS | 기본 Bash 3.2, curl, Command Line Tools(`xcode-select --install`, 없으면 `git`은 설치 안내 스텁뿐이다). Python 불필요 | symlink |
| Windows Git Bash | Git, Python 3.9+, 선택한 도구가 요구할 때 lock과 정확히 같은 네이티브 Node | 검증된 사본 |

`python3` 3.9+와 uv가 모두 없으면 설치기는 고정 버전 uv를 받아 sha256을
확인하고, `tools.lock.json`의 Python을 직접 만든 뒤 관리형 `python3` shim이
그것을 가리키게 한다. Alpine 같은 musl 기반 Linux는 지원하지 않는다. 고정된
빌드가 glibc를 요구하기 때문이다.

Windows에서 기본 Codex 설치나 Node 기반 도구를 고른 프로필은 먼저 네이티브
**Node.js와 npm**이 필요하다. 설치기가 알려 주는 정확한 버전을
[공식 Node.js 아카이브](https://nodejs.org/dist/)에서 설치하고, PATH 옵션을
켠 채로 Git Bash를 다시 연다. 설치기는 다른 Node 버전을 받아들이지 않고
멈춘다. Antigravity만 쓰는 `core` 프로필은 Node가 필요 없다.

## 프로필과 도구

기본 설치는 `core` capability를 고른다. 하네스, Bash, Git, Python, 코딩 agent
하나다. GitHub CLI, Notion CLI, 세 provider 전부, 연구 도구, 클러스터 도구는
필수가 아니다. `full` 호환 프로필은 예전의 전체 provider·GitHub·Notion·연구
도구 구성이 필요한 머신에만 쓴다.

프로필은 `core`, `council`, `github`, `notion`, `research`, `hpc`,
`container`, `remote`, `full`이다. 선택형 설치기는 기존 잠금 다운로드 절차를
그대로 쓰고, 요청한 프로필을 비공개 receipt에 기록해 업데이트 때 그 도구
묶음만 다시 적용한다. capability receipt가 없는 기존 설치는 agent가 명시적으로
옮기기 전까지 예전의 전체 도구 업데이트 경로를 유지한다.
[OMS-RUNTIME.md](OMS-RUNTIME.md)를 참고한다.

없는 Git·tar·gzip·find를 설치할 때를 빼면 root가 필요 없다. 관리 도구의
부트스트랩 버전, 플랫폼 URL, 무결성 값은 `tools.lock.json`에 고정되어 있다.
Codex, Claude, agy는 설치와 업데이트 때 현재 안정판을 찾고, 받은 바이트를 각
공식 릴리스 체크섬과 대조한다. 예약 업데이트는 OMS가 바뀌지 않아도 설치된
provider를 갱신한다. 수동 `--no-tools`는 도구 갱신을 건너뛰고, 명시한
`OH_MY_SETTING_TOOL_LOCK`은 재현 가능한 고정 설치를 유지한다. 비공개
`provider-tools.lock.json` 스냅샷은 찾은 버전을 기록할 뿐 영구 버전 정책이나
설치 증명이 아니다. 인식된 standalone Codex 설치는 자체 업데이트를 유지하고,
OMS는 결과 버전만 확인한다.

provider 의존성은 provider와 함께 정해진다. Codex와 Claude는 고정된 Node를
포함하고 Antigravity는 포함하지 않는다. 새로 받은 파일은 쓰기 전에 검증한다.
정확히 같은 버전의 기존 외부 CLI는 재사용하고 doctor가 버전만 확인된 것으로
표시한다. 설치, 업데이트, 복구, 제거는 사용자 단위 수명주기 잠금 하나를
공유한다.

## 업데이트

설치와 예약 업데이트는 `tools.lock.json`에 고정된 전용 uv Python을 쓴다.
시스템이나 프로젝트 Python은 부트스트랩일 뿐 타이머의 인터프리터가 아니다.
자동 업데이트는 체크아웃의 로컬 수정을 보존하고 `blocked`로 보고한다. 개인
정책은 추적되는 설치 트리 밖의 사용자 스킬에 둔다. systemd 설치는 로그아웃
뒤에도 유지되는지 보고한다. 트리거를 설치할 때
`OH_MY_SETTING_AUTO_UPDATE_LINGER=1`을 명시하면 켜지고(호스트 권한이 필요할 수
있다), 아니면 cron을 쓴다. linger는 이 계정의 모든 사용자 서비스에 적용되므로
OMS가 기본으로 켜지 않는다.

매일 도는 업데이트는 수정 없는 체크아웃만 fast-forward하고, 수정됐거나 갈라진
체크아웃은 건너뛴다. 기존 설치의 업그레이드는 `oms update` 한 번이고, 릴리스마다
바뀌는 것은 마이그레이션 노트(현재 [MIGRATION-0.7.md](MIGRATION-0.7.md))에
있다.

## 서비스와 화면

GitHub나 Notion capability를 고르면, 대화형 설치기는 브라우저 로그인을
`gh auth login`과 `ntn login`에 맡기고 Work Journal의 Notion 대상을 찾는다.
비대화형 설치는 core 런타임을 약하게 만드는 대신 빠진 capability를 기록한다.
Claude Code에는 main과 subagent용 간결한 HUD(모델·effort, context, 사용량 한도
카운트다운, 비용, Git 상태)가 들어가고, Codex에는 사용자 하단 표시줄이 없을 때
같은 역할의 기본 표시줄이 들어간다.

## 제거

제거는 관리하던 설정을 복원하지만 외부 CLI와 사용자 PATH 항목은 남긴다. 세
전역 규칙 파일(`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`,
`~/.gemini/AGENTS.md`)의 기존 파일은 `<파일>.backup.<타임스탬프>`로 옮겨지고
(설치 때 알리고 `oms doctor`가 보고한다), 설치가 유지되는 동안 적용되지 않으며,
`oms uninstall`이 복원한다.

예외 상황(Windows 사본 모드, Antigravity headless 권한, Notion data source
지정)은 [COMPONENTS.md](COMPONENTS.md)와 [WORK-JOURNAL.md](WORK-JOURNAL.md)에
있다.

스크립트는 `~/.oh-my-setting/scripts/`에 있고 `oms <도구>`로 부른다. 투명성과
복구를 위해 문서화할 뿐 직접 쓰라는 것은 아니다.
