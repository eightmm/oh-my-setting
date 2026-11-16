# OMS Multi-Agent · Multi-Host 구현 방향

작성: 2026-11-16. 상태: **Stages A–F 순수 Rust 구현 및 결정적 fault-injection 검증 완료 (PR #224 / `main` 통합 대기); Stage G 미착수**.
검토 기준: `c9919a1021dfb610a5a0d71b3d733d752e9bdd13`.

## 1. 목표와 적용 범위

여러 프로젝트·코딩 에이전트·원격 호스트·GPU 클러스터를 같은 작업 모델로 다룬다.
목표는 실행 중인 에이전트 수가 아니라 **검증을 통과해 통합된 작업의 처리량과 재현성**이다.

기본 방향은 **Worktree-first execution, PR-gated integration**이다. Git은 소스 이력을,
GitHub는 공유 변경의 검토·통합을, OMS는 작업 배정·컨텍스트·실행·검증·복구를 맡는다.
SSH는 전송·제어 수단이며, 실제 계산 자원 배치는 기존 Slurm 등 실행 환경에 맡긴다.
GitHub를 사용할 수 없는 환경에서도 로컬 개발·검증·결과 보존은 가능해야 한다.

이 문서는 구현할 기능, 동작 원칙, 도입 순서와 완료 기준을 정의한다. Rust crate 구성,
기존 구현 파일, 함수 시그니처, 저장 형식, 새 CLI 철자는 지정하지 않는다. 실제 구조는
구현 시점의 Rust 공통 코어와 공개 계약에 맞춰 정한다. 미래 기능을 현재 명령처럼 제시하지 않는다.

이번 문서의 채택만으로 원격 명령 실행, GPU 사용, 인증정보 배포, 자동 병합·배포를 허용하지 않는다.
[기존 control-plane 승인 경계](CONTROL-PLANE-EVOLUTION.md)를 유지한다.

## 2. 기존 기반을 재사용하는 방법

아래는 기준 커밋에 대한 앞선 검토의 요약이며, 운영 환경에서의 실행 검증은 아니다.
후속 구현을 시작할 때 최신 main과 진행 중 작업을 다시 대조한다.

| 현재 기반 | 이번 확장의 방향 |
|---|---|
| Task·Attempt·Lease, 독립 작업 공간, 검증·admission·landing | 별도 작업 시스템을 만들지 않고 동일한 소유권과 완료 의미를 확장 |
| 의존성·쓰기 범위에 따른 wave 안내 | 안내와 실제 실행을 구분하고, 결정적인 병렬 admission·실행으로 연결 |
| Advisory 파일 lease | 경고를 배타 제어로 오인하지 않음. 필요한 작업·자원에는 명시적인 실행 lease 적용 |
| SSH target 등록·Git 상태·동기화, 별도 remote adapter 계약 | Checkout 관리와 실행 권한을 계속 분리한 채 원격 실행·복구 추가 |
| Slurm 제출·실행 기록·결과 요약 | 새 GPU scheduler 대신 기존 작업의 신원·상태·결과 연결 |
| Context·출력 조회·실패 진단 개선 작업 | 병합된 계약을 재사용하고 새로운 병렬 로그·메모리 체계를 중복 생성하지 않음 |

기능과 권한의 기준은 [Components](COMPONENTS.md), [Runtime](OMS-RUNTIME.md),
[Panel](TERMINAL-PANEL.md)이다.
관측만 허용한 기능을 이번 계획에 포함됐다는 이유로 변경·실행 기능으로 승격하지 않는다.

## 3. 책임 분리: 판단은 Main, 반복 제어는 Runtime

| 역할 | 책임 | 경계 |
|---|---|---|
| Main / Integration owner | 목표·범위·계약 확정, 예외 판단, 결과 수락과 통합 책임 | 매 polling·재시도·대기열 판단을 LLM 호출로 처리하지 않음 |
| Deterministic runtime | DAG 준비 상태, lease, 예산, 병렬 실행, 복구와 통합 직렬화 | 모호한 상태를 성공으로 바꾸거나 승인 범위를 확대하지 않음 |
| Worker | 제한된 조사·구현·테스트와 결과 제출 | 자기 결과 승인, 임의 재위임, 권한 확대 금지 |
| Reviewer / Verifier | 고정된 후보와 기준에 대한 독립 확인 | 작성자의 성공 주장이나 모델 간 합의를 실행 증거로 대체하지 않음 |
| Host executor | 승인된 작업 실행, 현지 상태·산출물 보존·조회 | 중앙의 통합·publication 권한을 자동 상속하지 않음 |

Single Main 원칙은 **작업 목표/room의 소유권 경계**에서 유지한다. 하나의 전역 LLM이 모든
프로젝트를 직렬 처리할 필요는 없다. 독립 프로젝트는 병행하되, 같은 통합 대상과 공유 자원에는
단일 변경 주체를 둔다. 다른 클러스터에서 OMS를 실행해도 동일 목표의 두 번째 Main을 자동 생성하지 않는다.

## 4. 작업을 나누고 협업 모드를 선택하기

작업은 에이전트 이름이나 파일 개수가 아니라 독립적으로 검증 가능한 결과 단위로 나눈다.
각 작업은 목표·비목표·완료 기준, 의존 결과, 고정된 입력 버전, 읽기/쓰기 범위, 공유 자원,
실행 능력, 예산, 제출할 증거, 통합 대상을 가진다. 이것은 논리적 계약이며 저장 스키마의 확정안은 아니다.

| 모드 | 사용 상황 | 결과 처리 |
|---|---|---|
| Single-agent | 작거나 순차 의존성이 큰 변경 | 분업 비용을 추가하지 않고 같은 검증 경로 사용 |
| Parallel exploration | 원인 분석·설계 대안·저장소 조사 | 읽기 전용 분업 후 출처와 미확인 사항을 합침 |
| Parallel implementation | 독립 기능 또는 안정된 계약을 공유하는 구현 | 작업별 workspace, 결과별 검증 후 통합 |
| Contract-first | API·데이터 형식·공통 코어를 함께 변경 | 계약과 호환성 기준을 먼저 확정하고 구현 병렬화 |
| Competitive candidates | 해법이 불확실한 알고리즘·성능 개선 | 같은 입력·예산·고정 평가에서 후보 비교, 검증된 후보만 채택 |

다른 파일을 수정해도 API, 설정, 데이터베이스, 빌드 산출물, 테스트 포트가 충돌할 수 있다.
쓰기 범위 외에 **공유 계약과 부수효과**를 작업에 선언한다. 불확실한 공유 변경은 계약 확인 또는
직렬 실행으로 처리하며, 파일 경로가 다르다는 이유만으로 병렬 안전을 보장하지 않는다.

예: 공통 실행 계약을 먼저 합의한 뒤 원격 실행과 패널 관측을 병행하고, 별도 검증자가
응답 손실·재시작 시나리오를 확인한다. 구현자에게 검증 기준을 약화할 권한을 주지 않는다.

## 5. 실제 병렬 스케줄링

준비된 DAG 작업 중 의존성, 충돌, 자원, 예산, 권한 조건을 만족하는 작업만 실행한다.
Wave는 설명용 뷰로 유지할 수 있지만, 무관한 느린 작업 때문에 다음 준비 작업까지 기다리는
전역 wave barrier를 기본으로 두지 않는다. 순환 의존성과 알 수 없는 선행 작업은 명시적으로 거부한다.
외부 선행 결과는 확인된 버전·수락 증거로 연결하고, 목록에 없다는 이유로 완료 처리하지 않는다.

실행 admission은 필요한 lease와 예산을 모두 확보하거나 모두 반환한다. 충돌 자원을 일부만 잡고
나머지를 무기한 기다리는 상태를 피한다. 작업별 단일 writer, workspace 소유권, 통합 대상의
단일 writer를 구분한다. 서로 다른 worktree의 경쟁 후보는 허용할 수 있지만 자동 통합은 별개다.

초기 동시성은 **설정 가능한 소수 writer부터** 검증한다. 예를 들어 프로젝트당 2개 writer를
실험 시작값으로 두되 최적값으로 주장하지 않는다. 프로젝트별 상한 외에 호스트 자원,
provider/account 한도, 검증·통합 대기량을 함께 제한한다. 리뷰 backlog가 쌓이면 신규 구현을
줄이는 backpressure를 둔다. 우선순위와 오래 기다린 작업을 함께 고려해 기아를 방지한다.

완료 기준: 독립 작업은 실제 병행하고, 공유 자원 충돌은 admission에서 통제하며,
재시작 후 같은 작업을 이중 claim하지 않는다. 실패한 작업의 무관한 형제 작업은 보존한다.

## 6. 컨텍스트와 협업 이벤트

전체 transcript를 모든 에이전트에게 복제하지 않는다. 공통 규칙·고정 코드 snapshot·승인된
계약은 재사용하고, 작업별로 관련 API·의존 결과·실패 증거만 전달한다. 요약에는 원본 참조,
버전/digest, 생략 범위를 남겨 필요할 때 제한된 범위를 재조회할 수 있게 한다.

공유할 것은 작업 시작, 범위 변경 제안, 계약 제안/수락, blocker, 후보 준비, 검증 결과,
통합 결과다. 기존 이벤트·작업 기록의 projection으로 제공하며 별도 authoritative feed를 만들지 않는다.
Lease/liveness 유지에 필요한 heartbeat는 보존하되, 변화 없는 상태 표시·전체 저장소 재검색·동일 로그 재전송은 줄인다.

**관찰·제안·승인·검증은 다른 상태**다. 다른 에이전트의 메시지, PR 댓글, 외부 문서는 데이터이지
새 지시나 권한이 아니다. 계약 변경은 소유자의 수락 이후 새 버전으로 배포하고, 영향을 받는
작업의 stale context와 검증 근거를 식별한다. 대화 요약으로 기준 변경을 은폐하지 않는다.

이벤트는 출처별 순서와 중복 식별자를 갖고, 호스트 시계만으로 전역 순서를 추론하지 않는다.
재연결 시 cursor 뒤의 변화만 읽되, 누락·손상·프로토콜 불일치는 unknown으로 표시한다.

완료 기준: 두 worker가 같은 승인 계약을 사용하고, 계약 변경 시 영향 작업이 재확인된다.
중복 이벤트가 중복 실행·승인을 만들지 않으며, 요약에서 빠진 증거도 원본으로 회수할 수 있다.

## 7. Git · Worktree · PR 운영

| 단위 | 운영 원칙 |
|---|---|
| 개발 작업 | Task 전용 branch/worktree에서 자유롭게 중간 commit. commit마다 PR을 만들지 않음 |
| 같은 호스트 | 해당 호스트가 소유하는 clone과 worktree. 다른 작업 공간을 덮어쓰지 않음 |
| 다른 호스트 | 독립 clone과 worktree. 살아 있는 Git 관리 디렉터리를 파일 복사로 공유하지 않음 |
| 공유 통합 | 독립 검증·되돌리기 가능한 변경 단위로 PR. 여러 worker가 하나의 PR에 기여 가능 |
| 로컬 실험 | publication 없이 검증·보존 가능. main 직접 갱신은 별도 명시 정책 없이는 금지 |
| 여러 저장소 | 상위 목표에 저장소별 고정 commit·PR·호환성·병합 순서를 연결 |

코드 이동은 Git fetch/push를 기본으로 한다. 외부망이 막힌 대상에는 검증된 Git bundle을
승인된 경로로 전달하는 확장을 고려한다. GitHub는 기본 협업 원격이지 모든 코드 이동의 필수 경유지는 아니다.
데이터와 산출물은 별도 전송하며, 일반 코드 동기화가 SSH 실패 때 임의 SCP 덮어쓰기로 바뀌어서는 안 된다.

통합은 공유 대상별로 직렬화한다. 후보의 base/result, 실제 변경 범위, 검증한 snapshot,
리뷰·수락을 연결한다. target이 바뀌거나 충돌 수정이 발생하면 새로운 통합 후보로 재검증한다.
작업별 테스트가 통과해도 합친 결과가 통과한 것으로 간주하지 않는다. GitHub merge queue의
가용성을 전제로 하지 않으며, 기존 보호 규칙을 우회하는 별도 merge 경로도 만들지 않는다.

여러 저장소의 PR은 원자적으로 병합된다고 가정하지 않는다. 호환 가능한 단계적 변경,
통합 manifest, 부분 실패 시 보류·복구 절차를 둔다. 자동 되돌리기도 외부 부수효과가 있으면
별도 승인 없이 실행하지 않는다.

## 8. 호스트 등록과 Workspace 준비

Project/Repository와 Host/Cluster를 별도 대상으로 등록한다. Git remote와 SSH 실행 대상도
구분한다. 호스트마다 지원 기능, 신뢰 수준, 허용 workspace root, 네트워크 접근,
데이터 위치, scheduler/account, 자원·비용 제한을 관리한다. 민감한 주소·경로·인증정보는
private 현지 설정에 두고 공개 문서·공유 컨텍스트에는 논리적 ID만 남긴다.

기존 동일 HOME 상대 경로 정책을 즉시 폐기하지 않는다. 먼저 그 계약으로 동작을 유지하고,
서로 다른 scratch/data 경로 지원은 별도 승인된 root mapping으로 확장한다. 실제 경로·symlink
검증과 소유권 확인 없이 임의 절대 경로를 허용하지 않는다.

수정 가능한 개발 workspace와 고정된 실행 snapshot을 분리한다. 실행 snapshot은 commit뿐
아니라 config, 데이터 manifest, 환경 lock 또는 image digest를 묶는다. 필요한 untracked/ignored
입력도 누락 없이 선언·고정하거나 실행을 거부한다. 작업 중 코드 sync·editable 설치·공유 산출물이
실행 코드를 바꾸지 않도록 한다. 단순히 시작 SHA를 로그에 적는 것으로 불변성이 보장되지는 않는다.

산출물은 임시 영역으로 전송하고 무결성·크기·경로를 검증한 뒤 게시한다. 데이터·checkpoint는
묶음 전송 또는 rsync/객체 저장소를 사용하고 파일별 SCP 반복을 기본으로 하지 않는다.
전체 checkpoint를 매번 중앙으로 복사하지 않고 manifest·요약·필요한 결과만 회수할 수 있게 한다.
자동 삭제는 활성 작업·미수집 결과·dirty workspace를 보호하며, 보존 기간과 용량 정책을 따른다.

## 9. 원격 실행과 복구

초기는 **SSH + 원격 OMS 실행 기능**으로 시작한다. 등록·상태 조회와 실행 승인을 분리하고,
workspace 준비, 작업 시작, 상태 조회, 취소 요청, 산출물 회수 같은 제한된 동작을 제공한다.
작업 내부의 프로젝트 명령은 승인 범위와 격리 정책 안에서 실행하며, typed API 자체를 sandbox로 주장하지 않는다.
상주 daemon·추가 포트·별도 분산 DB는 초기 필수조건으로 두지 않는다.

원격 요청은 고정된 입력과 operation identity로 묶는다. 요청 의도를 영속화한 뒤 전달하고,
원격 수락·실제 실행 식별자·진행·결과를 따로 기록한다. 명령에 부수효과가 생긴 이후 응답이
사라지면 **실패가 아니라 상태 불명**일 수 있다. 새 실행을 만들기 전에 기존 operation과
실제 프로세스/Slurm 상태를 reconcile한다. 조정으로 부재를 확정할 수 없으면 보류한다.

같은 요청 재전달은 같은 operation을 조회·회수해야 한다. 의도적인 재실행은 이전 실행과
연결된 새 attempt다. 외부 scheduler의 제출과 현지 기록을 하나의 transaction으로 취급하지 않으며,
단순 idempotency key만으로 exactly-once 실행을 보장한다고 주장하지 않는다.

Lease 세대/fencing 검사는 admission뿐 아니라 결과 수락·통합 시에도 적용한다. 만료된 worker는
결과를 보존할 수는 있어도 현재 작업을 완료시키거나 공유 코드를 갱신할 권한은 없다.
연결 단절만으로 이미 실행된 계산을 멈출 수 있다고 가정하지 않는다. 원격 부수효과가 필요한
경계에서 세대 검증을 강제할 수 없으면 그 한계를 표시하고 안전한 작업 범위로 제한한다.

취소 요청, 취소 수락, 실제 종료 확인을 구분한다. 로컬 SSH 종료는 원격 worker 종료가 아니며,
worker 종료는 Slurm job 취소가 아니다. 재접속·호스트 재부팅·PID/Job ID 재사용을 구분하는
host/cluster/실행 세대 정보를 사용한다. 오래된 결과가 늦게 도착해도 현재 성공으로 덮어쓰지 않는다.

완료 기준: 응답 손실·중복 전달·재부팅·stale lease에서 중복 제출과 잘못된 완료를 방지하거나
안전하게 보류한다. 실제 복구 범위를 시험으로 입증하고, 연결 끊김을 자동 재시작 허가로 사용하지 않는다.

## 10. 클러스터 선택과 실험 추적

Agent 실행 위치와 GPU 실험 실행 위치는 독립적으로 선택할 수 있어야 한다. 모델 호출 한도,
현지 도구·환경, 데이터 접근, 예상 queue 대기, 전송량, GPU 종류·메모리, 예산을 고려한다.
초기에는 설명 가능한 규칙과 사용자 지정 정책을 사용하고, 측정 이력 없는 학습형 router는 도입하지 않는다.

로그인 노드에 무거운 학습·빌드·agent 작업을 무조건 배치하지 않는다. 해당 클러스터의 정책에
맞는 실행 위치를 사용한다. Slurm에 제출한 뒤 자원 할당은 Slurm이 맡고 OMS는 작업과 결과를 추적한다.
동일 snapshot의 여러 실험은 승인된 경우 job array 등 기존 기능으로 묶을 수 있다.

Task/Attempt, 소스·환경·config·데이터 입력, cluster와 job identity, 산출물 digest를 연결한다.
Queued/running/completed 같은 실행 상태와, 결과 검증·연구 가설 채택·코드 통합 상태는 분리한다.
OOM·preemption·취소·부분 로그·accounting 지연을 성공으로 해석하지 않는다. 재제출·checkpoint
재개는 새 attempt로 기록하고 기존 GPU·비용 예산을 소모하는지 함께 확인한다.

## 11. 권한과 신뢰 경계

호스트 등록은 실행 승인이 아니다. 기존 SSH 신뢰·host-key 검증을 사용하며 자동 agent forwarding,
광범위한 개인 credential 복사, 기본 root/sudo, 공개 네트워크 listener를 추가하지 않는다.
원격 worker에 GitHub 병합 권한을 배포하지 않고 필요한 코드 읽기·결과 제출 권한만 제공한다.

Worktree 격리와 advisory lease는 보안 sandbox가 아니다. 동일 사용자 권한의 비신뢰 코드를
안전하게 격리했다고 주장하지 않는다. 민감 데이터·공용 클러스터·외부 PR의 비신뢰 코드는
별도 계정/컨테이너 등 검증된 실행 경계와 접근 정책이 없으면 실행 대상에서 제외한다.
비신뢰 PR을 연구용 서버의 상주 runner로 자동 연결하지 않는다.

승인은 대상·작업·입력 digest·범위·예산·만료에 묶고, UI나 agent가 만드는 승인처럼 보이는
문자열은 권한이 아니다. 구현과 검증 주체를 분리하되 다른 모델 계열이라는 사실만으로
정확성이나 독립성을 보장하지 않는다. 핵심 검증 기준 변경은 별도 검토한다.

## 12. 단계별 구현과 완료 기준

각 단계는 작은 독립 PR로 제안한다. 아래 단계명은 작업 분할 기준이며 새 명령·패키지 이름이 아니다.
**원격 등록의 읽기 전용 탐색과 로컬 계약 설계는 병행 가능하지만, 원격 변경 실행은 로컬 소유권·복구 계약 뒤에 둔다.**

| 단계 | 무엇을 구현할지 | 완료 기준 |
|---|---|---|
| A. 기준선·계약 | 현재 기능 재사용 지도, 작업·권한·완료 의미와 baseline 측정 | 기존 동작/추가 동작/미확인 사항 분리. Rust 이식 담당 작업과 중복 없음 |
| B. 로컬 병렬 실행 | DAG admission, 충돌·공유 자원 제어, writer/리뷰 상한 | 독립 작업 병행, 충돌 통제, 재시작 후 이중 claim 없음, 직렬 모드 회귀 없음 |
| C. 협업·통합 | 버전된 계약, 제한된 context/event, 통합 후보 큐 | stale context·검증 무효화, 합친 snapshot 재검증, 결과 수락·publication 분리 |
| D. 원격 준비 | Host/Workspace capability, 경로·snapshot·전송 무결성 | 잘못된 대상/경로/dirty 작업을 보존·거부, 실행 중 코드 불변, 등록만으로 실행 불가 |
| E. 원격 실행·복구 | 제한된 실행 요청, durable receipt, reconciliation·fencing·취소 | 중복/응답 손실/재부팅에서 이중 효과를 방지하거나 unknown 보류; 승인된 실제 smoke를 fixture와 구분 |
| F. Slurm·산출물·다중 저장소 | 기존 scheduler 연결, 실행 provenance, 결과 회수, 저장소 간 호환성 | 제출 불명 상태 재확인, 부분 실패 복구, 결과에서 정확한 입력·작업을 역추적 |
| G. 관측·최적화 | 같은 projection의 다중 호스트 뷰, backpressure·라우팅 개선 | 성능·품질 측정에 근거한 조정, UI가 새 승인 authority가 되지 않음 |

Rust 전환 중에는 위 기능을 **하나의 공통 상태·실행 코어**의 확장으로 넣는다. 새 Python 기능을
대량 추가한 뒤 다시 이식하거나, 동일 작업을 두 runtime이 동시에 소유하는 구조를 만들지 않는다.
필요한 기존 공통 계약이 아직 이식되지 않았다면 계약·fixture·읽기 전용 검증을 먼저 진행한다.
Writer 전환은 단일 소유권, 호환성, drain/recovery, rollback 조건을 만족한 뒤 활성화한다.

새 기능은 기존 공개 동작과 구분된 opt-in으로 시작한다. 비활성화는 신규 admission을 멈추는
행위이지 진행 작업·결과를 삭제하는 행위가 아니다. 이전 runtime이 읽을 수 없는 상태를 만들었다면
바이너리만 되돌리는 rollback을 허용하지 않는다. 저장소·프로토콜 선택은 실제 필요가 생길 때
작은 결정 기록으로 남기고, 파일 layout을 미리 고정하지 않는다.

## 13. 검증과 성능 평가

| 검증 시나리오 | 기대 결과 |
|---|---|
| 독립 작업 두 개와 공유 API 변경 | 독립 작업 병행, 계약 의존 작업은 검증된 계약 뒤 진행 |
| 검증 후 코드 또는 target 변경 | 이전 pass로 통합 불가, 새 후보 검증 |
| Worker 종료·controller 재시작 | 미수락 결과 보존, 소유권 재확인 전 중복 실행 없음 |
| 원격 수락 직후 응답 손실 | 기존 operation 재조회, 부재를 입증하지 못하면 재실행 보류 |
| Slurm 제출 후 기록 전 장애 | 실제 제출 상태 조정, 새 job을 무조건 제출하지 않음 |
| Lease 만료 뒤 이전 결과 도착 | 현재 완료·승인 상태 갱신 거부, 이전 결과는 별도 보존 |
| 시계 차이·PID/Job ID 재사용 | 다른 실행을 기존 실행으로 오인하지 않음 |
| 전송 중단·잘못된 digest·경로 탈출 | 불완전 결과 게시 금지, 원본·dirty 작업 보존 |
| 리뷰 backlog·한도 부족 | 실행 상한과 backpressure 준수, 성공/무제한으로 간주하지 않음 |
| 여러 저장소 중 한쪽 통합 실패 | 부분 완료와 영향 명시, 사전 정의된 호환성·복구 정책 적용 |

먼저 가짜 provider/SSH/scheduler와 임시 저장소로 결정적 회귀·장애 주입을 검증한다.
실제 호스트 smoke, 실제 provider, 실제 GPU 실행은 각각 별도 승인·증거로 구분한다.
문서 병합이나 fixture 통과를 multi-cluster 실사용 검증으로 표시하지 않는다.

같은 task corpus·입력·검증 기준·예산에서 single-agent, 로컬 병렬, context 공유,
원격 분산을 단계적으로 비교한다. 모델 버전·캐시·queue 대기를 기록하고 반복 측정한다.
기능 추가로 처리량이 개선됐다는 수치는 측정 전에는 제시하지 않는다.

핵심 지표는 accepted 작업당 시간·비용, 완료율, 통합 재작업률, 회귀/잘못된 pass,
중복 실행, 복구율과 사람 개입량이다. 자원 관점에서는 Git/SSH 호출 수, 반복 context,
실제 디스크 I/O, 전송량, CI·GPU 시간, 검증 대기를 함께 본다. 단일 저장소 전체 스캔,
작은 원격 호출·파일별 전송 반복을 줄이고 event/cursor·한도 있는 batch·불변 snapshot을 재사용한다.
Rust 사용 자체를 디스크 I/O 감소나 생산성 개선의 증거로 삼지 않는다.

## 14. 구현 에이전트에게 전달할 지시

1. 착수 시 최신 Rust 전환·관련 PR 상태를 확인하고, 재사용할 계약과 추가할 동작부터 제시한다.
2. 첫 실행 기능은 로컬의 작은 병렬 slice로 제한한다. 작업 소유권·검증·통합을 먼저 완성한다.
3. Contract-first 협업과 관련 context 공유를 연결하고, 기존 이벤트·상태의 두 번째 해석기를 만들지 않는다.
4. 로컬과 원격이 동일한 작업·결과 의미를 갖게 한다. 전송 실패와 작업 실패는 구분한다.
5. 새 공개 API·스키마·경로 정책은 구현 PR에서 제안하고 호환성·철회 기준을 함께 적는다.
6. 현재 통과 근거가 정확히 어느 입력·후보·환경을 검증했는지 남기고 변경 뒤에는 재평가한다.
7. PR마다 목표, 비목표, 재사용한 기능, 추가 동작, 권한 변화, 검증/미검증, rollback을 보고한다.
8. Main 통합·publication·원격 실행·유료 계산을 각자의 승인 경계 안에서만 수행한다.

후속 승인이 필요한 것은 실제 대상/허용 root, 신뢰·격리 수준, provider/GPU/전송 예산,
공유 API·저장 계약 변경, publication/자동 병합 범위다. 이 문서에서 특정 저장소 파일 배치나
crate 수, 중앙 DB, 상주 daemon, 자동 계정 배포, 전역 자동 router를 요구하지 않는다.

최종 방향: **공통 작업·권한·증거 계약을 먼저 완성하고, 병렬 실행을 안정화한 뒤 같은 의미를
원격 호스트와 클러스터로 확장한다.**

## 15. Stages A–F 구현 및 검증 현황

Stages A–F는 기존 순수 Rust 코어(`1.99.1`, `#![forbid(unsafe_code)]`, 추가 외부 의존성 없음)의 opt-in 확장으로 구현했습니다. Stage G(측정 기반 라우팅·최적화)는 측정 데이터 축적 전까지 보류합니다.

| 단계 | 주요 구현 위치 | 핵심 동작 |
|---|---|---|
| A. 기준선·계약 | `oms-command-plan`, `oms-runtime`, `oms-command-autonomy` | 기존 Task/Lease/Target/Slurm/Landing 의미 유지 및 opt-in 확장 |
| B. 로컬 병렬 실행 | `crates/oms-command-plan/src/{engine,query,proposal}.rs`, `crates/oms-command-autonomy/src/{parallel,planning,services}.rs`, `crates/oms/src/services_control.rs` | DAG cycle/unknown-dep 검증, `--resources`/`--priority`/`--max-writers`/`--max-review-backlog` backpressure, singleton controller lock, non-barrier `std::thread::scope` 병렬 실행(`Services::invoke_concurrent` -> `Scope::ProcessGroup`으로 Linux 다중 스레드 감독) |
| C. 협업·통합 | `crates/oms-command-plan/src/contracts.rs`, `crates/oms-command-autonomy/src/conflicts.rs` | 버전된 계약(`contract-propose`/`contract-accept`/`refresh-contracts`, `landing` 보호 및 claim 무효화 초기화), stale 계약 gating, `.oms/plan/late-results.jsonl` 격리, `oms plan-run --verify-combined` 및 detached worktree `--no-ff` 통합 재검증(`verify_combined`)·target drift 방어 |
| D. 원격 준비 | `crates/oms-runtime/src/{targets,exec_snapshot,ssh_adapter,cli_schema,parser,router}.rs`, `crates/oms/src/main.rs` | `execution_approved: false` 기본값, identity fingerprint 결합 `approve`/`revoke`, 불변 `exec-snapshot` 생성·검증, `--target` 생략 시에도 등록된 미승인 호스트 차단 및 `--snapshot` 로컬·원격 checkout 일치 검증 |
| E. 원격 실행·복구 | `crates/oms-runtime/src/ssh_adapter.rs` | `.oms/runtime/remote-ops/<op_id>.json` durable ledger, `--op-id` 멱등/중복 차단, 16-hex nonce 기반 원격 프레임·bounded `dd`·`STILL_RUNNING` 출력 차단, `reconcile`/`cancel`(대기 루프 `runner.sh`, parent+child `SIGKILL`, `lstart` 미검증 시 `unknown` 보류), symlink 조상 거부·SHA-256 atomic `collect` |
| F. Slurm·산출물·다중 저장소 | `crates/oms-command-slurm-submit/src/{lib,ledger,reconcile}.rs`, `crates/oms-command-landing/src/{cli,delivery,manifest}.rs` | `slurm-submit` provenance 및 attempt·`-M <cluster>` 단위 `reconcile`(`squeue`/`sacct` 복구·비정상 상태 실패 보존·`unknown` exit 75), `land --check-manifest` / `--execute-manifest` 엄격 `depends_on` 검증, state-root 중복 제거 락, checked-out target drift 방어 및 `partial-hold`(exit 3) |

### 13절 시나리오별 결정적 검증 매핑 (Fixture / Fault-Injection)

아래 테스트는 **가짜 `OMS_SSH_BIN`, 가짜 `sbatch`/`squeue`/`sacct`, 임시 Git 저장소 기반의 결정적 회귀·장애 주입 검증**입니다. 실제 다중 클러스터·실제 GPU·외부 LLM 호출 실증과는 구분됩니다.

| # | 13절 검증 시나리오 | 검증 테스트 |
|---|---|---|
| 1 | 독립 작업·공유 API 계약 | `oms_command_autonomy::tests::parallel_run::{parallel_refills_slots_without_barrier_and_isolates_a_panicking_sibling, overlapping_scopes_never_run_together, overlapping_resources_never_run_together}`, `oms::services_control::tests::{control_services_invoke_concurrent_works_across_threads_on_linux, invoke_concurrent_timeout_reaps_process_group}`, `oms::native_entry_contract::sigterm_on_native_peer_command_reaps_setsid_descendants`, `oms_command_plan::cli::{merged_dependencies_scheduling_and_late_results, versioned_contracts_gate_scheduling}` |
| 2 | 검증 후 코드 또는 target 변경 | `oms_command_autonomy::tests::combined_verification::{textually_clean_merge_with_incompatible_combination_fails_verification, target_advancing_during_verification_cannot_verify, verified_combination_reports_identity_and_leaves_no_trace, cli_verify_combined_returns_structured_outcomes}`, `oms_runtime::tests::runtime_contract::snapshot_detects_checkout_and_lockfile_drift`, `oms_command_landing::landing_contract::manifest_publish_refuses_checked_out_target_drift` |
| 3 | Worker 종료·controller 재시작 | `oms_command_autonomy::tests::parallel_run::{one_controller_per_state_repository_and_flag_validation, parallel_refills_slots_without_barrier_and_isolates_a_panicking_sibling}` |
| 4 | 원격 수락 직후 응답 손실 | `oms_runtime::ssh_adapter::tests::{exec_op_id_disconnect_recovers_via_reconcile_without_duplicate, omitted_target_blocks_unapproved_host_and_snapshot_binds_remote_head}` |
| 5 | Slurm 제출 후 기록 전 장애 | `oms_command_slurm_submit::reconcile_contract::{lost_sbatch_reply_is_recovered_from_the_scheduler_and_blocks_a_resubmit, forced_resubmit_never_adopts_earlier_attempt_and_job_id_keeps_its_attempt, cluster_flag_is_recorded_and_forwarded_to_scheduler_queries, late_forced_attempt_cluster_and_tag_scoping_prevent_misattribution}` |
| 6 | Lease 만료 뒤 이전 결과·계약 변경 | `oms_command_plan::cli::{merged_dependencies_scheduling_and_late_results, versioned_contracts_gate_scheduling}` |
| 7 | 시계 차이·PID/Job ID 재사용 | `oms_runtime::ssh_adapter::tests::{pid_reuse_boot_id_and_conflicting_op_id_are_fenced, cancel_escalates_sigkill_and_blocks_spoofed_frames_and_unverifiable_lstart}`, `oms_command_slurm_submit::reconcile_contract::terminal_states_never_map_to_success_and_lag_or_ambiguity_is_unknown` |
| 8 | 전송 중단·digest 불일치·경로 탈출 | `oms_runtime::ssh_adapter::tests::{collect_enforces_paths_bounds_and_digest_atomically, collect_rejects_symlink_ancestors_and_overflow_with_open_stderr}`, `oms_runtime::tests::runtime_contract::target_approval_is_bound_to_identity` |
| 9 | 리뷰 backlog·한도 부족 backpressure | `oms_command_autonomy::tests::parallel_run::review_backlog_stops_claims_drains_in_flight_and_exits_three`, `oms_command_plan::cli::merged_dependencies_scheduling_and_late_results` |
| 10 | 다중 저장소 부분 통합 실패 | `oms_command_landing::landing_contract::{manifest_failure_holds_dependents_keeps_integrated_repos_and_resumes_idempotently, manifest_rejects_malformed_graphs_and_missing_receipts_with_exit_two, manifest_check_publishes_nothing_and_execution_needs_the_publication_fence, manifest_publish_refuses_checked_out_target_drift}` |

실제 원격 호스트·실제 Slurm 클러스터·실제 GPU 환경에서의 end-to-end 스모크 테스트는 각 호스트의 명시적 `target approve` 이후 별도 증거로 기록합니다.
