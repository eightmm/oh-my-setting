# 목표에서 결과 확인으로 이어지는 OMS 패널

작성일: **2026-11-26 (KST)**. 이 문서는 이 변경의 코드 경로와 검증 범위를 설명한다.
기존 **목표 → 현재 Main → Leads** 화면을 유지하며, 완료 기준 확인과 결과 읽기,
범위가 붙은 후속 요청 초안과 선택한 결과 공유를 연결한다. 더 살펴보기에서는 기존 읽기 API의
사용량·프로젝트 관계·예상 라우팅·모델 카탈로그를 필요할 때 읽는다.
새로운 scheduler, task 배정 경로 또는 완료 원장을 만들지 않는다.
다른 화면 구성의 합의 이력은 [공통 작업 화면](CONTROL-SURFACE-UX.md)에 남긴다.
그 문서의 모든 로드맵 기능을 이번 변경의 구현 완료 항목으로 해석하지 않는다.

## 사용 흐름과 구현 범위

| 제안 | 현재 구현 | 의미와 경계 |
|---|---|---|
| 목표와 완료 기준 | 목표 아래 repository 완료 기준의 확인 수, 현재 확인이 필요한 task, canonical 다음 단계 요약을 표시한다. 기준·근거·Plan 선택 조건은 접어서 읽는다. | Plan task 완료 수와 완료 기준 수를 합산하지 않는다. unavailable은 0이 아닌 미확인이다. repository 결과를 현재 Main Room만의 성과로 표시하지 않는다. 다음 단계는 설명이며 실행 권한이 아니다. |
| Main·Lead·배정 기록 구분 | 새 작업 시작, Main 대화에서 이어가기, 특정 Lead에게 질문, 배정 기록 보기를 구분한다. | Lead 질문은 읽기 전용 상담이다. readiness 대화가 존재한다는 이유로 canonical Lead task나 worker가 배정됐다고 표시하지 않는다. |
| 일시적인 읽기 실패 | registry가 같은 물리 프로젝트 key를 독립적으로 확인했고 같은 generation이면 마지막 public snapshot을 보존하고 오래된 화면임을 표시한다. | 모든 쓰기를 차단한다. private 답변·설정·요청 초안·승인 버튼은 지운다. key 미확인, alias 대상 변경, 프로젝트·Room 전환에는 private 데이터와 이전 범위의 제어 상태를 보존하지 않는다. |
| 대기와 복구 | 제한된 자동 확인이 끝나면 중지 상태와 수동 확인을 표시한다. Lead별 마지막 turn이 대기 중인지, 새 답변인지, 현재 패널에서 읽었는지 표시한다. | 확인 버튼은 같은 turn의 상태만 읽는다. 불확실한 전송을 새 turn으로 자동 재전송하지 않는다. 새 답변 표시는 확인된 metadata에만 근거하며 읽음 상태는 현재 패널 메모리 범위다. |
| 입력과 읽기 위치 | Lead 입력은 UTF-8 4,000바이트를 세고, 새 목표는 Lead 선호 prefix를 포함한 최종 2,000자 한도를 센다. Room 새로 고침은 읽던 위치를 유지하고 새 메시지 안내를 제공한다. | 이전 메시지는 명시적으로 더 읽는다. 새 메시지가 도착했다는 이유로 중간을 읽던 사용자를 맨 아래로 이동시키지 않는다. |
| Lead 연결 선택 | 특정 Lead 하나 또는 세 Lead 전체를 명시적으로 연결한다. 미래 방문의 자동 연결 선호는 별도 opt-in으로 저장한다. | 이번 연결 클릭은 미래 자동 실행 선호를 저장하지 않는다. 선호 체크를 켜는 동작도 현재 방문에서 즉시 연결을 실행하지 않는다. |
| 설정과 관측 값 | 다음 메시지에 적용할 요청 설정, catalog 관측 나이, 요청 모델·effort와 해당 답변에서 확인된 모델 증거를 구분한다. 설정 범위와 OMS thread 연속성을 표시한다. | Lead 상담 설정은 worker로 상속되지 않는다. 계정·failover·사용량을 이 답변에 결합하는 reader는 이번에 추가하지 않았으며 해당 값은 null이고 미확인으로 안내한다. 저장소에 관련 telemetry가 없거나 수집되지 않는다는 뜻이 아니다. |
| 긴 결과 읽기 | Lead 답변, Room 결과, worker 출력은 선택한 뒤에만 byte-bounded 페이지로 읽는다. 잘림, 현재 바이트 범위, 다음 부분, 처음부터 다시 읽기를 표시한다. | 복사는 현재 표시된 부분만 복사한다. 한 번에 전체 원문을 모으거나 화면 밖에서 자동으로 끝까지 읽지 않는다. 파일 경로를 사용자가 입력하는 reader를 추가하지 않는다. |
| 결과에 대한 후속 요청 | 현재 프로젝트·Room에 고정한 상태에서 목표·요약과 확인된 `task`(및 Lead 답변의 `turnId`)를 담은 수정 가능한 초안을 준비한다. 복사 또는 지원하는 호스트의 대화 문맥에 추가하고 Main 대화로 이동할 수 있다. | 문맥 추가는 Main 수신 receipt, 사용자 메시지 전송, 작업 배정, 실행 시작, 완료 승인이 아니다. 이 상태를 성공한 실행으로 표시하지 않는다. |
| 선택한 결과 공유 | 결과 창의 `이 결과로 요청`은 참조만 담은 초안을 준비하고, `발췌문 포함`을 누를 때만 현재 표시된 결과의 앞부분을 UTF-8 4,000바이트 이내로 초안에 붙인다. 사용자가 공유를 누르면 그 발췌문과 수정한 요청 초안만 현재 호스트 대화 문맥에 추가한다. | 공유 전에 같은 물리 프로젝트·Room·Main·결과·task를 다시 확인하고 exact page를 읽는다. 원문 digest·byte 범위·선택 본문이 달라지면 공유하지 않는다. 참조와 초안만 추가하는 동작은 private 본문을 포함하지 않는다. |
| 더 살펴보기 | 사용량과 쿼터, 프로젝트 관계, 예상 라우팅, 모델 카탈로그 중 선택한 화면만 읽는다. 라우팅·카탈로그는 페이지로, 그래프는 노드 선택이나 이름·경로 검색으로 더 읽는다. | 저장소의 최근 사용량을 현재 Room이나 답변의 사용량으로 표시하지 않는다. 0과 미확인, 추출·추정·모호·구조 관계, 모델별 effort와 공급자 단위 effort를 구분한다. 읽기 전용 호스트에서도 사용할 수 있으며 명령 실행이나 index 생성을 요청하지 않는다. |

완료 기준은 공개 projection에서 최대 8개, 기준별 근거는 최대 3개를 표시한다.
Plan 선택 조건은 최대 8개다. 생략이 있으면 일부만 표시한 사실을 안내한다.
완료 기준의 참조는 설명용 opaque 식별자다. 이를 파일 경로나 다운로드·실행 대상에 넣지 않는다.
현재 막힌 이유나 review 대기는 Plan producer의 결과를 사용하며, 화면이 별도 scheduler를 계산하지 않는다.

## 기존 API와 연결

패널 리소스와 두 플러그인 manifest는 `ui://oms/conversation-panel/v33.html`을 사용하며,
이전 버전의 리소스 읽기 별칭을 유지한다.

| 경로 | 추가 입력·출력 | UI 사용 |
|---|---|---|
| `oms_list_projects` | alias별 `projectKeys`, pinned/cwd의 `projectKey` | 마지막 정상 화면과 private 선택의 물리 프로젝트 범위를 확인한다. |
| `oms_panel_snapshot` | `acceptance`, `planReadiness`, `attention`, `nextStep`의 repository projection | 기준 확인 수, 근거 상세, 현재 확인할 일과 다음 단계 설명을 표시한다. |
| `oms_open_lead_conversations` | 선택적인 `providers` 배열; 생략하면 세 provider | 개별 연결과 전체 연결에 같은 endpoint를 쓴다. 반환된 subset만 검증하여 병합한다. |
| `oms_lead_conversation_status` | 공개 `latestTurn`; 명시적인 `answerPage` private read | 마지막 turn 상태와 답변 페이지를 분리한다. full read는 projectKey·Room·provider·leadRequestId·turnId를 고정한다. |
| `oms_lead_settings` / `oms_update_lead_settings` | `scope`, `catalogAgeSeconds`, selection과 lastServed | 다음 메시지 설정과 실제 답변 증거를 구분하고 기존 revision-checked 저장을 유지한다. |
| `oms_panel_chat` | transcript/diff·attempt에 `evidenceCursor`; 이 모드는 exact `projectKey` 필수 | 선택한 Room 결과를 읽는다. public/private projectKey와 Room·participant·attempt·section이 모두 일치해야 표시한다. 기존 메시지·legacy page 읽기는 유지한다. |
| `oms_worker_output` | `answerPage`, private `page` metadata | 확인된 projectKey·Room·Lead attempt·worker attempt 쌍의 원문만 읽는다. |
| `ui/update-model-context` | `oms-followup-draft`의 선택적 `project` 별칭·`goal`·`summary`·선택적 `task`·선택적 `turnId`·`text`; 선택 공유에는 `oms-selected-finding`의 `authority: context-only`·선택적 `project` 별칭·선택적 `task`·선택적 `turnId`·표시용 `source`(`kind`·`summary`)·`selection`·`text` (두 문맥 모두 `projectKey`·`Room`·`owner`와 내부 `source` 식별자·`digest`·`pageCursor`는 생략) | 참조만 추가하는 클릭과 발췌문을 공유하는 클릭을 분리한다. Main 수신이나 실행의 증거로 사용하지 않는다. |
| `oms_panel_usage` | 기존 `repository-recent-attempts` projection | 더 살펴보기의 사용량 화면에서만 공급자 관측·쿼터 창·최근 시도의 보고된 값과 출처를 읽는다. |
| `oms_panel_project_graph` | 기존 `node` 또는 `query`, `limit: 20`, `depth: 2` | 노드와 confidence가 붙은 관계를 표시하고, 선택한 노드 주변이나 검색 결과만 다시 읽는다. |
| `oms_panel_tui_view` | 기존 `view: routes` 또는 `modelcatalog`, `page` | 최대 20행의 카드를 표시하고 이전·다음 페이지를 명시적으로 읽는다. 예상 라우팅과 캐시 관측의 한계를 함께 표시한다. |

답변은 private `_meta` 경로로 전달하며 초기 public snapshot에 본문을 넣지 않는다.
페이지는 scope와 원문 digest에 결합한다. UI는 바이트 길이·offset 연속성·전체 크기·digest·cursor를
검증하고, 다른 원문 또는 범위의 응답이면 표시된 private 본문과 복사·후속 요청 제어를 지운다.
프로젝트 전환 뒤 늦게 도착한 응답도 이전 본문을 되살리지 않는다.

패널 메모리의 후속 초안은 서버·reader가 확인한 `projectKey`·`Room`·`owner`와 Lead
(`participant`·`operation`·`turnId`) 또는 worker·Room attempt 쌍에 고정해 활성 범위와 `task` 귀속만
검증한다. 선택한 결과의 `task`는 같은 물리 프로젝트·Room의 정확한 worker·attempt 또는 Lead
operation과 연결된 값만 넣으며, 확인되지 않으면 생략한다. 결과를 선택하지 않은 일반 초안은 현재
attention `task`를 사용할 수 있다.
호스트에 전달하는 `oms-followup-draft` 문맥(선택적 `project` 별칭·`goal`·`summary`·선택적 `task`·선택적
`turnId`·`text`), `oms-selected-finding` 문맥(`authority: context-only`·선택적 `project` 별칭·선택적 `task`·선택적
`turnId`·`source`의 `kind`와 표시용 `summary`·`selection`·`text`), 복사 가능한 초안 본문(선택적 `Turn: …`·선택적
`Task: …`·목표 요약·항목 요약)에는 `projectKey`·`Room`·`owner`와 내부 `source` 식별자
(`provider`·`participant`·`leadRequestId`·`operation`·`leadAttempt`·`workerAttempt`·`attempt`·`format`),
전체 원문 `digest`, `pageCursor`를 의도적으로 넣지 않는다.
초안은 전환·실패 시 비우는 패널 메모리이며 새로운 영구 receipt가 아니다.

선택 공유의 `selection`에는 포함한 발췌문 `text`, 원문의 0부터 시작하는 UTF-8 byte `offset`,
`bytes`, `totalBytes`, `truncated`, 원래 표시된 부분의 `displayedBytes`, 선택 발췌문의 `excerptSha256`가
포함된다. UI 범위 표시는 사람이 읽는 1부터 시작하는
byte 번호다. 문자열 중간을 자르지 않으므로 한도에 맞춘 실제 바이트 수는 4,000보다 작을 수 있다.
패널 메모리의 private `draft.source`는 공유 직전 재검증을 위해 exact worker 쌍, Room attempt 또는 Lead의
`Room`·`participant`·`operation`·`leadRequestId`·`turnId`, 전체 원문 `digest`, Lead 답변의 검증한
`pageCursor`를 보존하되 모델 문맥에는 내보내지 않는다.
사용자가 아직 보지 않은 다음 페이지나 private transcript의 다른 메시지는 공유하지 않는다.

공유는 읽기 전용·오래된 화면·문맥 기능이 없는 호스트에서 사용할 수 없다.
공유 직전에 registry와 snapshot을 확인하고, 같은 원문 페이지를 다시 읽은 뒤 현재 범위를
한 번 더 확인한다. 대기 중 프로젝트·Room·원문·task·요청 초안이 바뀌거나 패널이 닫히면
문맥 업데이트를 취소한다. 호스트가 text 형식만 지원하면 동일한 선택 payload를 text로 보낸다.
원문과 선택 발췌문의 해시는 서로 다른 검증 값이며, 해시나 cursor 자체가 실행 권한을 주지 않는다.
공유 버튼은 별도의 Plan 준비·실행 도구를 호출하지 않는다.

v33 화면 정리는 같은 데이터를 덜 겹치게 보여 준다. 상태 색은 결정 필요(`blocked`·입력·review·승인 대기)와
실패(`failed`·`timed_out`·`guard_stop`·`rejected`·`interrupted`·`orphaned`)를 나누고, 아바타 점은
실패 › 결정 › 작업 중 › 연결 › 대기 순으로 하나만 고른다. 진행 막대는 완료 기준이 있으면 기준을,
없을 때만 Plan task를 그리며, 기준 목록 전체가 공개된 경우에만 기준별 칸을 나눈다.
Lead 연결 설정은 접힌 메뉴로 옮기고, 결과 미리보기 대신 결과 창 안의 요청 버튼을 쓴다.

더 살펴보기의 세 읽기 도구는 실제 `chatgpt-panel` profile에서 app에 공개되어 있고
`readOnlyHint`를 갖는다. 페이지를 열거나 항목을 바꿀 때만 호출하며, 읽기 전후 registry의
물리 프로젝트 key를 확인한다. 화면·프로젝트·Room 전환 뒤 늦은 응답은 표시하지 않는다.
허용한 필드만 이름을 붙인 카드로 보여 주며 raw JSON이나 진단 응답을 그대로 펼치지 않는다.

## 검증과 남은 확인

프런트엔드의 기존 `node:test` + VM/DOM fixture를 확장했다.
다음 명령으로 84개 테스트를 실행한다(목표·결과 흐름의 70개를 확장하고 선택 공유·더 살펴보기·첫 화면 12개, v33 화면 정리 2개 시나리오 추가).

```sh
node --test crates/oms-mcp-server/tests/conversation-panel.test.mjs
```

추가 회귀 범위는 같은 물리 프로젝트에서만 public stale 보존, stale 쓰기 차단,
alias 재매핑·프로젝트 전환·늦은 응답의 private purge, 전송 중복 방지와 같은 turn 복구,
개별 Lead 선택과 미래 자동 연결 분리, 부분 완료 기준과 Plan 구분, 긴 기준·다음 단계 문구,
UTF-8 한도·목표 prefix, Room 읽기 위치, 긴 원문의 scope/digest 검증,
공백만 있는 중간 페이지를 지나 끝까지 읽기, 다른 Room attempt 또는 물리 projectKey 응답 거부,
초안 본문·문맥에서 내부 식별자 생략과 확인된 요약·turn·task 전달, 서로 다른 worker·Lead 결과의 task 귀속과 미확인 task 생략이다.
선택 공유는 worker·Lead·Room의 실제 출력 버튼·`이 결과로 요청`·`발췌문 포함`·공유 버튼으로 검증하며, 발췌문 포함 전에는 선택 초안을 만들지 않는 것과 참조 초안의
private 본문 비노출, 멀티바이트 발췌 한도·두 해시·byte 범위, 변경된 원문·물리 key·Room·task·초안
거부, teardown과 늦은 응답, 문맥 기능이 없는 호스트에서 발췌문 포함 숨김, text-only 호스트를 포함한다. 더 살펴보기는 실제 광고된 입력 모양,
초기 호출 없음, 선택 화면만 읽기, 페이지 이동·그래프 주변·검색, 미확인 그래프와 0의 구분, null과 0, 읽기 전용 모드,
view·alias·프로젝트 race와 잘못된 응답의 폐기를 검증한다.

이 fixture는 실제 브라우저의 layout·scroll 계산·접근성 트리와 native host capability를 대체하지 않는다.
320px와 736px의 실제 브라우저 렌더, 키보드·스크린리더 상호작용,
실제 호스트의 문맥 추가·clipboard·native Main 열기, 실제 provider 응답의 전체 경로는
이 변경에서 live 검증하지 않았다. 브라우저 runtime 설치는 CDN 접근 문제로 완료되지 않았다.
제품 dependency나 새 CI job은 추가하지 않았다.
Rust reader/projection과 저장소의 `./verify` 결과는 해당 변경의 검증 기록에서 별도로 보고한다.

긴 답변은 plain text로 표시한다. Markdown 렌더링, artifact 다운로드, 새 native provider resume,
GitHub Projects 전체 동기화와 새로운 dashboard 탭은 이번 구현 범위가 아니다.
이 제한을 다른 로드맵 항목의 구현 완료로 표시하지 않는다.
