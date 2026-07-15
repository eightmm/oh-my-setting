# oh-my-setting

Codex, Claude Code, Antigravity에 같은 규칙·스킬·agent harness를 모든 머신에
깔아주는 설정. 설치 후 모든 사용은 coding agent와의 대화로 이루어진다 —
터미널에 직접 칠 명령이 없다.

[English](README.md)

## 설치

`main`의 최신 버전을 설치한다:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

안전한 기본 구성으로 설치되며, 머신에 이미 있는 provider를 연결한다. 이후 설치
확인·업데이트·맞춤 설정은 coding agent에게 말하면 된다.

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
```

멀티에이전트 작업:

```text
현재 diff를 peer review로 검토해줘.
debate 1라운드로 세 모델에게 물어봐줘: vector DB와 pgvector 중 뭐가 맞을까?
codex에게 위임해줘: scripts/train.py에 입력 검증 추가.
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
| Multi-agent 리뷰·위임 | 세 로컬 모델의 ask/review와 격리 worktree write 위임 — outbound 민감정보 scrub, run artifact/index, 변경 범위 guard, 적용 전 patch admission까지 |
| Agent 상태·자율 핸드오프 | 공유 메모리, 실제 검증을 실행하는 task packet, subtask DAG, 범위가 고정된 task 하나만 claim·격리 위임하고 명시적 landing 전에는 review에서 멈추는 `plan-run` — 모두 repo root에 앵커됨 |
| ML 실험 추적 | Run id, ledger, 재현 캡슐, 사전등록 research run, metric/verdict 기록 — 깨진 계약으로 run을 날리지 않게 하는 게이트 포함 |
| ML 데이터·leakage | ID와 chem-bio key(scaffold/inchikey/cluster/assay) 기준으로 train/eval leakage와 split drift를 잡는 manifest; raw row는 저장 안 함 |
| ML/HPC 지원 | Slurm job reconcile, 단일 머신 GPU 큐, 로그 digest, 로컬 하드웨어/클러스터 컨텍스트 ([docs/COMPONENTS.md](docs/COMPONENTS.md)) |
| 재사용 코드 source | 신뢰 파일 로컬 registry + GitHub fetch ([docs/COMPONENTS.md](docs/COMPONENTS.md)) |
| 유지보수·릴리스 | 설치/업데이트/doctor, pre-push 검증 게이트, 복원되는 cleanup/uninstall, tag 기반 릴리스 ([docs/RELEASE.md](docs/RELEASE.md)) |

## 참고

- 로컬 우선: 기본은 로컬 파일과 CLI다. 명시적으로 요청했거나 로컬 자료만으로 신뢰성 있게 답할 수 없을 때 connector를 허용한다.
- 공유 하네스 쓰기는 파일별 lock을 쓴다; `OMS_LOCK_TIMEOUT`이 대기/stale 복구
  시간(기본 `300`초)을 정한다.
- 토큰, API key, private data, cluster/머신 상세는 commit하지 않는다.
- agent가 실행하는 스크립트는 `~/.oh-my-setting/scripts/`에 있고 PATH의
  dispatcher로 `oms <tool>`로도 부른다 (`oms list`가 카탈로그 출력) — 투명성과
  복구용 문서화일 뿐, 직접 실행할 일은 없다.

## Star

도움이 됐다면: [github.com/eightmm/oh-my-setting](https://github.com/eightmm/oh-my-setting)
