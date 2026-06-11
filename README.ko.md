# oh-my-setting

Codex, Claude Code, Antigravity에 같은 규칙·스킬·워크플로를 모든 머신에
깔아주는 설정. 설치 후 모든 사용은 coding agent와의 대화로 이루어진다 —
터미널에 직접 칠 명령이 없다.

[English](README.md)

## 설치

유일한 shell 단계:

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

새로 설치된 CLI를 못 찾으면 새 shell을 한 번 연다. 이후는 전부 agent가 실행한다.

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

spec이 확정되기 전에는 코드를 짜지 않는다.

## 이렇게 말하면 된다

프로젝트:

```text
이 프로젝트 시작하자.
oh-my-setting ml 템플릿 적용해줘.        # 또는: general, slurm
oh-my-setting project doctor 돌려줘.
```

리뷰·자문 (세 로컬 모델 병렬):

```text
현재 diff를 multi-agent review로 검토해줘.
이 diff에 ML pre-training 리뷰 게이트 돌려줘.
debate 1라운드로 세 모델에게 물어봐줘: vector DB와 pgvector 중 뭐가 맞을까?
codex에게 위임해줘: scripts/train.py에 입력 검증 추가.
```

실험 (ML):

```text
이 훈련을 run ledger 통해서 실행해줘, note는 "lr sweep".
최근 ledger 10개 보여줘.
Slurm job 12345 끝날 때까지 기다렸다가 digest해서 보고해줘.
이 분자 데이터셋 split에 leakage 없는지 훈련 전에 확인해줘.
이 실험을 run 전에 가설주도 형태로 정리해줘.
```

메모리·핸드오프:

```text
이 repo에서는 완료 주장 전에 scripts/check.sh fast 돌린다는 걸 기억해줘.
이 repo pin으로 저장해줘: 현재 작업은 dataloader 리팩터.
active task packet 보여줘.
```

유지보수:

```text
oh-my-setting 설치 상태 확인해줘.
oh-my-setting 업데이트하고 doctor 다시 돌려줘.
oh-my-setting 연결 해제해줘.              # 또는: 완전히 제거해줘
```

## 구성 요소

- **Start router + spec 인터뷰** — 진입 문구 하나; 단계별 인터뷰, `PROJECT.md`
  확정 후 템플릿·skeleton·doctor까지 한 번에.
- **Multi-agent 워크플로** — review(diff 게이트, ML 체크리스트, debate, 합성),
  ask(독립 의견), delegate(격리된 git worktree, 리뷰 가능한 patch 회수).
  artifact는 `.oms/artifacts/`에 저장.
- **안전장치** — 외부 CLI 호출 전 outbound prompt scrub(credential, private
  key, 머신 경로, cluster 상세 감지 시 호출 차단); 주입 context는 reference
  data로 fence; diff는 sanitize.
- **공유 메모리 + task 핸드오프** — compact cross-agent 메모리(`.oms/memory/`)와
  active task packet(`.oms/task/current.md`)으로 세 agent 누구든 같은 작업을
  이어감; task close 시 결론이 메모리로 승격.
- **ML 가드레일** — 실험 run ledger(pre-flight `check.sh` 게이트, 중복 실행
  경고), 스캐폴딩되는 `ml_smoke.py` one-batch 계약, ML 특화 리뷰 게이트,
  chem-bio 도메인 체크리스트(split/leakage, label, metric), 가설주도
  research-method 루프(falsifiable 가설, metric 사전등록, baseline, anti-pattern
  가드), 긴 로그 digest(`--wait`로 Slurm job 종료까지 대기), 머신/Slurm 스냅샷.
- **프로젝트 템플릿 + doctor** — general/ml/slurm managed rule block과 모든
  agent가 같은 규칙을 보는지 검증하는 doctor.

## 참고

- 로컬 우선: MCP server, app connector, plugin connector tool 사용 안 함.
- 토큰, API key, private data, cluster/머신 상세는 commit하지 않는다.
- agent가 실행하는 스크립트는 `~/.oh-my-setting/scripts/`에 있다 — 투명성과
  복구용 문서화일 뿐, 직접 실행할 일은 없다.

## Star

도움이 됐다면: [github.com/eightmm/oh-my-setting](https://github.com/eightmm/oh-my-setting)
