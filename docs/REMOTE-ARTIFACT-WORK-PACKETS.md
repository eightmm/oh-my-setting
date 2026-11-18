# 원격 산출물: 수집 중단·묶음 재개·디스크 사용량 후속 작업

**상태: DESIGN ONLY — 구현 에이전트에 넘길 작업 명세다.**
작성일은 2026-11-18이며 조사 기준은 main
[`f9a695ea2de90a160754252c1ee9cbaa4dd7a488`](https://github.com/eightmm/oh-my-setting/commit/f9a695ea2de90a160754252c1ee9cbaa4dd7a488)이다.
이 문서는 runtime, 전송 한도, 설치 의존성, SSH 설정이나 자동 삭제 정책을 바꾸지 않는다.
원격 장비·Slurm·GPU·유료 provider는 실행하지 않았다. 아래 완료 조건은 미래 구현의 검증 조건이다.

채택 상태는 [통합 작업 큐](ORCHESTRATOR-WORK-QUEUE.md)를 따른다. AR-01은 원격 collect를 사용하는
경로의 교정이다. AR-02/03은 실제 여러 산출물의 전달·완료 consumer가 필요한 경우 함께 채택하는
한 선택 기능이며, AR-04는 cache 공간 문제가 확인된 경우의 inventory부터 검토한다.
일반 coding/provider orchestration의 필수 선행 작업으로 이 네 ID를 모두 구현하지 않는다.

## 1. 기존 구현과 이번에 추가할 범위

[#243](https://github.com/eightmm/oh-my-setting/pull/243)의 단일 파일 재개 수집은 이미 병합됐다.
기존 [연구 후속 문서](RESEARCH-FOLLOWUPS.md)의 여러 artifact 수집 제안을 이 구현 위에서 구체화한다.
Overall Main과 provider 사이의 task·worktree·결과 연결은
[#259](https://github.com/eightmm/oh-my-setting/pull/259)의 범위이며 여기서 다시 설계하지 않는다.
원격 수집은 필요한 프로젝트만 선택해서 사용한다. 일반 coding이나 native pane의 필수 경로가 아니다.

| 확인한 현재 동작 | 소스 기준 | 추가 작업의 이유 |
|---|---|---|
| `collect`는 하나의 `remote_path`와 `dest`를 받는다. 기본 상한은 64 MiB, 최대는 512 MiB다. | [ssh_adapter.rs](../crates/oms-runtime/src/ssh_adapter.rs):15–19, 2130–2144 | 여러 파일의 필수/선택 여부와 묶음 완료를 연결할 계약이 없다. 대형 checkpoint 전체 수집을 지원한다고 표현하지 않는다. |
| ordinary `run_collect`는 stdout의 blocking read, stderr thread join, child wait를 수행한다. 함수 내부에는 실행 중 interrupt 재검사나 전체 deadline이 없다. | 같은 파일:2298–2403 | byte 상한은 유효하지만 출력 없이 열린 pipe에서 시간 상한·취소 응답을 보장하지 않는다. 실제 장애 재현은 후속 fixture 대상이다. |
| resume 경로는 nonblocking drain, chunk당 60초 timeout, 중간 interrupt 검사, exact size/hash, private cache와 destination lock을 갖는다. | [resumable.rs](../crates/oms-runtime/src/ssh_adapter/resumable.rs):6–19, 226–299, 301–486 | 이 기능을 재사용해 ordinary collect와 종료 의미를 맞추고, chunk별 시간 제한 외에 요청 전체 상한을 정의한다. |
| resume identity는 host/port/remote 경로/destination/hash/size/chunk 크기를 묶는다. | 같은 파일:309–317 | 전송 identity는 존재한다. 실행 snapshot·run·선택한 결과 묶음의 identity와는 별개다. |
| 캐시 chunk와 manifest를 검증한 뒤 새 임시 파일로 조합하고 최종 hash를 확인해 게시한다. 성공 응답에도 cache 경로를 남긴다. | 같은 파일:364–486 | 완료 후에도 cache가 남는다. 파일별 byte 상한을 여러 전송의 총 디스크 상한으로 설명할 수 없다. |
| 기존 remote operation ledger, 실행 snapshot, artifact evidence, cleanup 보호 로직이 있다. | [ssh_adapter.rs](../crates/oms-runtime/src/ssh_adapter.rs), [exec_snapshot.rs](../crates/oms-runtime/src/exec_snapshot.rs), [evidence.rs](../crates/oms-runtime/src/evidence.rs), [services_control.rs](../crates/oms/src/services_control.rs) | 새로운 원격 scheduler·상주 daemon·artifact DB 대신 기존 identity와 evidence를 확장한다. |

위 평가는 해당 경로의 정적 추적이다. 전체 SSH adapter의 모든 동작을 감사하거나
실제 네트워크의 속도·hang 빈도·디스크 절감을 측정한 결과가 아니다.
기존 단일 파일의 digest·경로·경합·부분 재개 시험을 유지한다.

## 2. AR-01 — ordinary collect의 취소·deadline·cleanup 일치

**원격 collect 경로 사용 시 P1. 의존성 없음.** 기능 확대 전에 기존 수집 경로의 응답성을 정리한다.

### 변경 지점과 구현 계약

- `ssh_adapter::run_collect`, `resumable::fetch`, 두 경로의 child 종료 처리를 추적한다.
  [oms-supervisor process](../crates/oms-supervisor/src/process.rs)의 bounded streaming과 cleanup 증거를
  우선 재사용한다. 대용량 파일을 작은 capture buffer에 모두 담는 변환은 하지 않는다.
  필요한 streaming sink는 기존 reactor에 최소 확장하고 별도 detached process manager를 만들지 않는다.
- ordinary/resume 모두 monotonic 요청 deadline과 interrupt를 전송·stderr drain·종료 대기 중 처리한다.
  chunk 시작마다 전체 deadline을 다시 주지 않는다. 연결·전송·cleanup의 시간 책임을 구분하고,
  timeout 후 cleanup에 허용하는 유한 grace도 명세에 넣는다.
- 출력량 제한, stderr의 제한, 정상 EOF, child exit, cleanup 완료를 각각 기록한다.
  stderr를 읽다 상한에 도달해도 child가 pipe에 막힌 채 남지 않도록 계속 drain하거나 안전하게 종료한다.
- 최종 rename 전의 취소/timeout/실패 때 기존 destination은 보존한다. Resume은 검증된 chunk를 유지하며,
  ordinary 모드의 임시 파일은 기존 소유권 규칙으로 정리한다. 불명 cleanup을 성공으로 바꾸지 않는다.
- local SSH client 정리가 원격 job 취소를 의미하지 않는다. Collection은 read operation이며
  기존 `cancel`의 remote operation 권한을 얻지 않는다. 다른 transfer가 쓰는 ControlMaster도 종료하지 않는다.
- 성공 게시에는 임시 파일과 parent identity, lock custody, fsync/rename 결과를 확인한다.
  Resume의 directory durability 의미를 ordinary 경로에서도 일관되게 보고한다.
  Rename 후 directory fsync 실패나 응답 손실은 이미 destination이 바뀌었을 수 있는
  `publication_unknown`이다. 이전 destination이 보존됐다고 보고하거나 무조건 rollback하지 않고
  실제 destination의 identity/hash와 게시 증거를 reconcile한다.

### 완료 조건

기존 `collect_enforces_paths_bounds_and_digest_atomically` 및
`collect_rejects_symlink_ancestors_and_overflow_with_open_stderr`를 확장한다.
Fake SSH가 stdout을 안 닫음, stdout만 닫고 stderr 유지, stderr만 지속 출력,
leader 종료 뒤 descendant가 pipe 유지, 취소와 최종 byte 도착 경합을 재현한다.
Timeout/cancel 뒤 한정된 시간 안에 반환하고 게시 전 destination 보존·child cleanup 증거를 확인한다.
Rename 직후 directory fsync 실패와 응답 손실에서는 게시 불명 상태와 재확인 결과를 검증한다.
가짜 clock/동기화 fixture를 우선 사용하며 60초 sleep이나 실서버가 자동 검증의 조건이 되지 않는다.
정상·empty·상한 초과·digest 불일치의 기존 응답 계약도 유지한다.

## 3. AR-02 — 실행에 묶인 bounded artifact manifest

**우선순위 P2. 선택형 원격 결과 묶음이 실제 필요한 과제에서 구현한다.**
예시는 하나의 실행이 만든 `metrics.json`, `summary.json`, 제한된 로그, 선택형 checkpoint다.
먼저 프로젝트가 만든 immutable bundle 하나를 기존 단일 파일 수집으로 전달해 충분한지 확인한다.
별도 manifest가 필요하면 AR-03의 전달·완료 consumer와 한 기능으로 채택한다. Consumer 없이
manifest 규약만 추가하지 않으며, 구현 PR은 계약과 전송 단계로 나눌 수 있다.

### 변경 지점과 구현 계약

- 기존 execution snapshot 및 remote operation/run identity를 사용해 immutable manifest를 만든다.
  필드는 설계안이며 현행 CLI 옵션이 아니다: manifest version/digest, producer identity,
  실행 snapshot digest, run 또는 remote operation reference, 파일별 상대 경로·크기·SHA-256·필수 여부.
- artifact를 만드는 프로젝트가 완료된 결과 파일과 hash를 제공하거나, 기존 실행 권한 안의
  producer가 고정된 결과를 해시한다. 수집기가 임의의 실행을 재시작하거나 권한 밖 원격 명령을 만들지 않는다.
  hash의 출처를 기록하고, 현재 내용의 hash만 맞는 것을 해당 실행의 산출물임이 증명됐다고 표현하지 않는다.
- 전체 manifest byte, 파일 수, 파일별 크기, 전체 byte, 경로 깊이를 제한한다.
  기존 512 MiB 파일 상한을 묶음 도입만으로 올리지 않는다. 큰 checkpoint는 위치·metadata만
  수집하거나 별도로 승인한 저장소 경로를 사용한다.
- 경로는 명시적 목록으로 고정한다. 절대 경로, parent traversal, 중복·정규화 충돌,
  symlink·hardlink·device, destination 상호 포함, case-insensitive 호스트의 이름 충돌을 거부한다.
  숨은 glob 확장이나 원격 디렉터리 전체 순회를 추가하지 않는다.
- manifest 변경은 새로운 결과 묶음이다. 이전 부분 cache를 다른 run/manifest로 재분류하지 않는다.
  실행 중 계속 변하는 로그는 finalized artifact와 구분하며, 고정된 byte 범위·digest가 없는 live 파일은 제외한다.
- 전송 성공은 byte 무결성 증거다. 연구 metric의 의미 검증·task acceptance·Plan 완료는 기존 verifier가 담당한다.

### 완료 조건

기존 runtime contract 및 SSH fake fixture를 확장해 다른 run/snapshot/manifest 바인딩 거절,
잘린 manifest, 중복 경로, Unicode 경로, 경로 충돌, empty 파일, 필수 파일 누락,
파일/총량 상한, 전송 중 source 변경을 검증한다.
원격 경로·계정·raw 로그는 공개 문서나 모델 prompt에 복사하지 않는다.
Legacy 단일 파일 collect의 호출과 출력은 호환되게 유지한다.

## 4. AR-03 — 묶음 단위 partial/resume/완료 receipt

**조건부 P2. 실제 결과 묶음이 필요한 경우 AR-01 + AR-02 이후.**

### 변경 지점과 구현 계약

- `CollectRequest`, destination/transfer lock, chunk 검증, safe-fs 게시 기능을 재사용한다.
  항목별 진행은 묶음 manifest identity 아래에 연결하고 `pending/partial/verified/failed`를 구분한다.
  cache checkpoint는 delivery receipt가 아니며, 전체 필수 파일을 검증하기 전 완료 receipt를 쓰지 않는다.
- 기존 [multi-host 정책](MULTI-AGENT-MULTI-HOST-PLAN.md)의 묶음 전송 원칙을 유지한다.
  파일마다 SCP를 반복하는 구현은 하지 않는다. 제한된 manifest 전체에 하나의 전송 세션을
  사용하거나, 검토한 immutable bundle 하나를 기존 resumable 경로로 받는 설계를 먼저 비교한다.
  압축률보다 재개 byte·SSH process/round-trip·해시 비용·최대 메모리를 측정해 방식을 고른다.
- bundle extraction을 고른다면 private staging에서 선언된 regular file만 읽고,
  archive metadata의 경로·링크·owner·mode를 그대로 신뢰하지 않는다. 예상 외 항목·중복·압축 해제 총량 초과를 거부한다.
- 이미 검증한 파일도 재개 시 현재 로컬 byte/identity를 확인한다. 잘못된 partial은 완료로 승격하지 않는다.
  모든 필수가 일치한 후 durable completion receipt를 게시하고, 소비자는 receipt가 가리키는
  immutable staging 결과만 읽는다. 여러 destination rename을 filesystem 전체 원자성으로 표현하지 않는다.
- 동일 요청 재실행은 이미 확인한 결과를 읽는다. 중간 종료·성공 직후 응답 손실에서 duplicate 실행,
  remote job 재제출, 기존 검증 산출물 덮어쓰기를 유발하지 않는다.
- 선택 파일 실패는 목록과 사유로 남기고 필수 파일 실패와 구분한다. Receipt에는 transport 결과와
  verification 결과를 따로 넣고, 기존 artifact index/evidence 경로에 reference만 연결한다.

### 완료 조건

첫 파일 후 중단, 마지막 파일 hash 실패, 필수 누락/선택 누락, 완료 receipt 직전·직후 crash,
같은 destination의 동시 수집, manifest 교체, local 결과 변조를 결정적 fixture로 검증한다.
최종 게시 전 실패는 기존 destination과 검증된 재개 자료를 보존한다. 게시 이후 오류·응답 손실은
AR-01의 publication-unknown/reconcile 계약으로 처리하며, 성공으로 보이는 부분 묶음이 없어야 한다.
원격 모델·GPU 없이 fake SSH와 임시 Git/run fixture로 입증한다.

## 5. AR-04 — 재개 cache의 공간 예산과 검토 가능한 정리

**조건부 P2. Cache 공간 문제가 확인되면 inventory·dry-run부터 기존 단일 파일에 독립 적용 가능하다.**
삭제 apply에는 아래 durable completion producer가 선행하고 bundle 통합은 AR-03 이후다.

### 현재 비용과 변경 계약

현재 resume은 cache chunk를 저장한 뒤 조합용 임시 파일을 추가로 쓴다.
같은 destination에 기존 파일이 있으면 게시 전에는 기존 파일·cache·조합 파일이 함께 존재한다.
이는 코드에서 확인한 데이터 복제 구조이며 실제 disk 사용량이나 절감률 측정값은 아니다.

- 먼저 현재 transfer들의 cache byte, 조합용 예상 byte, 기존 destination 보존 byte,
  transfer 수·상태를 읽는 bounded inventory를 만든다. 기존 retention/GC 패턴을 재사용하고
  실제 임의 destination 바깥을 자동 스캔하는 새로운 전역 collector는 만들지 않는다.
- 호출자 예산과 현재 공간을 전송 전에 확인하고, 실행 중 ENOSPC도 정상 실패로 다룬다.
  preflight 여유 공간만으로 다른 process의 공간 사용을 막았다고 주장하지 않는다.
- 기본은 보존과 정리 계획 표시다. 삭제 적용은 별도의 명시적 요청과 exact plan digest,
  lock custody, transfer identity 재확인 뒤에만 한다. 이 명세 PR은 삭제를 실행하지 않는다.
- 현행 단일 파일의 성공 JSON 응답과 cache manifest는 durable delivery receipt가 아니다.
  삭제 apply 전에 AR-01의 게시 상태와 actual destination 검증에 묶인 completion evidence를
  기존 artifact/evidence 경로에 영속화하는 producer를 연결한다. 기존 cache에 receipt가 없으면
  보호 상태로 두고, 단순히 manifest의 chunk가 모두 있다는 이유로 완료를 추정하지 않는다.
- active/partial/unknown delivery, receipt가 없는 결과, 다른 owner, symlink/identity 변경은 보호한다.
  확인된 completed cache만 정책의 후보이며, destination을 재검증하지 못하면 보류한다.
  cache 삭제는 원격 원본·사용자 산출물·실행 중 checkpoint 삭제로 확대하지 않는다.
- cache hit를 이유로 full digest 검증을 없애지 않는다. 재개 byte와 local disk 재읽기 byte를
  별도로 측정해 실제 병목을 확인한 후에만 검증된 chunk의 읽기 중복을 줄인다.

### 완료 조건

같은 파일의 다른 transfer identity, 명시된 cache root 밖 경로, active lock, stale plan,
동시 재개, lock 상실, symlink/owner 교체, ENOSPC, 부분 삭제 실패를 검증한다.
Dry-run은 mutation이 없어야 하며 삭제 후에도 artifact evidence와 완료 결과는 보존돼야 한다.
디스크 공간이 필요한 실제 상황을 확인하지 못하면 자동 GC 활성화를 완료 기준으로 삼지 않는다.

## 6. 외부 근거와 선택 기준

공식 [OpenSSH sftp manual](https://man.openbsd.org/sftp)의 `-a` 설명은
재개하는 partial 내용이 원본과 다르면 결과가 손상될 수 있음을 명시한다.
따라서 일반적인 resume 옵션을 OMS의 exact hash/size 검증보다 강한 보장으로 바꾸지 않는다.
같은 문서의 buffer 조절은 round-trip과 메모리의 tradeoff다. 큰 chunk가 언제나 빠르다고 전제하지 않는다.

공식 [ssh_config manual](https://man.openbsd.org/ssh_config)의 `ConnectTimeout`과
`ServerAliveInterval`/`ServerAliveCountMax`는 연결과 연결 생존 확인의 설정이다.
서버가 응답하지만 artifact producer가 멈춘 경우의 전체 작업 deadline을 대신하지 않는다.
이 해석에 따라 AR-01은 transport 생존 확인과 요청 deadline을 구분한다.

## 7. 구현 순서·검증 범위

| 작업 | 선행 | 첫 구현의 합격 증거 |
|---|---|---|
| AR-01 | 원격 collect 경로 사용 | ordinary/resume 취소·deadline·cleanup, 기존 destination 보존 |
| AR-02 | 실제 결과 묶음 필요; 기존 단일 bundle로 충분한지 확인 | AR-03 consumer에 연결된 bounded manifest와 run/snapshot 바인딩, legacy 호환 |
| AR-03 | 실제 결과 묶음 필요; AR-01, AR-02 | partial/resume, exact bundle completion receipt, 무중복 재개 |
| AR-04 | cache 공간 문제 확인; apply는 completion producer 이후 | 공간 inventory와 무변경 정리 계획; 승인된 apply의 경합 보호 |

구현 에이전트는 최신 HEAD를 재확인하고 기존 runtime/SSH canonical test를 먼저 확장한다.
선택한 코드 경로에 맞는 Rust·Linux/macOS 검증을 실행하며, 새 SSH 서버·GPU·유료 CI matrix는 추가하지 않는다.
이번 문서의 검증은 source 대조와 문서 참조 검사이며, 위 기능을 구현·실서버 검증했다고 보고하지 않는다.
