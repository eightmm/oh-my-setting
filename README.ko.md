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

기본 설치는 `core` capability만 선택한다. 즉 하네스, Bash, Git, Python,
그리고 coding-agent provider 하나만 필수다. GitHub CLI, Notion CLI, provider
세 개 전부, 연구 도구, 클러스터 도구는 기본 필수 의존성이 아니다. 예전의
all-provider/GitHub/Notion/research 구성이 필요한 머신에서만 `full` 호환
profile을 사용한다. root 권한은 필요 없다. 관리 도구 버전·플랫폼 URL·무결성
값은 `tools.lock.json`에 고정되고 새 다운로드는 사용 전에 검증된다. 이미 있는
외부 CLI는 정확한 버전이면 재사용하고 doctor가 version-only로 표시한다.
install/update/repair/uninstall은 사용자 단위 lifecycle lock 하나를 공유한다.

capability profile은 `core`, `council`, `github`, `notion`, `research`, `hpc`,
`container`, `remote`, `full`이다. 선택형 설치기는 기존 locked downloader와
transaction을 그대로 재사용하고, 요청한 profile을 private receipt에 기록해
업데이트 때도 그 도구 집합만 다시 적용한다. capability receipt가 없는 기존
설치는 agent가 명시적으로 마이그레이션하기 전까지 legacy full-tool update
경로를 유지한다. 자세한 내용은
[docs/OMS-RUNTIME.md](docs/OMS-RUNTIME.md)에 있다.

GitHub 또는 Notion capability를 선택한 경우 대화형 설치기는 `gh auth login`과
`ntn login`의 브라우저 로그인을 위임하고 Work Journal의 Notion 대상을 찾을 수
있다. 비대화형 설치에서는 core runtime을 약화시키지 않고 누락된 capability를
명시적으로 기록한다. Claude Code에는 메인·서브에이전트 HUD가
설치되고, Codex에는 사용자가 직접 정한 footer가 없을 때 같은 목적의 내장
footer가 기본 설정된다. 매일 실행되는 updater는 깨끗한 checkout만 기본적으로
fast-forward하며 dirty 또는 diverged checkout은 건너뛴다. uninstall은 관리
설정을 복원하지만 외부 CLI와 user-local PATH 항목은 남긴다.

설치는 전역 에이전트 규칙 파일 세 개 — `~/.claude/CLAUDE.md`,
`~/.codex/AGENTS.md`, `~/.gemini/AGENTS.md` — 를 관리한다. 기존 파일이 있으면
`<파일>.backup.<타임스탬프>`로 이동되고, 설치가 유지되는 동안 적용되지 않으며,
`oms uninstall`이 복원한다.

| 호스트 | 필요 | 관리 파일 |
|---|---|---|
| Linux, WSL | Bash 3.2+, Git, Python 3.9+ (또는 uv) | symlink |
| macOS | 기본 Bash 3.2, Git, Python 3.9+ (또는 uv) | symlink |
| Windows Git Bash | Git, Python 3.9+, lock과 정확히 같은 네이티브 Node | 검증된 사본 |

예외 상황 — Windows 사본 모드, Antigravity headless 권한, Notion data source
지정 — 은 [docs/COMPONENTS.md](docs/COMPONENTS.md)와
[docs/WORK-JOURNAL.md](docs/WORK-JOURNAL.md)에 있다. 기존 설치의 업그레이드는
`oms update` 한 번이고, 릴리스마다 무엇이 바뀌는지는 마이그레이션 노트 —
현재 [docs/MIGRATION-0.6.md](docs/MIGRATION-0.6.md) — 에 명시된다.

## 시작

코딩 agent를 아무 디렉터리에서든 열고 — 빈 디렉터리, 진행 중 프로젝트, 기존
repo — 이렇게 말한다:

```text
이 프로젝트 시작해줘.
```

agent가 상태를 판별해 분기한다. 빈 디렉터리는 spec 인터뷰 → `PROJECT.md` →
템플릿 → doctor로, 기존 repo는 코드를 먼저 읽고 빈 곳만 인터뷰하며, 진행 중
프로젝트는 상태와 다음 할 일을 보고한다.

`PROJECT.md`를 확정한 뒤에는 검토한 작업 제안 → 제한된 구현 → 합격 검사 →
Draft PR까지 맡길 수 있다. 생성된 작업이 스스로 승인되지는 않으며 merge와
release는 별도 권한으로 남는다.

## Typed Runtime Core

`oms runtime`은 기존 hardened execution plane 위에 놓인 표준 라이브러리 기반
semantic layer다. 분산된 상태를 effective TaskEnvelope로 합쳐 읽고, 완료
조건별 EvidenceCoverage를 계산하며, bounded ContextManifest를 만들고,
capability profile을 선택하고, 여러 머신 사이에 sanitized capsule을 옮기고,
로컬·container·remote backend를 정직한 receipt와 함께 실행하며, 비교 가능한
연구 실험을 평가한다.

다음 mutation 경로는 대체하지 않는다.

```text
peer-delegate -> patch-admit -> patch-land
```

plan lease, executor soul, one-use approval, commit intent, Draft PR intent가
계속 권위 경계다. runtime snapshot·capsule·context bundle·backend receipt는
증거나 advisory state일 뿐 write authority가 아니다.

간결한 agent 표면은 `oms list --frontdoor`, 기존 전체 명령은
`oms list --all`에서 볼 수 있다.

## 이렇게 말하면 된다

```text
이 프로젝트 시작해줘.
oh-my-setting ml 템플릿 적용해줘.                 # 또는 general, slurm
지금 diff 피어 리뷰 돌려줘.
세 모델에게 debate 한 라운드로 물어봐: vector DB냐 pgvector냐?
이거 codex한테 위임해줘: scripts/train.py에 입력 검증 추가.
확정된 PROJECT.md를 구현해서 Draft PR까지 만들어줘.
현재 effective task contract와 증거가 없는 완료 조건을 보여줘.
이 patch를 리뷰하는 데 필요한 최소 context만 만들어줘.
이 머신에는 core와 research capability만 준비해줘.
다른 워크스테이션으로 넘길 sanitized continuity capsule 만들어줘.
고정 seed로 실험하고 invariant가 나빠지면 지지하지 않는 것으로 판정해줘.
이 split 정책 다른 에이전트한테 물어보고 thread 유지해줘.
학습 전에 이 데이터셋 group split leakage 확인해줘.
런 돌리기 전에 가설 기반 실험으로 정리해줘.
Slurm job 12345 끝나면 로그 digest해서 보고해줘.
oh-my-setting 업데이트하고 doctor 다시 돌려줘.
```

## 구성 요소

전부 agent가 필요할 때 알아서 집어 쓴다. 간결한 agent catalog는
`oms list --frontdoor`, 기존 호환 명령 전체는 `oms list --all`, 상세 문서는
[docs/COMPONENTS.md](docs/COMPONENTS.md)다.

- **Typed semantic runtime** — effective TaskEnvelope projection,
  criterion-linked EvidenceCoverage, context manifest, 표준 failure recovery,
  optional capability profile, portable capsule, execution receipt,
  ExperimentContract v2, 내용 비저장형 harness 효과 telemetry
- **프로젝트 설정** — start router, spec 인터뷰, `general`/`ml`/`slurm`
  템플릿, 세 agent의 관리 규칙·설정 표면이 같은지 검증하는 doctor,
  로컬에서만 숨겨지는 agent 파일
- **다른 에이전트에게 묻기** — 이어지는 peer thread, 세 모델 council과
  프로젝트 자체 검사가 뒤를 받치는 리뷰 게이트, 결정 시점 advisor, 발신 전
  민감정보 scrub. 심사하는 자리는 일부러 덜 받는다: 읽기용 4개 툴, MCP 표면
  없음, 작성자의 논리 없이 증거만 — 그리고 죽거나 잘린 시트는 합성에서
  이름으로 표시될 뿐 하나의 의견처럼 인용되지 않는다
- **쓰기 위임** — 격리 worktree 위임(patch를 만들면 안 되는 감사는
  `--read-only`), admission 사다리, 단일 변이 경계, 해시 동결 executor soul과
  워커 권한 지문, 위반 시 스냅샷 기반 권한 복구, primary 권한 환경변수
  미전달, 그리고 재귀 위임 금지 — 워커가 다른 peer를 부르려 하면 서버 측에서
  거부되고 필요를 답변으로 보고하라고 안내받는다
- **Agent 상태와 핸드오프** — 일간 요약과 선택적 Notion mirror가 있는 Work
  Journal, 우선순위 attention inbox, compaction 직전 세션 핸드오프, 출처를
  재검증하는 공유 메모리, 되돌릴 수 있는 tracked-state checkpoint, 검증이
  실제로 실행되는 task packet, 반복 실패 시 advisor를 지목하는 fail-ledger,
  landing 뒤 중단돼도 provider 재호출 없이 이어지는 commit intent
- **ML과 HPC** — run spine과 재현 캡슐, 사전등록 가설 run, 데이터셋 leakage
  manifest, Slurm reconcile과 GPU 큐
- **Provider와 모델** — 캐시된 capability 프로브, provider 기본값 또는 정확한
  모델/effort 선택, 명시한 경우에만 한 번 쓰는 capacity fallback, family 다양성
  진단, provider가 제공할 때만 기록하는 내용 비저장형 native telemetry,
  그리고 provider 자신의 정지 사유를 실어 나르는 전송 — 토큰 한계에서 잘린
  답은 완성된 것처럼 읽히는 대신 fail-closed로 떨어진다
- **운영과 실행 경계** — 영속 attempt 이벤트와 child-attempt 재개, 제한된
  supervisor, 한 번만 쓰는 승인, `trusted-local`/`isolated`/`remote` preflight와
  실제 실행 backend, 선택적 Herdr 제어와 VS Code·Stably Orca·Codex 열기,
  읽기 전용 cockpit, 로컬 OTLP JSONL, advisory semantic 평가, 기본 원격 쓰기
  경로가 새 브랜치와 Draft PR 생성뿐인 제한 코딩 루프
- **유지보수** — 롤백 가능한 트랜잭션 업데이트, stable/edge channel projection,
  doctor, 하나의 전체 검증 게이트와 보호 브랜치용 빠른 pre-push 모드;
  append-only 상태는 gc로 압축되고 agent에게 보이는 projection은 전부
  경계가 있으며 생략은 침묵 대신 명시된다

skill은 세 계층으로 붙는다: 어디서나 같은 범용 skill, 필요한 명령이 있는
머신에만 링크되는 머신 조건 skill(`oms-slurm`, `oms-gpu-workstation`), 그리고
저장소의 `.oms/skills/`에 포지되는 프로젝트 skill. 전역 skill 이름은 `oms-`로
시작하고, 포지된 프로젝트 skill은 접두사 없이 그대로 둔다.

## 참고

- 로컬 우선: connector는 명시적으로 요청했거나 로컬 자료로 답할 수 없을 때만.
- 토큰, private data, cluster/머신 상세는 commit하지 않는다. portable runtime
  capsule도 sanitized advisory state이며 raw `.oms`는 자동 동기화하지 않는다.
- 스크립트는 `~/.oh-my-setting/scripts/`에 있고 `oms <tool>`로 부른다. 투명성과
  복구용 문서화일 뿐 사용자가 직접 실행하는 명령 집합이 아니다.

## Star

도움이 됐다면: [github.com/eightmm/oh-my-setting](https://github.com/eightmm/oh-my-setting)
