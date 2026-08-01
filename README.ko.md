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
Node(nvm 경유), uv, GitHub CLI. peer 하나만 깔린 council은 council이 아니므로
peer는 기억해야 할 플래그가 아니라 설치의 일부다. root 권한은 필요 없다. 설치할
수 없는 머신에서는 `--no-tools`를 붙이고, `gh auth login`은 대화형이라 한 번
직접 실행한다.

Claude Code에는 간결한 status-line HUD도 기본으로 설치된다. 모델, 실시간 context
bar와 token, Claude가 제공할 때의 5시간/7일 plan 사용량, 예상 session 비용,
reasoning effort를 한 줄로 보여준다. 로컬 렌더러라 API 호출이나 token 소비는
없다. 사용자가 이미 만든 `statusLine`은 건드리지 않고, update/uninstall도
oh-my-setting이 소유한 command만 갱신하거나 제거한다.

Antigravity는 standing permission이 있어야 headless council 호출에 답한다.
없으면 peer 호출이 조용히 거부되는데, 기본 설치는 무엇이 거부될지 보고만 하고,
`--peer-permissions`를 주면 설치 시점에 consult 프로파일(`read_file(*)`,
`command(*)`)을 부여한다(넓힌 설정 파일 옆에 `.bak` 보존). Codex와 Claude
Code는 호출 단위로 권한을 받으므로 여기 해당 없음.

선택적인 Work Journal Notion mirror는 설치할 때
`--notion-data-source ID`를 주면 된다. 프로세스 환경에
`OMS_WORK_JOURNAL_NOTION_TOKEN`이 있으면 연결과 schema까지 검증하고, 디스크에는
비밀이 아닌 ID와 property mapping만 저장한다. 자세한 내용은
[docs/WORK-JOURNAL.md](docs/WORK-JOURNAL.md)에 있다.

| 호스트 | 필요 | 관리 파일 |
|---|---|---|
| Linux, WSL | Bash 3.2+, Git, Python 3.9+ | symlink |
| macOS | 기본 Bash 3.2, Git, Python 3.9+ | symlink |
| Windows Git Bash | Git, Python 3.9+; Node와 provider CLI는 각자의 Windows 설치기로 | 검증된 사본 |

Windows는 Git for Windows가 symlink 권한을 가정할 수 없어 사본을 쓴다. 선택된
모드는 설치 영수증에 기록되어 update·doctor·uninstall이 같은 소유권 계약을
공유한다(`OH_MY_SETTING_LINK_MODE=auto|symlink|copy`로 덮어쓴다). 네이티브
PowerShell은 실행 표면이 아니다 — Git Bash나 WSL을 쓴다. 예외 상황 상세는
[docs/COMPONENTS.md](docs/COMPONENTS.md)에 있다.

## 시작

코딩 agent를 아무 디렉터리에서든 열고 — 빈 디렉터리, 진행 중 프로젝트, 기존
repo — 이렇게 말한다:

```text
이 프로젝트 시작해줘.
```

agent가 상태를 판별해 분기한다. 빈 디렉터리는 spec 인터뷰 → `PROJECT.md` →
템플릿 → doctor로, 기존 repo는 코드를 먼저 읽고 빈 곳만 인터뷰하며, 진행 중
프로젝트는 상태와 다음 할 일을 보고한다. 아키텍처를 규정하는 작업은 그것이
의존하는 결정을 기다리고, 경계가 분명한 변경은 로컬 계약을 확인해 그대로
진행한다.

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

전부 agent가 필요할 때 알아서 집어 쓴다. 스크립트별 전체 카탈로그는
[docs/COMPONENTS.md](docs/COMPONENTS.md)에 있다.

모든 설치는 동일한 범용 skill 5개를 노출한다. 무관한 세션이 쓸 수 없는 도메인
context를 지지 않도록, 나머지 두 계층은 해당되는 곳에서만 스스로 붙는다.
머신 조건 skill은 필요한 명령이 있는 머신에만 링크되고(클러스터의 `slurm`,
`nvidia-smi`가 있는 `gpu-workstation`), 프로젝트 skill은 저장소의
`.oms/skills/`에 만들어진다 — ML 템플릿이 실험·데이터셋 안전 규율을 설치하고,
`oms skill-forge`는 그 저장소를 검사해 확인한 사실을 저장한다. 세 CLI 모두 각
계층을 네이티브로 읽는다.

**프로젝트 설정**

- 빈 디렉터리·기존 repo·진행 중 프로젝트를 판별하는 Start router
- 아키텍처를 규정하는 작업을 막아 세우는 단계별 spec 인터뷰
- `general`/`ml`/`slurm` 템플릿과 `PROJECT.md` 계약
- 세 agent가 같은 규칙을 읽는지 검증하는 project doctor
- 프로젝트별 agent 파일을 로컬에서만 숨겨, 공개 commit과 프로젝트
  `.gitignore`에 하네스 잔여물이 남지 않게 한다

**다른 에이전트에게 묻기**

- 작업 중 peer에게 묻고, 모든 provider가 읽고 이어 쓰는 공유 thread 하나로
  맥락을 유지한다 (일회성 질문이 아니라)
- 같은 질문을 세 모델에 보내고 **독립 모델 family가 몇 개 답했는지** 보고하는
  council, 필요하면 debate 라운드
- 독립 리뷰어 세 명 + 프로젝트 자체 검사의 기계적 실행이 뒤를 받치는 리뷰
  게이트 — 스스로 통과했다는 주장으로는 실패하는 diff를 통과시킬 수 없다
- 결정 시점에 다른 CLI family로 라우팅되는 advisor 패스
- 프롬프트가 나가기 전 민감정보 scrub, 그리고 provider가 headless로 동작하는 데
  필요한 상시 권한을 부여하는 한 줄 명령

**쓰기 위임**

- 격리된 git worktree에 write 작업을 위임하고 리뷰 가능한 patch를 받는다
- 적용 전 admission 사다리: 클린 트리, 검증, 그리고 **단정이나 테스트 파일을
  지워서 통과하는 patch 거부**
- 변이 경계는 하나(`patch-land`), 저장소당 landing은 한 번에 하나
- Executor soul — 해시로 동결된 행동 명세, 경로 허용목록, 동결된 검증 명령,
  task lease 결속 — 로 워커의 권한을 사후에 증명할 수 있다
- 어느 provider의 워커에도 주입되는 재사용 role 프로필
- 설정·remote·ref·hook·공유 상태에 걸친 워커 권한 지문, 그리고 워커가 워커를
  못 만들도록 하는 위임 깊이 상한

**Agent 상태와 핸드오프**

- 자동 Work Journal 프로젝트 메모리: 구조화된 결과를 sanitize해 재생성 가능한
  일간·주간 로컬 요약을 증분 색인하고, 에이전트는 `oms journal show`와 하루
  한 번의 프롬프트 다이제스트로 다시 읽으며, 설치 때 구성한 Notion mirror에는
  마감된 기간만 필요할 때 보낸다
- Claude Code·Codex가 세션을 compact해 지우기 직전에 자동으로 캡처되는 세션
  핸드오프 다이제스트 — 프로젝트의 `.oms/handoffs/`에 남고, 일간 다이제스트가
  최신 것을 가리킨다
- 되돌릴 수 있는 Markdown 원본 + 검색 가능한 로컬 인덱스의 공유 메모리
- 검증이 주장이 아니라 실제로 실행되는 task packet
- lease가 있는 subtask DAG, 그리고 범위가 고정된 task 하나만 claim해 격리
  위임하고 명시적 landing 전에는 review에서 멈추는 실행
- 명령 지문으로 키를 잡는 fail-ledger — 같은 것이 미해결로 두 번 실패하면
  advisor를 지목한다. harness가 적용된 저장소에서는 Bash 도구 명령이 실패할
  때마다 Claude Code 훅이 자동으로 채운다
- append-only JSONL 상태 계약과 검증기·복구 도구

**ML과 HPC**

- 명령·artifact·출력을 하나로 묶는 run id와 run spine
- 재현 캡슐, 그리고 출력에서 그것을 만든 run으로 되짚기
- 예측·baseline·metric을 미리 고정하는 사전등록 가설 run, 병렬 agent가 실험을
  중복하지 않게 하는 experiment board
- ID와 선언된 group key 기준으로 train/eval leakage와 split drift를 잡는
  dataset manifest — raw row는 저장하지 않는다
- Slurm job reconcile, 단일 머신 GPU 큐, agent 컨텍스트에 맞춘 로그 digest,
  로컬 하드웨어/클러스터 스냅샷

**Provider와 모델**

- CLI별 capability 프로브, 바이너리 신원에 캐시
- 티어 라우팅(fast/balanced/deep)과 모델이 없을 때의 호출 시 폴백, provider
  호환성과 family 다양성 진단

**유지보수**

- 롤백 가능한 트랜잭션 업데이트, 설치 doctor, 상태 보고
- 선택적 auto-update 트리거
- pre-push hook으로 걸리는 검증 게이트 하나
- 교체한 것을 복원하는 cleanup·uninstall

## 참고

- 로컬 우선: 기본은 로컬 파일과 CLI다. 명시적으로 요청했거나 로컬 자료만으로 신뢰성 있게 답할 수 없을 때 connector를 허용한다.
- 공유 하네스 쓰기는 파일별 lock을 쓴다. `OMS_LOCK_TIMEOUT`이 대기/stale 복구
  시간(기본 `300`초)을 정한다.
- 토큰, API key, private data, cluster/머신 상세는 commit하지 않는다.
- 프로젝트별 agent 파일은 로컬 전용이라 새로 clone하거나 `git clean -x`를 하면
  사라진다. 그때는 그 체크아웃에서 템플릿을 다시 적용하면 된다.
- 스크립트는 `~/.oh-my-setting/scripts/`에 있고 `oms <tool>`로도 부른다
  (`oms list`가 카탈로그 출력). 투명성과 복구용 문서화일 뿐이다.

## Star

도움이 됐다면: [github.com/eightmm/oh-my-setting](https://github.com/eightmm/oh-my-setting)
