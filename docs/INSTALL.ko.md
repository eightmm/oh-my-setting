# oh-my-setting 설치

Linux, macOS, Windows via WSL2의 호스트 요구 사항, 프로필과 유지보수를 다룬다.
한 줄 설치 명령과 바뀌는 파일의 요약은 [README](../README.ko.md)를 참고한다.
[English](INSTALL.md)

추가 계정은 `oms connect-services --account codex:ID`, `--account claude:ID`, 또는
`--account pi:ID`로 등록한다. 전용 홈(`CODEX_HOME`, `CLAUDE_CONFIG_DIR`,
`PI_CODING_AGENT_DIR`), 라우팅, doctor 점검, repair 및 제거는
[provider accounts](TERMINAL-PANEL.md#provider-accounts)를 참고한다. 수동
config-scoped claude.ai 사용은 문서화되어 있지만, native 선택 필드만으로
credential origin을 증명하지 못하므로 `--verify` 격리 검증은 여전히
unsupported/uncertain이다. Native status는 선택된 root에 metadata를 초기화할 수
있다. 등록·doctor·update는 status를 자동 실행하지 않는다. Claude 상태는 기존
Codex 및 Pi 파일과 분리되며, 구버전은 이를 관리하거나 임의 홈 충돌을 보호하지
못한다. Antigravity OAuth 계정 격리는 확인되지 않아 fail-closed
(`provider account isolation is unproved`)로 거부된다.

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

## 호스트

| 호스트 | 필요한 것 | 관리 파일 |
|---|---|---|
| Linux (glibc) | Bash 5.0+, curl. Git·tar·gzip·find는 없으면 설치한다(이때 root나 sudo가 필요하다) | symlink |
| macOS | Bash 5.0+, curl, Command Line Tools(`xcode-select --install`, 없으면 `git`은 설치 안내 스텁뿐이다). 설치기는 이미 있는 Homebrew Bash 5로 재실행할 수 있다. 없으면 `brew install bash` 후 다시 실행한다. | symlink |
| Windows via WSL2 | WSL2 안에서 위 Linux 요구 사항 적용 | symlink |

Claude Code [memory 문서](https://code.claude.com/docs/en/memory.md)는 데스크톱 Cowork 세션이
symlink나 hard link인 `~/.claude/CLAUDE.md`를 건너뛴다고 적는다. 기본 link 모드에서는 전역 규칙이
바로 그 symlink라서 그 표면에서는 로드되지 않을 수 있다. 터미널 CLI는 해당하지 않는다.
`OH_MY_SETTING_LINK_MODE=copy`는 일반 파일로 설치하고 `oms doctor`가 원본과의 일치를 검사한다.
OMS는 Cowork 표면을 시험하지 않았다.

OMS lifecycle 명령은 Rust 네이티브 바이너리로 실행한다. 설치와 예약 업데이트는
전용 Python 인터프리터를 만들거나 요구하지 않는다. 검증된 staged payload가 없는
설치·업데이트는 선택한 체크아웃의 `rust-toolchain.toml`에 선언된 도구 체인과
Cargo로 빌드하므로 먼저 rustup으로 Rust를 준비한다. Native·Shell 빌드는 Cargo
자식 프로세스에서만 상속된 `RUSTUP_TOOLCHAIN`을 제거하며 호출자의 환경은 유지한다.
`--locked`로 `Cargo.lock`을
유지하면서 누락된 의존성을 받을 수 있다. 도구 체인과 의존성이 이미 캐시에
있을 때만 `CARGO_NET_OFFLINE=true`로 오프라인 빌드를 선택한다.

관리 도구의 배포 대상은 amd64/arm64의 glibc Linux와 macOS다. Alpine 같은
musl 기반 Linux는 이 배포 대상에 포함되지 않는다.

Ubuntu 24.04+에서 AppArmor가 비특권 user namespace를 제한하면 `oms doctor`가
Codex sandbox 실행 실패 가능성을 경고한다. AppArmor 프로필에서 `bwrap`의
user namespace를 허용하거나 sysctl로
`kernel.apparmor_restrict_unprivileged_userns=0`을 설정한다.

## Windows via WSL2

WSL1이 아닌 **WSL2**를 사용한다. 저장소와 `~/.oh-my-setting`, WSL 홈은
모두 Linux 파일시스템 안에 둔다. `/mnt/c`는 사용하지 않는다. provider CLI도
WSL 안에 설치하고 실행하며 위 Linux 요구 사항을 따른다.
네이티브 Windows(Git Bash/MSYS2/Cygwin)는 지원하지 않으며 OMS 실행을 거부한다.
`scripts/install-tools.sh`와 `scripts/doctor.sh`를 직접 실행해도 거부하며,
관리 도구의 배포 목록에는 네이티브 Windows 대상이 없다.

<details>
<summary>기존 네이티브 Windows 설치 제거 절차</summary>

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

</details>

## 프로필과 도구

기본 설치는 `core` capability를 고른다. 하네스, Bash, Git, OMS 네이티브 런타임, 코딩 agent
하나다. GitHub CLI, Notion CLI, 세 provider 전부, 연구 도구, 클러스터 도구는
필수가 아니다. `full` 호환 프로필은 예전의 전체 provider·GitHub·Notion·연구
도구 구성이 필요한 머신에만 쓴다.

프로필은 `core`, `council`, `github`, `notion`, `research`, `hpc`,
`container`, `remote`, `full`이다. 선택형 설치기는 기존 검증 다운로드 절차를
그대로 쓰고, 요청한 프로필을 비공개 receipt에 기록해 업데이트 때 그 도구
묶음만 다시 적용한다. capability receipt가 없는 기존 설치는 agent가 명시적으로
옮기기 전까지 예전의 전체 도구 업데이트 경로를 유지한다.
[MIGRATION.md](MIGRATION.md#capability-profiles)를 참고한다.

| `oms install-profile` 옵션 | 동작 |
|---|---|
| `--apply --add --profile github` | 기존 프로필·provider·누락 허용 정책을 유지하며 capability를 추가한다. |
| `--add` 없는 `--profile NAME` | 기록된 선택을 대체한다. |
| `--reapply` | 기록된 선택을 그대로 다시 적용한다. |
| `--plan` | 변경 없이 미리 확인한다. |

일반 재설치는 저장된 프로필·주 provider·누락 허용 정책을 유지한다. 옵션 없는
`install-tools`, `--only`, `--providers`는 도구만 처리하고 이 선택을 덮어쓰지
않는다. 명시적 profile/full/add/reapply 작업이 선택을 기록한다. Receipt 경로는
`--receipt`, `OMS_CAPABILITY_RECEIPT`, 기본 OMS 설정 경로 순서로 선택하며 상대
경로는 호출 디렉터리를 기준으로 한다. 업데이트는 작업 디렉터리를 바꾸기 전에
선택된 receipt의 경로를 새 프로세스에 전달한다.
업데이트할 때 명시한 receipt가 없거나 안전하지 않으면 오류를 내며 전체 도구
갱신으로 바꾸지 않는다.

`OH_MY_SETTING_MCP=0`, `OH_MY_SETTING_AGY_PLUGIN=0`은 각각의 연동 설치를
건너뛴다. 설치 receipt에 두 선택을 기록하므로 환경 변수가 없는 업데이트와
예약 실행도 유지한다. `oms update`에 환경 변수를 명시하면 기록된 선택을
바꾼다. Antigravity는 `auto`(기본: 있을 때 설치), `1`(필수)도 받는다.

## 도구 설치와 갱신

없는 Git·tar·gzip·find를 설치할 때를 빼면 root가 필요 없다. 관리 도구의 이름과
배포처는 네이티브 설치기에 정의하고, 누락된 도구를 설치하거나 업그레이드할 때
공식 배포처에서 버전을 조회한다. 저장소에 기본 버전 lock을 두지 않는다.
일반 설치는 이미 있는 도구를 유지하고, `oms update`는 `--no-tools`를 지정하지
않으면 선택된 도구 묶음을 갱신한다.

| 도구 | 기본 릴리스 선택 | 페이로드 검증 |
|---|---|---|
| Node | 공식 배포 인덱스의 최신 LTS | 버전이 명시된 archive와 `SHASUMS256.txt`의 SHA-256 |
| uv | `astral-sh/uv`의 최신 정식 GitHub 릴리스 | 현재 호스트의 정확한 asset과 릴리스 SHA-256 digest |
| gh | `cli/cli`의 최신 정식 GitHub 릴리스 | 현재 호스트의 정확한 asset과 릴리스 SHA-256 digest |
| Claude·Codex·ntn | 각 npm 패키지의 `latest` dist-tag | 패키지 이름·정확한 버전·SHA-512 SRI, Claude/Codex의 native 패키지도 동일 검증 |
| Antigravity | 공식 updater의 현재 호스트 manifest | 버전이 명시된 URL과 SHA-512 |

Node는 prerelease나 가장 높은 Current 메이저가 아닌 LTS를 따른다. 현재 호스트의
페이로드만 준비되어 있으면 되므로 다른 플랫폼의 배포 지연이 이 호스트를 막지
않는다. metadata 조회 실패나 잘못된 응답은 오류로 보고하며 과거 기록의 버전으로
대체하지 않는다. 이미 설치된 버전이 조회된 버전보다 새로우면 기본 업데이트가
이를 낮추지 않는다.

OMS data 디렉터리의 비공개 `tool-releases.json`에는 조회한 버전, URL 또는 패키지
좌표, 무결성 값과 조회 시각을 기록한다. 다음 업데이트를 고정하는 정책이나 모든
기록된 버전의 설치 증명은 아니다. 예전 `provider-tools.lock.json`의 provider 관측
기록은 가져올 수 있지만 다음 버전 선택에는 쓰지 않는다. `oms doctor --tool-lock`은
이 기록 또는 명시적 pin의 형식을 검사하며 upstream 조회나 설치 바이트 검증을
뜻하지 않는다. fingerprint에는 조회 시각을 반영하지 않는다.

정확한 버전이 필요하면 선택한 도구와 현재 호스트의 페이로드 metadata가 든
schema-2 JSON 파일을 `OH_MY_SETTING_TOOL_LOCK`으로 지정한다. 상대 경로는 명령의
현재 작업 디렉터리를 기준으로 해석한다. 명시적 pin은 검증 후 이전 버전도 선택할
수 있다. 환경 변수 지정을 없애면 저장소 수정 없이 기본 upstream 조회로 돌아간다.

인식된 vendor-native Claude·Codex·Antigravity 설치는 vendor의 update 명령을
유지한다. OMS는 버전 조회 성공과 버전이 낮아지지 않았는지 확인하며 vendor 관리
바이트가 과거 OMS 관측 기록과 일치한다고 주장하지 않는다. 새 Claude/Codex 관리
페이로드는 npm metadata와 패키지를 검증한 뒤 설치한다. `ntn`은 검증한 패키지의
설치 스크립트를 실행할 수 있다. 명시적 pin은 기본 vendor update 명령 대신 정확한
버전의 검증 페이로드 경로를 사용한다.

OMS로 처음 설치한 Antigravity를 vendor updater에 넘길 때는 바이너리의 버전을
조회하기 전에 기존 소유권 checksum sidecar를 검증한다. 이전 vendor 업데이트로
checksum만 어긋난 경우, 인식된 vendor 경로에서 현재 사용자가 소유한 안전한 OMS
sidecar를 허용한다. 버전을 조회하는 동안 sidecar와 바이너리가 바뀌지 않았는지
확인한다. 업데이트가 실행되지 않으면 sidecar를 유지한다. Updater에 넘기기 직전에
sidecar를 제거하며, checksum이 오래된 경우 경고를 표시한다. 형식이 잘못되거나
안전하지 않거나 바뀐 sidecar는 거부한다. Vendor 바이너리와 이 인계는 OMS rollback
범위 밖이다. 명시적 pin으로 교체할 때는 계속 일치하는 소유권 checksum을 요구한다.

이미 설치된 Pi는 `oms install-tools --providers`에서만 `pi update self`로 갱신한다.
그 밖에 인식되는 선택형 npm provider는 사용자가 소유하고 쓸 수 있는 global prefix에서
현재 `latest` dist-tag를 사용한다. 14일 배포 지연이나 npm `--before` 제한은 없다.
npm lifecycle 스크립트는 패키지 코드를 실행할 수 있다. 명시적 tool lock에는 이
선택형 도구를 표현하는 항목이 없으므로 해당 갱신을 건너뛴다.

provider 의존성은 함께 선택한다. Claude·Codex·ntn에는 Node를 포함하며 `--only`에서
Node를 뒤에 적어도 먼저 설치한다. Antigravity에는 포함하지 않는다. 검증된 패키지를
임시 디렉터리에 받으려면 npm 7.18.0 이상이 필요하다. 기본 관리 Codex와 ntn 실행에는
Node 16 이상을 사용한다. 다른 prefix의 호환 npm은 재사용한다. 필요한 도구가 없거나
호환되지 않으면 검증된 Node 번들을 준비하며, npm만 복구할 때는 요구 버전을 만족하는
Node를 보존한다. 다만 선택된 버전의 NVM 디렉터리가 이미 불완전하거나
손상된 경우에는 소유권을 입증하지 못하는 파일을 교체하지 않고 거부한다. 해당
디렉터리의 자동 복구는 아직 구현하지 않았다. 관리 파일 반영
전에 다운로드와 패키지 이름·버전을 검증하며 설치·
업데이트·복구·제거는 되돌릴 수 있는 lifecycle transaction을 공유한다. Vendor update
명령의 복구는 vendor가 담당하며 해당 바이너리는 OMS 체크아웃 롤백 대상이 아니다.

## 업데이트

새 설치의 기본 추적 대상은 origin 저장소의 기본 브랜치를 따르는 `edge`다.
명시한 `OH_MY_SETTING_REF`, `--ref`, 저장된 추적 선택은 유지한다.
`--fallback-to-edge`가 성공하면 다음 업데이트도 `edge`를 따르도록 기록한다.
Runtime edge 채널 적용은 설정한 브랜치의 추적을 유지하고, 검토한 stable 릴리스
적용은 그 커밋을 고정한다. 채널 manifest가 없으면 stable은 명시적으로 승격하기
전까지 미설정 상태다.

네이티브 updater는 새 체크아웃의 Rust 도구 체인으로 incoming payload를 준비하고
검사한 뒤 반영한다. 누락된 locked 의존성은 받을 수 있고 오프라인 실행은 호출자가
명시적으로 선택한다. 자동 업데이트는 체크아웃의 로컬 수정을 보존하며 해당 실행을
blocked/skipped로 보고한다. 개인 정책은 추적되는 설치 트리 밖의 사용자 스킬에 둔다.

Incoming 설치는 도구를 다시 설치하거나 capability 선택을 덮어쓰지 않고 OMS 구성만
맞춘다. 업데이트를 소유한 부모가 core commit 뒤 선택된 도구를 갱신하며,
`--no-tools`와 rollback에서는 그 갱신도 생략한다.
Incoming revision의 설치기를 실행하므로, 이 분리 이전의 오래된 revision으로
rollback하면 그 revision의 기존 설치 동작이 적용된다.

생성된 cron·systemd 작업은 설치 시의 HOME·XDG 위치·receipt 경로·Rust 홈을 유지하고,
관리 bin과 Cargo 디렉터리를 PATH에 넣는다. 대화형 shell profile이 없어도 실행할 수
있다. 임시 `RUSTUP_TOOLCHAIN` 값은 저장하지 않으므로 새 체크아웃이 자신의 도구
체인을 선택한다. 기존 check-only 예약은 계속 확인만 한다.
명시적 tool-lock과 provider 설정 경로도 유지한다.

systemd 설치는 로그아웃 뒤에도 유지되는지 보고한다. 트리거를 설치할 때
`OH_MY_SETTING_AUTO_UPDATE_LINGER=1`을 명시하면 켜지고(호스트 권한이 필요할 수
있다), 아니면 cron을 쓴다. linger는 이 계정의 모든 사용자 서비스에 적용되므로
OMS가 기본으로 켜지 않는다.

매일 도는 업데이트는 수정 없는 체크아웃만 fast-forward하고, 수정됐거나 갈라진
체크아웃은 건너뛴다. 기존 설치의 업그레이드는 `oms update` 한 번이고, 릴리스마다
바뀌는 것은 마이그레이션 노트(현재 [MIGRATION.md](MIGRATION.md#07))에
있다.

업데이트는 `--no-doctor`를 선택하지 않으면 core receipt를 확정하기 전에 incoming
설치의 doctor를 실행한다. 이 검사가 실패하면 core 변경은 롤백 대상이다.
선택한 provider 도구는 core 반영 뒤 갱신하며 도구별 버전과 capability를 확인한다.
현재는 이 갱신 뒤에 전체 연동 점검을 다시 실행하지 않는다. 도구 갱신이 실패하면
core 업데이트는 성공했다고 구분해 보고하며 provider 바이너리는 core 롤백 대상이
아니다.

### 설치·업데이트 미리 보기

`install.sh --plan`과 `oms update --plan`은 실행이 바꿀 내용을 출력하고 아무것도 바꾸지 않는다.
clone·fetch, lifecycle lock, link, 백업 이동, 설정 편집, 등록이 모두 일어나지 않는다. 각 줄은
`STEP: STATUS: detail`이고 status는 `change`, `current`, `skip`, `not-previewed`, `note` 중 하나다.

쓰기 없는 모드가 있는 단계(link와 백업, capability profile, Claude hook 행, Antigravity 권한 규칙,
auto-update trigger)는 그 단계 자신의 코드로 미리 본다. 나머지는 로직을 따로 흉내 내지 않고
`not-previewed`로 이름만 적는다: clone·fetch, 네이티브 payload 빌드, tool installer와 그것이 추가하는 PATH 줄,
서비스 로그인, Codex plugin과 `config.toml` 블록, MCP·Antigravity plugin 등록, Claude의 `statusLine`·
`modelSettings`·`attribution` key. plan은 디스크에 있는 checkout을 읽으므로, update plan은 fetch가 가져올
commit이 아니라 현재 commit을 다시 link하는 경우를 설명한다. checkout이 아직 없으면(첫 설치 전 curl 형태)
그렇게 말하고 멈춘다.

기존 checkout의 상세 설치 계획을 출력하려면 검증된 네이티브 payload가 준비되어 있어야 한다.
payload가 없으면 `install.sh --plan`은 준비가 필요하다고 알리고, 바이너리 빌드나 의존성
다운로드를 실행하지 않는다.

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
