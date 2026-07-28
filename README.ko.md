# oh-my-setting

Codex, Claude Code, Antigravity에 같은 규칙·스킬·agent harness를 모든 머신에
깔아주는 설정.

**직접 실행할 일은 없다.** 사람이 치는 명령은 설치 한 줄뿐이고, 그 뒤로 이
하네스는 사람의 것이 아니라 에이전트의 것이다. 도구(`oms ...`)는 에이전트가
작업 도중에 부르고, 상태(`.oms/`)도 에이전트가 쓰며, 업데이트·상태 점검·다른
에이전트 CLI의 권한 설정까지 전부 에이전트에게 말해서 시킨다. 이 문서에 명령이
보인다면 그건 에이전트가 실행할 것을 보여주는 것이다.

[English](README.md)

## 설치

`main`의 최신 버전을 설치한다:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

하네스와 함께 이 하네스가 조율하는 CLI가 설치된다: Claude Code, Codex,
Antigravity, 그리고 Node(nvm 경유), uv, GitHub CLI. peer가 하나만 설치된 council은
council이 아니므로, peer는 기억해야 할 플래그가 아니라 설치의 일부다. root는
필요 없다 — npm global은 nvm prefix로, `gh`는 `~/.local/bin`으로 간다. 설치할 수
없는 머신에서는 `--no-tools`를 넘기고, 대화형 단계인 `gh auth login`은 한 번
직접 실행한다. 이후 설치 확인·업데이트·맞춤 설정은 coding agent에게 말하면 된다.

## 시작

아무 디렉토리에서나 — 빈 디렉토리든, 진행 중이던 코드든 — coding agent를
열고 말한다:

```text
이 프로젝트 시작하자.
```

agent가 상태를 감지해서 라우팅한다:

- 빈 디렉토리 → spec 인터뷰 → `PROJECT.md` → 템플릿 → 안전한 skeleton → doctor
- 기존 repo → 코드 먼저 읽고 템플릿 적용, `PROJECT.md`는 코드에서 채우고
  빈칸만 인터뷰
- 진행 중 프로젝트 → `PROJECT.md` 읽고 doctor 실행, 상태와 다음 스텝 보고

아키텍처에 영향을 주는 작업은 관련 spec 결정이 확정될 때까지 기다리며,
범위와 로컬 계약이 명확한 변경은 확인 후 바로 진행할 수 있다.

## 이렇게 말하면 된다

프로젝트:

```text
이 프로젝트 시작하자.
oh-my-setting ml 템플릿 적용해줘.        # 또는: general, slurm
oh-my-setting project doctor 돌려줘.
공개 repo니까 agent용 파일은 git에 안 올라가게 해줘.
```

멀티에이전트 작업:

```text
현재 diff를 peer review로 검토해줘.
debate 1라운드로 세 모델에게 물어봐줘: vector DB와 pgvector 중 뭐가 맞을까?
codex에게 위임해줘: scripts/train.py에 입력 검증 추가.
이 split 정책 다른 에이전트한테도 물어보고 대화 이어서 기록해줘.
```

ML·HPC:

```text
이 분자 데이터셋 split에 leakage 없는지 훈련 전에 확인해줘.
이 실험을 run 전에 가설주도 형태로 정리해줘.
Slurm job 12345가 끝나면 로그를 요약해서 보고해줘.
이 훈련은 single-GPU 박스 큐에 넣어줘.
```

유지보수:

```text
oh-my-setting 설치 상태 확인해줘.
설치된 AI 모델 route와 provider CLI 호환성 확인해줘.
oh-my-setting 업데이트하고 doctor 다시 돌려줘.
```

## 구성 요소

아래 전부 코딩 agent가 필요할 때 알아서 호출해 쓴다 — 채팅으로 의도만 말하면
agent가 맞는 스크립트나 skill을 고른다. 직접 실행할 일은 없다. 능력 그룹만
요약하고, 스크립트별 전체 카탈로그는
[docs/COMPONENTS.md](docs/COMPONENTS.md)에 있다.

| 능력 | 무엇을 주나 |
|---|---|
| 프로젝트 부트스트랩 | Start router + 단계별 spec 인터뷰, general/ml/slurm 템플릿, `PROJECT.md` 게이트, 세 agent가 같은 규칙을 보는지 검증하는 project doctor |
| 깔끔한 공개 repo | 프로젝트별 하네스(`AGENTS.md`, `CLAUDE.md`, `PROJECT.md`)를 `.git/info/exclude`로 로컬에서만 숨긴다 — agent는 그대로 읽고, commit과 프로젝트 `.gitignore`에는 하네스 잔여물이 남지 않는다 |
| 에이전트 간 대화 | 작업 중에 다른 에이전트에게 묻고 그 맥락을 유지한다. 모든 provider가 읽고 이어 쓰는 공유 thread 하나로, codex·claude·antigravity가 서로의 답변 위에 쌓아 올린다 (일회성 질문이 아니라) |
| Multi-agent 리뷰·위임 | 세 로컬 모델의 ask/review와 격리 worktree write 위임 — outbound 민감정보 scrub, run artifact/index, 변경 범위 guard, 적용 전 patch admission까지 |
| Agent 상태·자율 핸드오프 | 되돌릴 수 있는 Markdown 원본과 프로젝트 로컬 SQLite 인덱스를 함께 쓰는 공유 메모리, 실제 검증을 실행하는 task packet, subtask DAG, 범위가 고정된 task 하나만 claim·격리 위임하고 명시적 landing 전에는 review에서 멈추는 `plan-run` — 모두 repo root에 앵커됨 |
| ML 실험 추적 | Run id, ledger, 재현 캡슐, 사전등록 research run, metric/verdict 기록 — 깨진 계약으로 run을 날리지 않게 하는 게이트 포함 |
| ML 데이터·leakage | ID와 chem-bio key(scaffold/inchikey/cluster/assay) 기준으로 train/eval leakage와 split drift를 잡는 manifest; raw row는 저장 안 함 |
| ML/HPC 지원 | Slurm job reconcile, 단일 머신 GPU 큐, 로그 digest, 로컬 하드웨어/클러스터 컨텍스트 ([docs/COMPONENTS.md](docs/COMPONENTS.md)) |
| 재사용 코드 source | 신뢰 파일 로컬 registry + GitHub fetch ([docs/COMPONENTS.md](docs/COMPONENTS.md)) |
| 유지보수 | 설치/업데이트/doctor, provider·model capability 검사, pre-push 검증 게이트, 복원 가능한 cleanup/uninstall |

## 참고

- 로컬 우선: 기본은 로컬 파일과 CLI다. 명시적으로 요청했거나 로컬 자료만으로 신뢰성 있게 답할 수 없을 때 connector를 허용한다.
- 공유 하네스 쓰기는 파일별 lock을 쓴다; `OMS_LOCK_TIMEOUT`이 대기/stale 복구
  시간(기본 `300`초)을 정한다.
- 토큰, API key, private data, cluster/머신 상세는 commit하지 않는다.
- 프로젝트별 agent 파일은 로컬 전용이라 새로 clone하거나 `git clean -x`를 하면
  사라진다. 그때는 그 체크아웃에서 템플릿을 다시 적용하면 된다 (한 문장 요청).
- agent가 실행하는 스크립트는 `~/.oh-my-setting/scripts/`에 있고 PATH의
  dispatcher로 `oms <tool>`로도 부른다 (`oms list`가 카탈로그 출력) — 투명성과
  복구용 문서화일 뿐, 직접 실행할 일은 없다.

## Star

도움이 됐다면: [github.com/eightmm/oh-my-setting](https://github.com/eightmm/oh-my-setting)
