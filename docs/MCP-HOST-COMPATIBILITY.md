# OMS 플러그인과 MCP UI의 공식 host 지원

확인일: **2026-11-24 (KST)**. 이 문서는 공식 지원 경로와 OMS의 구현 계획을 구분한다.
사용자의 실제 앱·계정에서 설치하거나 live rendering을 시험한 결과가 아니다.
상위 결정과 작업 순서는 [공통 제어 화면 로드맵](CONTROL-SURFACE-ROADMAP.md),
진행 추적은 [#438](https://github.com/eightmm/oh-my-setting/issues/438)을 따른다.

## 1. 공통으로 공유하는 부분

| 계층 | 담당하는 일 | 지원만으로 보장되지 않는 것 |
|---|---|---|
| MCP | OMS 도구 호출, resource 조회, structured/text 결과 전달 | HTML 화면이나 특정 위치 |
| MCP Apps | `ui://` HTML resource, iframe, host와 view 사이 표준 bridge | 모든 앱의 side panel·메뉴·설정 지원 |
| Host extension | 대화 옆 탭, global 진입점, file viewer 등 host의 화면 배치 | 다른 host에서 같은 metadata 동작 |
| Native UI adapter | Claude Code Mods처럼 host 고유 element로 화면 구성 | 같은 HTML bundle의 직접 재사용 |

[MCP Apps 표준 개요][mcp-apps]에 따라 tool의 `_meta.ui.resourceUri`가
`text/html;profile=mcp-app` resource를 가리킨다. Host가 HTML을 읽어 iframe을 만들고,
view가 `ui/initialize`로 연결한 뒤 tool input/result를 받는다. UI의 `tools/call`은
host를 거쳐 server에 전달된다. 버튼마다 새 모델 turn이 필요한 것은 아니다.
UI가 없는 client를 위해 읽을 수 있는 `content`와 `structuredContent`를 함께 유지한다.

OMS 설계에서는 이 경로 모두 같은 runtime projection과 typed action owner를 사용한다.
UI host, Main harness, inference provider, account와 물리적 실행 host를 분리한다.
어느 앱에서 화면을 여는지는 Main 선택이나 실행 소유권 변경을 뜻하지 않는다.

## 2. 공식 지원 matrix

| Host surface | 공식 UI 경로 | 대화 옆 화면과 조건 | OMS live 검증 |
|---|---|---|---|
| ChatGPT desktop 안의 Codex | MCP Apps + OpenAI extensions | `thread` content tab 지원; 실제 배치는 host 결정 | 미실시 |
| ChatGPT Work web | MCP Apps + OpenAI extensions | 공식 spec의 Web 지원 표 적용; classic ChatGPT web와 구분 | 미실시 |
| Codex CLI | MCP tools·plugin browser | 이 문서에서 HTML side panel renderer 지원을 확인하지 못함 | 미실시 |
| Codex IDE extension | 직접 MCP 연결과 plugin 설치를 구분 | 공식 문서는 plugin 미지원; desktop UI 지원을 전이하지 않음 | 미실시 |
| Claude Chat web/Desktop | MCP Apps HTML | `fullscreen` 요청이 side panel 또는 전체 화면으로 표현; rollout 조건 있음 | 미실시 |
| Claude Code CLI/Desktop Code/VS Code | MCP 결과는 text; native Mods UI 별도 | 지원 버전의 Mod pane; iframe renderer와 다른 구현 | 미실시 |
| Antigravity App | MCP Apps + AGY extensions | v2.34.0+의 `aux_pane`, `global`, `file` | 미실시 |
| Antigravity CLI/IDE | MCP tools | `ui://` iframe 미지원, text fallback | 미실시 |

Matrix의 근거는 아래 host별 공식 문서다. 표의 미실시는 platform 미지원 판정이 아니다.
실제 설치 대상의 버전·계정·조직 정책과 capability를 읽어 feature별로 확인한다.

## 3. ChatGPT desktop / Codex

[제품 변경 기록][openai-changelog]은 2026-07-09 Codex 앱이 macOS/Windows의
ChatGPT desktop 앱에 통합됐다고 설명한다. [Plugin 안내][openai-plugins]는 desktop
Codex의 plugin 사용과 CLI의 plugin browser를 지원하고 IDE extension은 구분한다.

[OpenAI MCP Extensions spec][openai-spec]은 Desktop의 `thread`를 지원으로 표기한다.
사용자가 대화 안의 새 content tab으로 MCP App을 열 때 tool에 `{}`가 전달되고,
thread마다 별도 UI instance가 생긴다. 최소 metadata의 모양은 다음과 같다.

```json
{
  "_meta": {
    "ui": { "resourceUri": "ui://oms/dashboard" },
    "openai/ui": { "entrypoints": [{ "type": "thread" }] }
  }
}
```

이 resource 이름은 설계 예시이며 현재 구현된 OMS 도구를 뜻하지 않는다.
`global`은 전역 진입점, `file`은 지원 파일의 viewer 확장이다. `thread`를 물리적으로
항상 오른쪽인 고정 좌표로 해석하지 않는다. Spec의 Web 열은 **Work browser**이며
classic ChatGPT를 제외한다. 구버전 standalone Codex에도 자동 적용된다고 가정하지 않는다.

OMS는 지원 surface 안에서 UI를 제공한다. Host 앱 전체 DOM/CSS 변경이나 다른 대화의
기록 추출을 이 entrypoint 계약에 포함하지 않는다. [개발자 확장 안내][openai-extensions]와
지원 matrix를 착수 시 재확인하며 최소 버전을 근거 없이 만들지 않는다.

## 4. Claude: Chat/Desktop과 Code를 별도 adapter로 취급

### Chat web / Desktop의 HTML

[MCP Apps quickstart][claude-quickstart]는 Claude의 HTML rendering과 Claude Code의
text-only tool 소비를 구분한다. Remote connector URL은 Anthropic infrastructure에서
접속하므로 그 경로에서 사용자의 `localhost`에 접근할 수 없다. Desktop 설정 파일을
통한 local MCP 연결은 별도 경로다. 첫 로컬 실증에 원격 hosting이 필수는 아니다.

[Display modes][claude-display]에 따라 view는 `inline`과 `fullscreen`을 선언하고,
사용자 click/key 동작에서 `app.requestDisplayMode({ mode: "fullscreen" })`를 요청한다.
Claude는 side panel과 전체 화면을 모두 `fullscreen`으로 보고한다. 실제 크기는
`hostContext.containerDimensions`로 읽는다. `sidebar`라는 SDK mode를 만들지 않는다.

Side panel은 일부 조직과 대화에 제공된다. 반환 mode와 사용 가능한 mode를 확인해
fallback하며, 지원 계정 전체에 항상 열린다고 표시하지 않는다. 자동 확장 선호는
resource의 `_meta.ui.preferredDisplayMode`로 요청할 수 있지만 **Claude 확장**이다.
이 필드의 위치와 표준화 상태는 착수 시 재확인한다.

[Global entrypoint][claude-entrypoints]는 `openai/ui`의 `global` metadata를 받아
Artifacts에서 앱을 열고 sidebar에 고정하는 경로다. 제한된 조직의 unified experience와
remote custom/Verified connector 조건이 있으며 Community listing은 제외된다.
이는 OpenAI의 모든 extension 또는 `window.openai` API 호환성의 증거가 아니다.

### Claude Code의 native Mods

[Mods overview][claude-mods]와 [UI API][claude-mod-ui]는 JS/TS event handler가 native
element를 그리는 경로를 제공한다. `$.ui.open({ id, title })`로 pane을 열고 `ui.render`의
`Pane` handler에서 내용을 반환한다. 넓은 fullscreen terminal에서는 transcript 옆,
그 외에는 prompt 위에 배치된다. MCP의 HTML resource를 그대로 terminal에 띄우는 기능은 아니다.

| 실행 surface | 문서의 버전·제약 |
|---|---|
| Terminal | Claude Code 2.1.287+ |
| Desktop Code tab | 내장 Claude Code 2.1.286+; 앱 버전과 구분 |
| VS Code | extension 2.1.299+; feature flags를 받지 않는 환경은 2.1.304+ |
| `claude -p` / Agent SDK | handler 실행과 UI rendering을 구분; UI는 렌더링하지 않음 |
| WSL/SSH/cloud | 설치 위치와 session 종류별 제한; local 성공을 전이하지 않음 |

OMS Mod는 선택형 projection renderer로 계획한다. Mod가 사용자 권한으로 실행되는
native code라는 사실은 OMS의 lease·approval·typed writer 검사를 생략할 근거가 아니다.
공통 데이터 계약은 공유하되 HTML과 native element renderer를 억지로 하나로 합치지 않는다.

## 5. Antigravity

[AGY MCP Apps][agy-apps]는 App v2.34.0+와
`capabilities.extensions["io.modelcontextprotocol/ui"]`를 명시한다. Side panel metadata는
다음 형태다. 이 역시 OMS의 구현 완료 예제가 아니다.

```json
{
  "_meta": {
    "ui": { "resourceUri": "ui://oms/dashboard" },
    "com.google.antigravity/ui": {
      "entrypoints": [{ "type": "aux_pane", "title": "OMS" }]
    }
  }
}
```

`aux_pane`은 tool 완료나 메뉴로 열리고 `(server, tool)`별 탭 하나를 갱신한다.
수동 launch는 `{}`다. `global`은 대화 밖 full-page이고 conversation-only capability가
없다. `file`은 opaque file reference를 받는다. `aux_pane`/`global` tool은 inline 대신
그 surface에 표시된다. CLI/IDE는 iframe을 렌더링하지 않는다.

CLI는 `_meta.ui.visibility`를 무시할 수 있다. 따라서 app-only helper도 server에서
caller·scope를 검사해야 한다. View의 tool 호출은 owning server에 한정되고 닫을 때
진행 중 호출이 중단된다. Teardown 알림과 persistent browser storage에 의존하지 않는다.
OMS 실행 수명과 기록은 backend에 두고 재접속 시 같은 run을 조회한다.

## 6. 기존 OMS에서 재사용할 부분과 남은 작업

현재 [MCP server](../crates/oms-mcp-server/src/lib.rs)는 graph UI resource를 제공한다.
하지만 [graph HTML](../crates/oms-mcp-server/src/project-graph.html)은
`window.openai.toolOutput`/`sendFollowUpMessage`와 Codex 전용 문구를 사용한다.
이를 표준 bridge와 capability별 host adapter로 연결해야 한다. Resource metadata만으로
Claude/AGY live rendering이나 control parity를 주장하지 않는다.

| 기존 경로 | 재사용 및 변경 경계 |
|---|---|
| [Claude marketplace](../.claude-plugin/marketplace.json) | `oms-skills`와 `oms-panel`을 따로 선택할 수 있다. 네이티브 pane은 설치된 `oms`를 호출하며 현재 프로젝트에서 `/oms-panel`로 연다 |
| [MCP integration writer](../crates/oms-command-lifecycle/src/integrations.rs) | Claude local stdio 등 기존 설정·소유권 재사용 |
| [AGY bundle](../plugins/antigravity/plugin.json)와 [installer](../crates/oms-command-lifecycle/src/plugins.rs) | 기존 `mcp_config.json` 생성과 설치·제거 ownership 재사용 |
| Runtime·panel-core·session adapters | 같은 projection, Main binding, generation과 기존 action writer 유지 |

[Claude의 component matrix][claude-plugin-support]는 Chat/Cowork/Code가 같은 folder의
서로 다른 subset을 읽는다고 명시한다. Chat에서 plugin-bundled local MCP가 무시되는 것과
Desktop 개발 설정의 local server 연결은 다른 경로다. Local CLI용 skill은 remote Chat의
typed MCP 도구 안내로 조정하며 임의 shell proxy를 추가하지 않는다.
[AGY packaging][agy-plugins]의 manifest와 `mcp_config.json`도 별도 adapter로 유지한다.

## 7. 구현·검증 순서와 canonical Issue

1. **#432/#433:** 기존 runtime의 bounded 읽기 projection과 text 결과를 공통 계약으로 확정한다.
2. **#438:** 표준 bridge를 먼저 연결하고 OpenAI `thread`, Claude display modes,
   AGY `aux_pane`을 각각 capability 확인 뒤 사용한다. Claude Code Mod는 독립 선택 slice다.
3. **#441:** 설치·인증·연결·UI rendering·관측·제어 상태를 나눠 표시한다.
   Local Claude Desktop/AGY App의 read-only 실증은 #435의 remote ChatGPT pilot과 병행 가능하다.
4. **#434/#435:** remote caller/repo/Room binding 검증 후 private ChatGPT 연결을 실증한다.
   로컬 pilot도 자신의 caller·scope 경계를 검증하며 remote 권한을 자동 상속하지 않는다.
5. **#443:** 빈 인자 launch, 새 conversation, link와 context reference로 같은 run을 다시 연다.
   UI 복원·conversation 전송을 Main 재시작이나 native message delivery로 계산하지 않는다.
6. **#445:** host별 bundle·버전·연결 경로와 private/public 배포·rollback 증거를 남긴다.
   실제 control은 별도 #436/#437 계약과 검증을 통과한 기능만 노출한다.

최소 검증 표의 key는 `host / surface / version / account-policy / transport / feature`다.
각 행에는 `supported`, `partial`, `unsupported`, `unknown`, fallback과 함께
**공식 문서 근거 / fixture 결과 / live 결과**를 분리해 기록한다. 문서 지원을 live 성공으로
승격하거나 한 host의 성공으로 전체 matrix를 채우지 않는다.

Read-only pilot은 동일 repo/run/revision 확인, `{}` 초기 화면, 없는 capability의 fallback,
stale/gap/오류 표시, view 종료·재접속, 중복 Main 방지와 권한 없는 호출 거절을 검증한다.
기존 owner의 가장 가까운 fixture를 확장하고 실제 설치 대상에서의 rendering은 별도 증거로
남긴다. 이 문서 등록은 plugin 설치·원격 endpoint 개방·유료 실행의 완료 기록이 아니다.

[mcp-apps]: https://apps.extensions.modelcontextprotocol.io/api/documents/overview.html
[openai-changelog]: https://learn.chatgpt.com/docs/whats-new
[openai-plugins]: https://learn.chatgpt.com/docs/plugins
[openai-spec]: https://github.com/openai/mcp-extensions/blob/main/docs/spec.md
[openai-extensions]: https://developers.openai.com/plugins/build/extensions
[claude-quickstart]: https://claude.com/docs/connectors/building/mcp-apps/quickstart
[claude-display]: https://claude.com/docs/connectors/building/mcp-apps/display-modes
[claude-entrypoints]: https://claude.com/docs/connectors/building/mcp-apps/entry-points
[claude-mods]: https://code.claude.com/docs/en/plugins/mods/overview
[claude-mod-ui]: https://code.claude.com/docs/en/plugins/mods/interface
[claude-plugin-support]: https://claude.com/docs/plugins/platform-support
[agy-apps]: https://antigravity.google/docs/mcp-apps
[agy-plugins]: https://antigravity.google/docs/build-plugins
