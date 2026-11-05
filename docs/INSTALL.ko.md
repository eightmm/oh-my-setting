# oh-my-setting 설치

이 안내는 Linux, macOS, Windows via WSL2에서 OMS를 설치하고 관리하는 사용자를 위한 문서다.
아래 명령으로 시작한 뒤 호스트와 프로필별 세부 내용을 확인한다.

한 줄 설치 명령과 설치하면 바뀌는 것은 [README](../README.ko.md)에 있다. 이
문서는 호스트 요구 사항, capability 프로필, 업데이트의 세부 내용을 다룬다.
[English](INSTALL.md)

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

## 호스트

| 호스트 | 필요한 것 | 관리 파일 |
|---|---|---|
| Linux (glibc) | Bash 3.2+, curl. Git·tar·gzip·find는 없으면 설치한다(이때 root나 sudo가 필요하다). Python 3.11+는 관리형 런타임으로 설치된다 | symlink |
| macOS | 기본 Bash 3.2, curl, Command Line Tools(`xcode-select --install`, 없으면 `git`은 설치 안내 스텁뿐이다). Python 3.11+는 관리형 런타임으로 설치된다 | symlink |
| Windows via WSL2 | WSL2 안에서 위 Linux 요구 사항 적용 | symlink |

Python 요구 사항은 상한 없는 3.11 이상이다. 관리형 런타임은 `tools.lock.json`의 3.12 버전을 사용하므로 Ubuntu 20.04/22.04나 macOS Command Line Tools의 오래된 시스템 Python을 바꿀 필요가 없다.

시스템 `python3`가 없거나 3.11 미만이면 설치기는 고정 버전 uv를 받아 sha256을
확인하고, `tools.lock.json`의 Python을 직접 만든 뒤 관리형 `python3` shim이
그것을 가리키게 한다. Alpine 같은 musl 기반 Linux는 지원하지 않는다. 고정된
빌드가 glibc를 요구하기 때문이다.

provider 앱의 PATH가 `/usr/bin:/bin`뿐이어도 hook launcher는 관리형
인터프리터를 선택한다. Antigravity MCP 등록은 해석된 인터프리터의 절대 경로를
저장한다. 관리형 런타임이 없어도 오래된 시스템 Python으로 제거와 lifecycle
도움말을 실행할 수 있다.

Ubuntu 24.04+에서 AppArmor가 비특권 user namespace를 제한하면 `oms doctor`가
Codex sandbox 실행 실패 가능성을 경고한다. AppArmor 프로필에서 `bwrap`의
user namespace를 허용하거나 sysctl로
`kernel.apparmor_restrict_unprivileged_userns=0`을 설정한다.

## Windows via WSL2

WSL1이 아닌 **WSL2**를 사용한다. 저장소와 `~/.oh-my-setting`, WSL 홈은
모두 Linux 파일시스템 안에 둔다. `/mnt/c`는 사용하지 않는다. provider CLI도
WSL 안에 설치하고 실행하며 위 Linux 요구 사항을 따른다.
네이티브 Windows(Git Bash/MSYS2/Cygwin)는 지원하지 않으며 OMS 실행을 거부한다.

기존 네이티브 Windows 설치를 제거하려면 기존 설치 체크아웃에서 Windows를
마지막으로 지원한 `oms-windows-final` 태그를 사용한다.

```bash
cd ~/.oh-my-setting
git fetch origin tag oms-windows-final
git switch --detach oms-windows-final
bash scripts/uninstall.sh
```

전환 전에 체크아웃의 로컬 변경을 보존한다. 현재 릴리스는 네이티브 Windows의
`oms uninstall`도 거부한다. 이미 실행 중인 구버전 업데이트도 새 트리에 진입하면
중단하고 체크아웃과 관리 링크를 롤백한다. WSL2 설치는 별도로 시작한다.

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
[MIGRATION.md](MIGRATION.md#capability-profiles)를 참고한다.
`oms install-profile --apply --add --profile github`은 기존 프로필, provider,
누락 허용 정책을 유지하며 capability를 추가한다. `--add` 없는 `--profile`은
기록된 선택을 대체하고, `--reapply`는 그대로 다시 적용한다. `--plan`으로
변경 없이 미리 확인할 수 있다.

`OH_MY_SETTING_MCP=0`, `OH_MY_SETTING_AGY_PLUGIN=0`은 각각의 연동 설치를
건너뛴다. 설치 receipt에 두 선택을 기록하므로 환경 변수가 없는 업데이트와
예약 실행도 유지한다. `oms update`에 환경 변수를 명시하면 기록된 선택을
바꾼다. Antigravity는 `auto`(기본: 있을 때 설치), `1`(필수)도 받는다.

`oms list`는 설정(`init`, `install-profile`), 일상 작업(`agent-plan`, `status`),
협업, 전달, 유지보수(`doctor`, `update`, `uninstall`) 순으로 명령을 묶는다.
`oms list --all`은 전체 목록을 유지한다.

없는 Git·tar·gzip·find를 설치할 때를 빼면 root가 필요 없다. 관리 도구의
부트스트랩 버전, 플랫폼 URL, 무결성 값은 `tools.lock.json`에 고정되어 있다.
새 provider 설치는 안정판을 찾아 비공개 `provider-tools.lock.json` 스냅샷에
기록한다. Linux/macOS의 새 Claude는 공식 HTTPS manifest의 SHA-256으로
네이티브 바이너리를 검증한 뒤 `install VERSION`을 실행한다. manifest에
OMS가 GPG 서명을 별도로 검증하지는 않는다. 네이티브 Windows Git Bash는 기존 npm 경로를 쓰며,
새 Codex와 agy도 기존 검증 설치 경로를 유지한다.

인식된 Claude, Codex, agy는 각 vendor의 update 명령으로 갱신한다.
Pi 갱신은 `install-tools.sh --providers`에서만 `pi update self`로 실행한다.
전체 도구 설치와 프로필 재적용은 Pi를 갱신하지 않는다. OMS는 vendor 경로 유지, `--version` 성공,
버전 하락 여부를 확인하며 스냅샷 버전과 같을 것을 요구하지 않는다.
Pi는 이미 설치된 경우에만 갱신한다. 예약 업데이트도 설치된 provider를
갱신하며, 선택적 npm provider는 게시된 지 14일 이상인 안정판을 사용한다.
npm lifecycle 스크립트는 실행될 수 있고 git/URL 의존성에는 이 기간이
적용되지 않는다. 수동 `--no-tools`는 도구 갱신을 건너뛴다.

명시한 `OH_MY_SETTING_TOOL_LOCK`이 있으면 Claude는 `claude install VERSION`으로
고정하고 정확한 버전을 확인한다. 네이티브 Codex와 agy는 고정 버전과 다르면
변경 전에 거부한다. 현재 lock에는 Pi 항목이 없어 명시적인 lock이 있으면
안내 한 줄과 함께 Pi 갱신만 건너뛰고 성공으로 끝낸다. OMS 소유 agy는 갱신
전에 기존 digest가 맞아야 실행하며, doctor도 변경된 바이트를 drift로 보고한다.
검증된 바이너리와 marker를 복원하거나 둘 다 보존한 뒤 프로필을 다시 적용한다.
기존 digest 확인, 갱신 성공, 버전 검증을 모두 통과해야 새 digest를 기록한다.
중단된 OMS 직접 설치의 잔여 상태는 고정 payload의 checksum을 검증한 뒤
먼저 복구한다. 잔여 상태 없는 변조는 계속 거부한다. doctor는 provider/model
실행 전에 선택된 관리 도구를 검증하고, digest가 틀린 사본의 버전 실행도 건너뛴다.
버전 명령은 PATH에서 선택된 CLI에만 실행한다. 추가 사본은 sidecar digest가
일치해도 실행하지 않고 `not executed`로 표시한다. npm 버전은 패키지 metadata에서 읽는다.
`digest` 상태는 `matched`, `unmarked`, `invalid`, `unknown`이며 바이트 확인
결과를 나타낼 뿐 추가 사본의 실행을 허가하지 않는다.
vendor 보관 버전은 PATH에 있어도 한 설치로 묶고 삭제 명령을 제안하지 않는다.
줄 또는 필드 구분자가 들어간 항목은 이스케이프해서 unknown으로 표시하고 실행하지 않는다.
무결성 오류로 버전 확인을 건너뛰면 `not probed`로 표시한다. 직접 설치 도구의
중단된 트랜잭션 잔여 상태는 vendor 버전을 정상으로 판단하기 전에 보고한다.

네이티브 versions 디렉터리가 없거나 비었거나 유효한 실행 파일이 없으면
기존 npm/shim 경로를 유지한다. OMS 소유 Claude npm shim과 기존 네이티브 버전이 함께 있으면 최신 네이티브
바이너리의 설치기로 launcher를 복원하고, shim에 기록된 prefix의 npm 사본만
제거한다. 소유권을 증명할 수 없으면 변경 전에 거부하고 복구 방법을 알린다.
vendor 설치 후 npm 정리 전에 중단되면 backup을 유지한다. 재실행은 PATH
순서와 무관하게 네이티브 launcher를 직접 확인하고 재설치 없이 정리한다.
활성 npm 루트의 Claude launcher는 `~/.local` prefix에서도 그대로 허용한다.
`oms doctor`는 PATH, vendor 루트, 모든 nvm Node prefix의 사본을 나열하고
선택된 경로, 채널, 버전과 중복 제거 명령을 표시한다. 정리 명령은 선택되지
않은 npm 패키지나 외부 launcher만 대상으로 한다. vendor가 보관한 이전
버전 파일은 중복으로 세거나 실행·제거하지 않고, alias의 버전 점검은 재사용한다.
제거와 복구는 vendor launcher나 루트를 지우지 않는다.

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
바뀌는 것은 마이그레이션 노트(현재 [MIGRATION.md](MIGRATION.md#07))에
있다.

provider 도구 갱신 뒤에는 시간 제한이 있는 연동 점검으로 Claude hooks,
Codex plugin, 활성 MCP/Antigravity 인터페이스를 확인한다. 실패하면 core
업데이트 성공과 연동 상태 실패를 따로 보고하고 실패로 종료한다. provider
바이너리는 core 롤백 대상이 아니다. `--no-doctor`나 예약 실행의 doctor
생략 설정을 쓰면 이 검증 근거도 생략한다.

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

예외 상황(Antigravity headless 권한, Notion data source
지정)은 [COMPONENTS.md](COMPONENTS.md)와 [WORK-JOURNAL.md](WORK-JOURNAL.md)에
있다.

스크립트는 `~/.oh-my-setting/scripts/`에 있고 `oms <도구>`로 부른다. 투명성과
복구를 위해 문서화할 뿐 직접 쓰라는 것은 아니다.
