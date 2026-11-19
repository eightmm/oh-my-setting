# 도구 context 재검토와 output reader 연결

확인일: **2026-11-19 (KST)**. 비교 기준은 main
[`7d9b291`](https://github.com/eightmm/oh-my-setting/commit/7d9b291592a926ce389693141d11ef8269eccfcd)이다.
Dreaming 명세 #280 병합 이후의 공식 문서와 제작자 원문을 현재 MCP caller에 대조했다.
이 문서는 [CC-04](CALLABLE-CONSOLIDATION-WORK-PACKETS.md)의
구현 근거와 남은 적용 조건이다. 새 작업 큐나 독립 플랫폼 계획을 만들지 않는다.

## 이번에 연결한 사용자 경로

`oms output-digest`는 저장된 출력의 `capture_ref`, `read_tool: oms_output_read`,
`read_arguments`를 반환했지만 기준 main에는 해당 MCP 도구가 등록되지 않았다.
페이지·검색·여러 검색어 처리와 파일 검증은 이미 `oms-output-capture`에 있었다.
따라서 큰 출력의 필요한 부분을 다시 읽으라는 안내가 실제 호출로 이어지지 않았다.
근거는 [digest producer](../crates/oms-command-peer/src/digest.rs),
[reference producer](../crates/oms-output/src/lib.rs), [capture core](../crates/oms-output-capture/src/lib.rs),
[MCP 등록과 dispatch](../crates/oms-mcp-server/src/lib.rs)다.

이 tree는 다음 연결을 구현한다.

- `oms_output_read`를 기본/core와 full 목록에 등록한다. Core는 13개, full은 32개가 된다.
  기존 두 profile의 실행 권한 의미는 그대로이며 도구 이름을 알았다는 사실이 읽기 권한은 아니다.
- `OutputDigest::retained_capture`가 canonical `capture_reference`를 사용한다.
  기존 withhold·크기·repository·filename 검사는 wrapper에 남긴다.
- MCP adapter는 기존 capture core로 동일 저장본을 읽고, 페이지·문자열 검색·compact batch 결과를 반환한다.
  저장본 전체의 outbound 검사를 거쳐 응답하므로 요청 페이지 밖의 내용도 검사 대상이다.
- 기존 native MCP fixture에서 실제 digest가 만든 `read_arguments`를 호출하여
  저장본 식별과 반환값을 검증한다. Provider 실행이나 모델 호출은 이 reader의 작업이 아니다.

원문의 무손실 보존 여부는 앞단 capture가 결정한다. 이 변경이 기존 command executor의
출력 한계를 확대하거나, 이미 잘린 출력의 나머지를 복원하는 것은 아니다.
`oms_peer_result.answer_ref`는 별도 operation 소유 결과이며 이번 capture reference와 합치지 않는다.
설치본은 이 변경이 포함된 revision에서만 reader를 제공한다.

## 최신 근거와 적용 판단

아래의 “OMS 판단”은 원문과 현재 소스를 대조한 설계 판단이다.
벤더의 수치나 다른 제품의 사례를 OMS의 성능 측정으로 사용하지 않는다.

| 근거 | 직접 확인한 내용 | OMS 판단 |
|---|---|---|
| [Claude Code MCP 문서][native-mcp], 2026-11-19 확인 | 지원 환경에서 도구 검색은 기본적으로 deferred/on-demand다. `ENABLE_TOOL_SEARCH=auto`를 명시했을 때 10% 임계값을 사용한다. 목록 변경 알림을 받아 native client가 갱신하는 경로도 있다. | 검색·갱신을 새 OMS 실행기로 복제하기 전에 host가 이미 가진 기능을 사용한다. 서버가 목록에서 숨긴 도구까지 검색되는 것은 아니다. |
| [Messages API의 세션 중 tool 변경][inline-tools], [2026-09-22 release note][release-notes] | `inline-tools-2026-09-15`는 전체 정의를 추가하거나 기존 schema를 교체할 수 있다. MCP toolset에는 별도 connector header가 필요하다. | API payload와 native CLI 제어면을 구분한다. 현재 OMS의 static catalog에 동적 producer가 없으므로 inline 변경 엔진은 만들지 않는다. |
| [Programmatic tool calling][ptc], 2026-11-19 확인 | 중간 결과를 코드에서 처리하여 모델 왕복을 줄이는 Messages API 기능이다. Managed Agents는 미지원이며 MCP connector 도구는 직접 호출할 수 없다. | 모든 native MCP 도구를 PTC로 감싸겠다는 계획은 채택하지 않는다. 지금은 기존 page/search/batch core의 실제 reader를 연결한다. |
| [Code execution with MCP][mcp-execution], **2025-11-04** | 필요한 도구 정의와 결과만 context에 넣는 접근을 설명한다. | 최신 발표 날짜와 혼동하지 않는다. 원문을 보존하고 필요한 부분을 다시 읽는 방향의 배경이며 새 sandbox 도입 근거는 아니다. |
| [Ramp Laminar 소개][laminar], **2026-10-20**, [제작자의 Tools 글][laminar-tools], 개별 게시일 미표시 | Catalog 동기화, 도구 검색, 실행을 분리한다. 작은 도구 집합은 직접 제공하며 긴 호출은 비동기로 처리한다. | 분리된 책임을 참고하되 기존 OMS store와 peer owner를 재사용한다. 그림의 100개→5개는 설명 예시이며 OMS 절감률이 아니다. |
| [MCP 2026-07-28 Tools 명세][mcp-tools]와 [SDK migration][mcp-migration] | 구조화 결과는 선택 사항이고 같은 목록의 순서는 결정적으로 유지하는 것이 권장된다. 일회성 CLI/진단용 stdio client의 자동 negotiation은 기본값으로 켜지 말도록 안내한다. | 현재의 static discovery와 dual-era/Tasks 구현을 재사용한다. Doctor의 legacy probe를 이번 reader 수정에 섞어 자동 협상으로 바꾸지 않는다. |

[Claude Code는 큰 성공 텍스트 결과를 자체 session 파일에 보존하는 경로도 제공한다][native-mcp].
그 파일과 OMS가 저장한 capture는 서로 다른 소유자가 관리한다. 이번 reader의 가치는
**OMS가 이미 반환한 원문 참조를 provider와 무관한 MCP 경로에서 실제로 읽을 수 있게 하는 것**이다.
이 재검토의 구현 근거는 직접 읽은 공식 문서와 제작자 글이다. 영상 소개문만으로 대담의
발언이나 시연 성공을 추정하지 않는다.

## Reader 계약과 검증 경계

호출자는 digest의 `read_tool`과 `read_arguments`를 그대로 사용한다.
검색이나 페이지 이동이 필요하면 기존 core가 허용하는 field만 덧붙인다.

| 요청 | 기존 core의 의미 |
|---|---|
| `capture_ref` + `repo` | Physical repository, 저장 파일명, SHA-256에 결합된 식별자. 권한 증명·검증 통과 receipt가 아님 |
| `offset`, `limit` | UTF-8로 투영한 텍스트의 byte 경계. 기본/최대 page 16 KiB, `limit` 최소 4 bytes |
| `query` | 1–256 UTF-8 bytes의 literal 검색. `limit`과 함께 사용하지 않음 |
| `compact: true`, `queries` | 1–8개 literal, 전체 1,024 bytes 이하. `query`/`limit`과 함께 사용하지 않음 |
| 다음 페이지·검색 인자 | 응답의 continuation 값을 사용. Compact 결과는 `read_base`와 delta를 합침 |

저장본 읽기 한도는 기존 **64 MiB**다. SHA는 저장 bytes를 검사하며 잘못된 UTF-8의
표시 변환과 구분한다. 반환되는 `capture_bytes`, `content_bytes`, `encoding`, `has_more`를
함께 해석한다. 내용을 표시할 수 없거나 파일이 변경·삭제되었으면 typed error로 반환하며
잘라낸 JSON을 성공 응답으로 내보내지 않는다.
JSON text payload는 직렬화된 UTF-8 기준 65,536 bytes까지 허용하며,
MCP envelope와 그 안의 문자열 escaping 비용은 이 payload 상한과 별개다.

검증 owner는 [capture contract](../crates/oms-output-capture/tests/capture_contract.rs),
[output digest의 기존 테스트](../crates/oms-output/src/lib.rs),
[MCP native fixture](../crates/oms-mcp-server/tests/native.rs)다.
Reference scope/SHA, descriptor 기반 no-follow·single-link 검사, root 변경,
delegated child의 repo 범위, 전체 저장본의 outbound withhold, 응답 cap을 함께 보존한다.
새 저장소·원본 command 재실행·임의 경로 읽기는 추가하지 않는다.

## 후속 작업은 실제 consumer가 있을 때

1. **Native tool search와 full 목록의 연결.** 현재 full profile은 모든 등록 도구를 보여 주지만
   기본 core에는 일부만 보인다. Native search가 core 밖 도구까지 찾을 것이라고 가정하지 않는다.
   Claude Code의 지원 경로에서 실제 discovery→search→call fixture를 확보한 뒤 provider별
   노출 정책을 정한다. 전체 provider의 설치 기본값을 이번 PR에서 바꾸지 않는다.
2. **목록 변경의 실제 producer.** 현재 catalog는 binary에 compile되고 `listChanged=false`다.
   Runtime plugin이 schema를 바꾸는 producer가 생기면 해당 owner에서 protocol에 맞는
   invalidation과 client refresh를 연결한다. 검색 요청의 부작용으로 connection별 도구를 해금하지 않는다.
3. **구조화 결과와 비용 관측.** Peer 결과의 `structuredContent`는 실제 consumer 필요가 확인될 때
   CC-10의 기존 schema/handler fixture로 검증한다. JSON text까지 복제하면 전송량은 늘 수 있다.
   Token·cache·왕복 비용은 기존 RF-05/06에서 측정하며 byte 크기를 token 절감으로 보고하지 않는다.

Dreaming 입력 보존·memory validity는 [DR-01–06](MEMORY-CONSOLIDATION.md),
native assignment/취소는 [LC](ORCHESTRATOR-LIFECYCLE-IMPLEMENTATION.md), 모델·effort·Main
선택과 native prompt 변경은 조사 당시 열린 #281–287의 owner에 둔다.
이번 변경은 그 작업의 완료를 대신 주장하지 않는다.

[native-mcp]: https://code.claude.com/docs/en/mcp
[inline-tools]: https://platform.claude.com/docs/en/build-with-claude/mid-conversation-system-messages
[release-notes]: https://platform.claude.com/docs/en/release-notes/overview
[ptc]: https://platform.claude.com/docs/en/agents-and-tools/tool-use/programmatic-tool-calling
[mcp-execution]: https://www.anthropic.com/research/code-execution-with-mcp
[laminar]: https://builders.ramp.com/post/building-laminar
[laminar-tools]: https://builders.ramp.com/post/building-laminar/tools
[mcp-tools]: https://modelcontextprotocol.io/specification/2026-07-28/server/tools
[mcp-migration]: https://ts.sdk.modelcontextprotocol.io/v2/migration/support-2026-07-28
