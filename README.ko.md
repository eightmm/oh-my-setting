# oh-my-setting

Codex, Claude Code, Antigravity에 같은 규칙·스킬·agent harness를 모든 머신에
깔아주는 설정. 설치 후 모든 사용은 coding agent와의 대화로 이루어진다 —
터미널에 직접 칠 명령이 없다.

[English](README.md)

## 설치

유일한 shell 단계:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

기본 설치는 최소 구성이다. 규칙·스킬·dispatcher와 이미 설치된 provider용
hook만 연결하며, provider CLI 설치, `.bashrc` 수정, machine snapshot, update
timer, star prompt는 실행하지 않는다. 기존 all-in-one 구성이 필요하면:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash -s -- --full
```

`--full`로 새 CLI가 설치되었다면 새 shell을 한 번 연다. 이후는 전부 agent가 실행한다.

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

리뷰·자문 (세 로컬 모델 병렬):

```text
현재 diff를 peer review로 검토해줘.
이 diff를 gated peer review로 pass/fail 판정해줘.
이 diff에 ML pre-training 리뷰 게이트 돌려줘.
Claude에 직접 호출하지 말고 이 diff용 review prompt만 export해줘.
이 Claude 답변을 artifact index에 import해줘.
debate 1라운드로 세 모델에게 물어봐줘: vector DB와 pgvector 중 뭐가 맞을까?
codex에게 위임해줘: scripts/train.py에 입력 검증 추가.
검증 실패하면 repair 라운드 최대 2번까지 돌려서 codex에게 위임해줘.
이 작업 전용 executor soul을 만들고 freeze한 다음 위임해줘.
codex의 patch를 admit 게이트(checks ladder) 돌려서 적용해줘.
```

재사용 코드 source:

```text
내 GitHub profile 보고 재사용할 equivariant GNN 코드 찾아봐.
flowfrag/equivariant.py를 flowfrag-equivariant로 등록해줘.
flowfrag-equivariant를 이 프로젝트로 가져와줘.
```

실험 (ML):

```text
이 훈련을 run ledger 통해서 실행해줘, note는 "lr sweep".
이 eval의 metrics.json을 ledger row에 기록해줘.
최근 ledger 10개 보여줘.
val_auc 기준 상위 run 보여줘.
오늘 이 repo에서 에이전트들이 뭐 했는지 timeline 보여줘.
Slurm job 12345 끝날 때까지 기다렸다가 digest해서 보고해줘.
이 run을 config.yaml과 seed 7까지 재현 캡슐로 저장해줘.
ckpt/best.pt는 어느 run에서 나온 거야?
run A랑 run B를 비교해서 metric 차이 보여줘.
이 분자 데이터셋 split에 leakage 없는지 훈련 전에 확인해줘.
이 실험 board에 claim 걸어줘, 다른 에이전트가 중복 실행 못 하게.
내 Slurm job들 reconcile해서 최종 상태를 shared memory에 기록해줘.
이 훈련은 single-GPU 박스 큐에 넣어서 차례 기다리게 해줘.
이 실험을 run 전에 가설주도 형태로 정리해줘.
metric val_auc/scaffold 기준 registered research run으로 실행해줘.
이 가설이랑 실험 설계를 훈련 전에 세 모델로 공격해줘.
```

메모리·핸드오프:

```text
이 repo에서는 완료 주장 전에 scripts/check.sh fast 돌린다는 걸 기억해줘.
이 repo pin으로 저장해줘: 현재 작업은 dataloader 리팩터.
active task packet 보여줘.
공유 plan에서 만료된 claim(방치된 review 포함) 회수해줘.
내 마지막 Codex 세션 여기로 핸드오프해서 이어서 해줘.
```

유지보수:

```text
oh-my-setting 설치 상태 확인해줘.
oh-my-setting 업데이트하고 doctor 다시 돌려줘.
skill picker 중복 항목 고치고 legacy oh-my-setting 링크 정리해줘.
oh-my-setting 연결 해제해줘.              # 또는: 완전히 제거해줘
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
| Agent 상태·핸드오프 | 공유 메모리, active task packet, 작업 분할용 subtask plan DAG(`agent-plan`), 세션 transcript 핸드오프 — 전부 작성 agent로 attribution되고 repo root에 앵커되어 어느 하위 디렉토리에서든 하나의 상태를 봄 |
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
