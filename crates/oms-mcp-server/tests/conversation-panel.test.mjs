// Run with node --test crates/oms-mcp-server/tests/conversation-panel.test.mjs.
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { setImmediate } from 'node:timers/promises';
import test from 'node:test';
import { createContext, runInContext } from 'node:vm';

const html = readFileSync(new URL('../src/conversation-panel.html', import.meta.url), 'utf8');
const script = html.match(/<script>([\s\S]+?)<\/script>/)[1];

class Element extends EventTarget {
  value = '';
  children = [];
  attributes = new Map();
  scrollTop = 0;
  get scrollHeight() { return this.children.length; }
  get textContent() { return this.children.map(child => child?.textContent ?? child).join(''); }
  set textContent(value) { this.children = [String(value ?? '')]; }
  add() {}
  append(...children) { this.children.push(...children); }
  prepend(...children) { this.children.unshift(...children); }
  setAttribute(name,value) { this.attributes.set(name,value); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  focus() { this.focused = (this.focused ?? 0) + 1; }
  select() { this.selected = true; }
  replaceChildren(...children) { this.children = children; }
  querySelectorAll() { return []; }
  setCustomValidity() {}
}

async function panel(projects, dashboard = {}, options = {}) {
  const elements = new Map([...html.matchAll(/\bid="([^"]+)"/g)].map(([, id]) => [id, new Element()]));
  const get = id => elements.get(id);
  get('launch').elements = {main:{value:'codex'}};
  get('launch').reset = () => { get('brief').value = ''; };
  const window = new EventTarget();
  window.scrollTo = (_x,y) => { window.scrollY = y; };
  const calls = [];
  const host = {status:null, failStart:true, replies:[], messages:[], notices:[]};
  function notify(payload) {
    window.dispatchEvent(Object.assign(new Event('message'), {
      source:window.parent, origin:'https://oms.example', data:{jsonrpc:'2.0', ...payload}
    }));
  }
  function tool(name, args) {
    calls.push({name, args});
    if (host.tools?.[name]) return host.tools[name](args);
    switch (name) {
      case 'oms_list_projects': return {projects,readOnly:options.readOnly};
      case 'oms_panel_snapshot': return {dashboard:{project:args.project, ...dashboard}};
      case 'oms_list_approvals': return {approvals:[]};
      case 'oms_start_main':
        if (host.failStart) throw new Error('start outcome unavailable');
        return {status:'started', requestId:args.requestId, room:'oms-app-' + args.requestId,
          provider:args.main, attempt:'fixture-attempt'};
      case 'oms_start_status':
        if (!host.status) throw new Error('recovery temporarily unavailable');
        return {requestId:host.responseId ?? args.requestId, status:host.status,
          room:host.status === 'not_found' ? null : 'oms-app-' + args.requestId,
          attempt:host.status === 'recorded' ? 'recorded-attempt' : null,
          provider:host.status === 'recorded' ? 'codex' : null,
          state:host.status === 'recorded' ? 'working' : null, terminal:host.status === 'recorded' ? false : null};
      default: throw new Error('Unexpected tool ' + name);
    }
  }
  window.parent = {postMessage(message) {
    const {id, method, params} = message;
    if (!method) { host.replies.push(JSON.parse(JSON.stringify(message))); return; }
    if (id == null) { host.notices.push(method); return; }
    let result = {};
    if (method === 'ui/initialize') {
      host.initialize = params;
      result = {hostInfo:{name:options.host || 'Codex',version:options.version},hostContext:{displayMode:options.mode,locale:options.locale},hostCapabilities:{message:options.message ? {text:{}} : undefined}};
    }
    if (method === 'ui/message') { host.messages.push(params); result = {isError:host.rejectMessage === true}; }
    if (method === 'tools/call') {
      try {
        const value = tool(params.name, params.arguments);
        result = params.name === 'oms_panel_chat' ? {_meta:{chat:value}, structuredContent:{count:0}} : params.name === 'oms_assign_result' ? {_meta:{assignment:value}} : {structuredContent:value};
      }
      catch (error) { result = {isError:true, content:[{type:'text', text:error.message}]}; }
      if ((params.name === 'oms_panel_snapshot' && host.delay === params.arguments.project && host.delay) || host.delayTool === params.name) {
        host.delay = null;
        host.delayTool = null;
        host.delayedId = id;
        host.release = () => notify({id, result});
        return;
      }
    }
    queueMicrotask(() => notify({id, result}));
  }};
  // Static localized nodes: reuse the id-ed stub when present so tests can read them.
  const staticNodes = [...html.matchAll(/<([a-z0-9]+)((?:\s[^<>]*)?\sdata-i18n(?:-attr)?="[^<>]*)>([^<]*)/g)].map(([, , attrs, inner]) => {
    const attr = name => attrs.match(new RegExp('\\s' + name + '="([^"]*)"'))?.[1];
    const node = elements.get(attr('id')) ?? new Element();
    for (const name of ['data-i18n', 'data-i18n-attr']) if (attr(name)) node.attributes.set(name, attr(name));
    if (attr('data-i18n')) node.textContent = inner;
    for (const pair of (attr('data-i18n-attr') ?? '').split(';').filter(Boolean)) node.attributes.set(pair.split(':')[0], attr(pair.split(':')[0]));
    node.initialText = inner;
    return node;
  });
  const document = {getElementById:get, createElement:() => new Element(), title:'',
    querySelectorAll: selector => staticNodes.filter(node => node.attributes.has(selector.slice(1, -1)))};
  const context = createContext({window, document, Option:Element, setTimeout, clearTimeout, crypto:{randomUUID},
    ...(options.navigator && {navigator:options.navigator}), ...(options.storage && {localStorage:options.storage})});
  runInContext(script, context);
  await setImmediate();
  return {get, elements, calls, host, notify, staticNodes, document, run: expression => runInContext(expression, context),
    async event(id, type) {
      get(id).dispatchEvent(new Event(type, {cancelable:true}));
      if (type === 'click') get(id).onclick?.();
      await setImmediate();
    },
    async open(method, params) {
      notify({method:'ui/notifications/tool-' + method, params});
      await setImmediate();
    }
  };
}

async function ambiguousStart(initialStatus = null) {
  const p = await panel(['one']);
  p.host.status = initialStatus;
  p.get('brief').value = 'Review the change';
  await p.event('launch', 'submit');
  return p;
}

for (const status of ['reserved', 'recorded', 'not_found']) {
  test('same goal reconciles ' + status + ' before any retry', async () => {
    const p = await ambiguousStart();
    const starts = () => p.calls.filter(c => c.name === 'oms_start_main');
    assert.equal(starts().length, 1);
    const requestId = starts()[0].args.requestId;
    p.host.status = status;
    p.host.failStart = false;
    const mark = p.calls.length;
    await p.event('launch', 'submit');
    const calls = p.calls.slice(mark).filter(c => ['oms_start_status', 'oms_start_main'].includes(c.name));
    assert.equal(calls[0]?.name, 'oms_start_status');
    assert.ok(calls.every(c => c.args.requestId === requestId && c.args.project === 'one'));
    assert.equal(starts().length, status === 'not_found' ? 2 : 1);
    if (status === 'recorded') assert.match(p.get('receipt').textContent, /recorded-attempt/);
  });
}

test('automatic reserved receipt clears the initial launch error style', async () => {
  const p = await ambiguousStart('reserved');
  const start = p.calls.find(c => c.name === 'oms_start_main');
  assert.ok(p.get('receipt').textContent.includes('oms-app-' + start.args.requestId));
  assert.doesNotMatch(p.get('receipt').className || '', /\berror\b/);
});

test('host ping and unsupported requests with a pending snapshot ID leave the snapshot in flight', async () => {
  const p = await panel(['one']);
  p.host.delay = 'one';
  await p.event('refresh', 'click');
  const id = p.host.delayedId;
  try {
    p.notify({method:'ping', id});
    p.notify({method:'unknown/request', id});
    p.notify({id});
    await setImmediate();
    assert.deepEqual(p.host.replies[0], {jsonrpc:'2.0', id, result:{}});
    assert.equal(p.host.replies[1]?.error.code, -32601);
    assert.equal(p.get('view').ariaBusy, 'true');
  } finally { p.host.release(); }
  await setImmediate();
  assert.equal(p.get('view').hidden, false);
  assert.equal(p.get('view').ariaBusy, 'false');
});

test('teardown acknowledges the host and discards late reads without sending more tool calls', async () => {
  const p = await panel(['one']);
  p.host.delay = 'one';
  await p.event('refresh', 'click');
  const mark = p.calls.length;
  p.notify({method:'ui/resource-teardown', id:'host-close', params:{}});
  p.host.release();
  await p.open('input', {arguments:{project:'one'}});
  assert.deepEqual(p.host.replies, [{jsonrpc:'2.0', id:'host-close', result:{}}]);
  assert.equal(p.get('view').hidden, true);
  assert.equal(p.calls.length, mark);
});

for (const failure of ['unavailable', 'different request']) {
  test('manual retry stays read-only when lookup returns ' + failure, async () => {
    const p = await ambiguousStart();
    if (failure === 'different request') {
      p.host.status = 'not_found';
      p.host.responseId = randomUUID();
    }
    p.host.failStart = false;
    const mark = p.calls.length;
    await p.event('launch', 'submit');
    const calls = p.calls.slice(mark);
    assert.ok(calls.some(c => c.name === 'oms_start_status'));
    assert.ok(!calls.some(c => c.name === 'oms_start_main'));
  });
}

test('late failed open preserves the newer manually selected project and its draft', async () => {
  const p = await panel(['one', 'two']);
  await p.open('input', {arguments:{project:'one'}});
  p.get('project-choice').value = 'two';
  await p.event('project-choice', 'change');
  p.get('brief').value = 'Draft for two';
  await p.open('result', {isError:true});
  assert.equal(p.get('view').hidden, false);
  assert.equal(p.get('project').textContent, 'two');
  assert.equal(p.get('brief').value, 'Draft for two');
  await p.open('result', {structuredContent:{panel:'OMS',project:'one'}});
  assert.equal(p.get('project').textContent, 'one');
  assert.equal(p.get('brief').value, '');
});

test('unbound open preserves the draft for the only configured project', async () => {
  const p = await panel(['one']);
  p.get('brief').value = 'Still composing';
  await p.open('input', {arguments:{}});
  assert.equal(p.get('brief').value, 'Still composing');
  await p.open('result', {structuredContent:{panel:'OMS',project:null}});
  assert.equal(p.get('project').textContent, 'one');
  assert.equal(p.get('brief').value, 'Still composing');
});

test('orphan open error fails closed and a later result-only open selects its project', async () => {
  const p = await panel(['one']);
  await p.open('result', {isError:true});
  assert.equal(p.get('view').hidden, true);
  await p.open('result', {structuredContent:{panel:'OMS',project:'one'}});
  assert.equal(p.get('view').hidden, false);
  assert.equal(p.get('project').textContent, 'one');
});

test('an unrelated successful result can open a project while another open is outstanding', async () => {
  const p = await panel(['one', 'two']);
  await p.open('input', {arguments:{project:'one'}});
  await p.open('result', {structuredContent:{panel:'OMS',project:'two'}});
  assert.equal(p.get('view').hidden, false);
  assert.equal(p.get('project').textContent, 'two');
});

test('active Lead cards, past executions and repository verification use their own records', async () => {
  const p = await panel(['one'], {
    scope:'repository', currentRoom:'room-two', active:3,
    main:{provider:'pi', room:'room-two'},
    conversationMain:{source:'room-session', state:'joined', provider:'codex', room:'room-one', participant:'chat-owner', incomplete:true},
    activeAttempts:[
      {provider:'claude', role:'lead', task:'live-task', room:'room-one', state:'working'},
      {provider:'agy', role:'lead', task:'unrelated', room:'room-two', state:'working'},
      {provider:'codex', role:'advisor', tool:'peer-review', room:'room-one', state:'working'}
    ],
    attempts:[{provider:'codex', role:'lead', task:'old-task', room:'room-one', state:'done'}],
    acceptance:{available:true, verified:1, total:3, complete:false},
    reviews:{available:true, passed:2, failed:1, unknown:0},
    operations:[{kind:'verify', artifactId:'check-17', status:'failed', exit:1}]
  });
  const cards = p.get('leads').textContent;
  assert.match(cards, /Claude Leadlive-task작업 중/);
  assert.doesNotMatch(cards, /Codex Lead|old-task|unrelated/);
  assert.match(p.get('activity').textContent, /old-task · room-one/);
  assert.equal(p.get('room').textContent, 'room-one');
  assert.equal(p.get('participant').textContent, 'chat-owner');
  assert.match(p.get('header-main').textContent,/Codex Main$/);
  assert.match(p.get('main-state').textContent, /작업방 목록 일부 조회/);
  assert.match(p.get('repo-team').textContent, /AGY Lead · room-two · unrelated/);
  assert.match(p.get('repo-team').textContent, /Pi Main · room-two/);
  assert.match(p.get('criteria').textContent, /1 \/ 3개 · 미완료/);
  assert.match(p.get('reviews').textContent, /보관된 리뷰 기록 · 통과 2 · 실패 1 · 판정 없음 0/);
  assert.match(p.get('operations').textContent, /verifycheck-17failed · 종료 코드 1/);
});

test('unbound partial snapshots leave unseen activity and absent verification unknown', async () => {
  const p = await panel(['one'], {active:9, activeAttempts:[{tool:'panel-main', provider:'agy', room:'room-two', state:'starting'}],
    currentRoom:'another-room', main:null,
    acceptance:{available:false}, reviews:{available:false}, degraded:true});
  assert.equal(p.get('current-main').textContent, 'Main 미확인');
  assert.equal(p.get('room-leads').hidden, true);
  assert.equal(p.get('identity').hidden, true);
  assert.equal(p.get('room').textContent, '미확인');
  assert.match(p.get('repo-team').textContent, /AGY Main · room-two/);
  assert.match(p.get('status').textContent, /일부 상태/);
  assert.match(p.get('criteria').textContent, /완료 기준 미확인/);
  assert.match(p.get('reviews').textContent, /리뷰 기록 미확인/);
  assert.match(p.get('operations').textContent, /결과물 미확인/);
});

test('a reserved launch and an incomplete Lead sample cannot confirm a different chat identity', async () => {
  const p = await panel(['one'], {active:9, activeAttempts:[],
    conversationMain:{source:'room-session', state:'joined', provider:'codex', room:'conversation-room', participant:'owner'}});
  assert.match(p.get('leads').textContent, /Lead를 확인하지 못했습니다/);
  p.host.status = 'reserved';
  p.get('launch').elements.main.value = 'claude';
  p.get('brief').value = 'Another task';
  await p.event('launch', 'submit');
  assert.match(p.get('receipt').textContent, /별도 Main 시작 요청 접수됨/);
  assert.match(p.get('header-main').textContent,/Codex Main$/);
  assert.equal(p.get('room').textContent, 'conversation-room');
});

test('a slow refresh keeps the previous timestamp and lets the user change projects safely', async () => {
  const p = await panel(['one','two'], {updated:'2026-11-24T08:00:00Z'});
  p.get('project-choice').value = 'one';
  await p.event('project-choice', 'change');
  const timestamp = p.get('updated').textContent;
  p.host.delay = 'one';
  await p.event('refresh', 'click');
  assert.equal(p.get('view').ariaBusy, 'true');
  assert.match(p.get('status').textContent, /이전 조회 결과/);
  assert.equal(p.get('updated').textContent, timestamp);
  assert.equal(p.get('project-choice').disabled, false);
  p.get('project-choice').value = 'two';
  await p.event('project-choice', 'change');
  p.host.release();
  await setImmediate();
  assert.equal(p.get('project').textContent, 'two');
  assert.equal(p.get('view').ariaBusy, 'false');
  assert.equal(p.get('launcher').open, false);
});

test('scoped chat navigation reads private bodies, paginates backwards and preserves history on refresh', async () => {
  const main = {source:'room-session',state:'joined',provider:'codex',room:'room-one',participant:'owner'};
  const p = await panel(['one'], {conversationMain:main});
  const chat = args => ({...args, source:'room',messages:[{seq:args.before ? 1 : 4,text:args.before ? 'earlier' : 'latest'}],nextBefore:args.before ? null : 4});
  p.host.tools = {oms_panel_chat:chat};
  assert.ok(!p.calls.some(c => c.name === 'oms_panel_chat'));
  await p.event('header-main','click');
  assert.equal(p.get('pane-chat').hidden,false);
  assert.deepEqual(p.calls.at(-1).args.project,'one');
  assert.match(p.get('chat-messages').textContent,/latest/);
  await p.event('chat-older','click');
  assert.equal(p.calls.at(-1).args.before,4);
  assert.match(p.get('chat-messages').textContent,/earlier.*latest/);
  await p.event('refresh','click');
  assert.match(p.get('chat-messages').textContent,/earlier.*latest/);
  await p.event('chat-back','click'); assert.equal(p.get('pane-overview').hidden,false);
  await p.event('tab-chat','click'); assert.match(p.get('chat-messages').textContent,/earlier.*latest/);
});

test('a project change discards a pending private result and clears selected members', async () => {
  const p = await panel(['one','two'], {conversationMain:{source:'room-session',state:'joined',provider:'codex',room:'r',participant:'owner'}});
  p.get('project-choice').value = 'one'; await p.event('project-choice','change');
  p.host.tools = {oms_panel_chat:args => ({...args,source:'room',messages:[{seq:1,text:'old private result'}]})};
  p.host.delayTool = 'oms_panel_chat';
  await p.event('header-main','click');
  p.get('project-choice').value = 'two'; await p.event('project-choice','change');
  p.host.release(); await setImmediate();
  assert.equal(p.get('project').textContent,'two');
  assert.equal(p.get('pane-overview').hidden,false);
  assert.doesNotMatch(p.get('chat-messages').textContent,/private/);
  assert.equal(p.get('chat-back').hidden,true);
});

test('usage and Git stay on demand and preserve scope, unknown measurements and PR head evidence', async () => {
  const p = await panel(['one'], {git:{available:true,branch:'main',dirty:false}});
  const pr = {number:7,headBranch:'topic',baseBranch:'main',scope:'linkedWorktree',matchesLocalHead:false,url:'https://other.example/pull/7',
    draft:true,checks:{passed:1,failed:0,pending:2,other:0}};
  p.host.tools = {oms_panel_git_status:() => ({git:{available:true,branch:'main'},github:{status:'available',repository:'org/repo',pullRequests:[pr]}}),
    oms_panel_usage:() => ({providers:[{provider:'codex',windows:[{used:0,name:'주간'}]}],attempts:[{id:'a1',provider:'codex',tokens:null,source:null}],
      providerTotals:[{provider:'codex',calls:4,tokenReports:2,tokens:810,reportedCostMicrousd:0,reportedCostCalls:1,estimatedCostMicrousd:210000,estimatedCostCalls:2}]})};
  assert.equal(p.calls.filter(c => Object.hasOwn(p.host.tools,c.name)).length,0);
  await p.event('tab-git','click');
  assert.match(p.get('git-content').textContent,/연결된 워크트리.*로컬 HEAD 불일치/);
  assert.match(p.get('git-content').textContent,/통과 1 · 실패 0 · 대기 2/);
  await p.event('tab-usage','click');
  assert.match(p.get('usage-content').textContent,/0% 사용/);
  assert.match(p.get('usage-content').textContent,/토큰 미확인/);
  assert.match(p.get('usage-content').textContent,/토큰 소계 810 · 사용량 보고 2\/4회/);
  assert.match(p.get('usage-content').textContent,/보고 비용 US\$0 · 1회/);
  assert.match(p.get('usage-content').textContent,/추정 비용 US\$0.21 · 2회/);
  await p.event('tab-git','click');
  assert.equal(p.calls.filter(c => c.name === 'oms_panel_git_status').length,1);
});

test('TUI inspection is on demand, pages in both directions and discards a superseded view', async () => {
  const p = await panel(['one'], {commandStatus:{available:true,total:9,active:2,executing:1}});
  p.host.tools = {oms_panel_tui_view:({view,page}) => ({view,page,status:'ready',columns:[{key:'task',label:'작업'}],
    rows:[{task:view+'-'+page}],nextPage:page===1 ? 2 : null})};
  await p.event('tab-activity','click'); assert.ok(!p.calls.some(c => c.name==='oms_panel_tui_view'));
  p.get('tui-view').value='plan'; await p.event('tui-view','change'); await p.event('tui-next','click');
  assert.match(p.get('tui-rows').textContent,/plan-2/);
  await p.event('tui-prev','click'); assert.match(p.get('tui-rows').textContent,/plan-1/);
  p.host.delayTool='oms_panel_tui_view'; p.get('tui-view').value='health'; await p.event('tui-view','change');
  p.get('tui-view').value='jobs'; await p.event('tui-view','change'); p.host.release(); await setImmediate();
  assert.match(p.get('tui-rows').textContent,/jobs-1/);
  assert.doesNotMatch(p.get('tui-rows').textContent,/health/);
});

test('private attempt detail requires the selected actor, attempt and original source on every page', async () => {
  const p = await panel(['one'],{conversationMain:{source:'room-session',state:'joined',provider:'codex',room:'r',participant:'owner'}});
  let patch = 'initial', returnedId;
  p.host.tools = {oms_panel_chat:a => ({room:a.room,participant:a.participant,source:'room',messages:[],
    attempts:[{id:'a1',tool:'peer-review',state:'done',sections:['info','events']}],
    evidence:a.section && {attempt:returnedId || a.attempt,section:a.section,page:a.page,source:'indexed-patch',
      lines:[a.section+' '+a.page],proofs:{diff:'initial'},digest:a.section === 'diff' ? patch : null,
      nextPage:a.section === 'diff' && a.page===1 ? 2 : null}})};
  await p.event('header-main','click');
  assert.equal(p.calls.at(-1).args.section,undefined);
  p.get('evidence-attempt').value='a1'; await p.event('evidence-attempt','change');
  assert.equal(p.get('evidence-lines').textContent,'info 1');
  const click = async label => {
    p.get('evidence-actions').children.find(button => button.textContent===label).dispatchEvent(new Event('click'));
    await setImmediate();
  };
  assert.doesNotMatch(p.get('evidence-actions').textContent,/변경 파일|기록된 답변/);
  await click('패치');
  assert.equal(p.get('evidence-lines').textContent,'diff 1');
  patch='new';
  await p.event('evidence-next','click');
  assert.equal(p.get('evidence-lines').textContent,'');
  assert.match(p.get('evidence-status').textContent,/근거가 바뀌었습니다/);
  assert.deepEqual(Object.entries(p.calls.at(-1).args).sort(),Object.entries({project:'one',room:'r',participant:'owner',attempt:'a1',section:'diff',page:2}).sort());
  returnedId='other'; await click('이벤트');
  assert.match(p.get('evidence-status').textContent,/일치하지 않습니다/);
  assert.equal(p.get('evidence-lines').textContent,'');
});

test('TUI partial totals and failed next pages preserve the last observed rows and navigation', async () => {
  const p = await panel(['one']);
  let state='unavailable';
  p.host.tools = {oms_panel_tui_view:({view,page}) => ({view,page,status:page===1?'ready':state,
    columns:[{key:'item',label:'항목'}],rows:page===1?[{item:'retained row'}]:[],nextPage:page===1?2:null,
    total:page===1?1:undefined,incomplete:true})};
  await p.event('tab-activity','click');
  p.get('tui-view').value='log'; await p.event('tui-view','change');
  assert.match(p.get('tui-status').textContent,/확인된 합계 1건.*일부 조회/);
  await p.event('tui-next','click');
  assert.match(p.get('tui-status').textContent,/조회할 수 없습니다/);
  assert.match(p.get('tui-rows').textContent,/retained row/);
  assert.equal(p.get('tui-next').hidden,false);
  state='ready'; await p.event('tui-next','click');
  assert.match(p.get('tui-status').textContent,/이 페이지에서 확인된 기록 없음 · 일부 조회/);
  assert.doesNotMatch(p.get('tui-status').textContent,/전체 0|합계 0/);
});

test('a compact snapshot reports only positive omissions and keeps empty samples uncertain', async () => {
  const p = await panel(['one'], {attemptsTrimmed:2,operationsTrimmed:1,attentionTrimmed:0,activeAttemptsTrimmed:3,
    active:0,activeAttempts:[],attempts:[],reviews:{available:true},
    conversationMain:{source:'room-session',state:'joined',provider:'codex',room:'r',participant:'owner'}});
  assert.match(p.get('status').textContent,/요약에서 생략된 기록 · 최근 실행 2건, 결과물 1건, 미종료 실행 3건/);
  assert.doesNotMatch(p.get('status').textContent,/주의 항목/);
  assert.match(p.get('leads').textContent,/확인하지 못했습니다/);
  assert.match(p.get('operations').textContent,/결과물 미확인/);
  assert.match(p.get('activity').textContent,/실행 기록 미확인/);
  const full = await panel(['one'], {attemptsTrimmed:0});
  assert.doesNotMatch(full.get('status').textContent,/생략/);
});

test('overview opens only bound work conversations, labels confirmed Main, and routes measured progress to the plan', async () => {
  const member={room:'r',participant:'native-lead',provider:'claude',role:'worker'};
  const p=await panel(['one'],{conversationBinding:'matched',
    conversationMain:{source:'room-session',state:'joined',provider:'codex',room:'r',participant:'owner',participants:[member]},
    activeAttempts:[{...member,task:'Test changes',state:'verifying'},{...member,room:'another',task:'Wrong room'}],
    attempts:[{...member,participant:'unbound',task:'Unrelated'}],tasksVerified:0,tasks:3,acceptance:{available:true,total:2,verified:1}});
  p.host.tools={oms_panel_chat:args=>({...args,source:'room',messages:[]}),oms_panel_tui_view:args=>({...args,status:'ready',columns:[],rows:[]})};
  assert.match(p.get('header-main').textContent,/Codex Main$/); assert.equal(p.get('header-main').hidden,false);
  assert.equal(p.get('plan-ratio').textContent,'0 / 3'); assert.equal(p.get('acceptance-bar').value,1);
  const threads=p.get('thread-rows'); assert.equal(threads.children.length,1); assert.match(threads.textContent,/Test changes.*검증 중/);
  threads.children[0].dispatchEvent(new Event('click')); await setImmediate();
  assert.equal(p.calls.at(-1).args.participant,'native-lead'); assert.equal(p.calls.at(-1).args.room,'r');
  await p.event('chat-back','click'); await p.event('header-main','click'); assert.equal(p.calls.at(-1).args.participant,'owner');
  await p.event('chat-back','click'); await p.event('plan-open','click');
  assert.equal(p.get('pane-activity').hidden,false); assert.equal(p.calls.at(-1).args.view,'plan');
  assert.equal(p.get('approval-card').hidden,true);
  const unknown = await panel(['one'],{tasks:4,tasksVerified:null,acceptance:{available:true,total:0,verified:0}});
  assert.equal(unknown.get('header-main').hidden,true); assert.equal(unknown.get('plan-strip').hidden,true);
  assert.equal(unknown.get('acceptance-bar').hidden,true); assert.equal(unknown.get('work-threads').hidden,true);
});

const openItem = {kind:'issue',number:9,title:'Review input boundaries',state:'OPEN',url:'https://github.com/org/repo/issues/9'};
const workData = {git:{available:true,branch:'main'},github:{repository:'org/repo',status:'unavailable',work:{
  issues:{status:'available',incomplete:true,items:[openItem]},pullRequests:{status:'available',items:[]},
  projects:{status:'available',items:[{number:2,title:'Release',state:'OPEN',items:[openItem]}]}}}};
const matchedMain={source:'room-session',state:'joined',provider:'codex',room:'r',participant:'owner'};
async function pickIssue(p) {
  p.host.tools={oms_panel_git_status:()=>workData};
  await p.event('tab-git','click');
  p.get('git-kind').value='issues'; await p.event('git-kind','change');
  const card=p.get('git-catalog').children[0];
  card.children.at(-1).dispatchEvent(new Event('click')); await setImmediate();
}

test('Git issue is an editable Lead request, and only an explicit send posts it to a matched Main', async () => {
  const p=await panel(['one'],{conversationMain:matchedMain},{message:true});
  await pickIssue(p);
  assert.match(p.get('git-catalog-status').textContent,/일부 조회/);
  p.get('git-provider').value='codex'; await p.event('git-provider','change');
  assert.match(p.get('git-goal').value,/Codex Lead.*org\/repo\/issues\/9/);
  p.get('git-goal').value+=' 변경 범위를 정리해 주세요.'; await p.event('git-goal','input');
  assert.equal(p.host.messages.length,0); assert.equal(p.calls.some(c=>c.name==='oms_start_main'),false);
  await p.event('git-send','click'); await p.event('git-send','click');
  assert.equal(p.host.messages.length,1); assert.equal(p.host.messages[0].content[0].text,p.get('git-goal').value);
  assert.match(p.get('git-editor-status').textContent,/배정 요청을 전달/);
  await p.event('git-new','click'); assert.equal(p.get('brief').value,p.get('git-goal').value);
  assert.ok(p.get('launcher').open); assert.equal(p.calls.some(c=>c.name==='oms_start_main'),false);
});

test('generic host initializes read-only, opens project by result, and shares only by user action', async () => {
  const p=await panel(['one','two'],{conversationMain:matchedMain},{host:'Claude',message:true,readOnly:true});
  assert.ok(p.host.notices.includes('ui/notifications/initialized')); assert.equal(p.host.initialize.appInfo.version,'0.4.1');
  assert.equal(p.get('view').hidden,true);
  await p.open('result',{structuredContent:{panel:'OMS',project:'two',readOnly:true}});
  assert.equal(p.get('project').textContent,'two'); assert.equal(p.get('launcher').hidden,true);
  assert.equal(p.get('header-main').hidden,true); await pickIssue(p);
  assert.equal(p.get('git-new').hidden,true); assert.equal(p.get('git-send').textContent,'현재 대화에 공유');
  assert.equal(p.host.messages.length,0); await p.event('git-send','click'); assert.equal(p.host.messages.length,1);
  assert.match(p.get('git-editor-status').textContent,/공유했습니다/);
  await p.event('git-new','click'); await p.event('launch','submit');
  assert.equal(p.calls.some(c=>['oms_start_main','oms_start_status','oms_list_approvals','oms_panel_chat','oms_decide_approval'].includes(c.name)),false);
});

test('number lookup rejects mismatched resources, supports verified older items, and board rows stay project scoped', async () => {
  const p=await panel(['one'],{conversationMain:matchedMain}); await pickIssue(p);
  assert.equal(p.get('git-send').hidden,true); await p.event('git-copy','click'); assert.equal(p.get('git-goal').selected,true);
  let item={...openItem,number:2,url:'https://github.com/other/repo/issues/2'};
  p.host.tools.oms_panel_git_status=()=>({github:{repository:'org/repo',lookup:{status:'available',item}}});
  p.get('git-number').value='2'; await p.event('git-find','submit');
  assert.match(p.get('git-match').textContent,/확인할 수 없습니다/);
  item={...item,url:'https://github.com/org/repo/issues/2'};
  await p.event('git-find','submit'); assert.equal(p.calls.at(-1).args.kind,'issue'); assert.equal(p.calls.at(-1).args.number,2);
  p.get('git-match').children.at(-1).dispatchEvent(new Event('click')); await setImmediate();
  assert.match(p.get('git-goal').value,/org\/repo\/issues\/2/);
  p.get('git-kind').value='projects'; await p.event('git-kind','change');
  assert.match(p.get('git-catalog').textContent,/Release.*Review input boundaries.*보드 식별 정보 미확인/);
});

test('provider availability and active Codex Lead are independent from Codex Main identity', async () => {
  const p=await panel(['one'],{conversationMain:matchedMain,activeAttempts:[
    {provider:'codex',role:'lead',room:'r',task:'Review',participant:'other',state:'working'},
    {provider:'claude',role:'lead',room:'other',task:'Elsewhere',state:'working'}]});
  p.host.tools={oms_panel_tui_view:()=>({view:'providers',page:1,status:'ready',rows:[
    {provider:'codex',installed:true,usable:true},{provider:'claude',installed:true},{provider:'antigravity',usable:false}]})};
  assert.match(p.get('header-main').textContent,/Codex Main$/); assert.match(p.get('leads').textContent,/Codex LeadReview작업 중/);
  assert.doesNotMatch(p.get('leads').textContent,/Elsewhere/); await p.event('provider-load','click');
  assert.match(p.get('provider-rows').textContent,/Codex Lead호스트 사용 가능.*Claude Lead설치됨 · 사용 가능 여부 미확인.*AGY Lead호스트 사용 불가/);
});

test('partial lists allow a scoped whole-backlog brief and stale items cannot be sent or launched as fresh', async () => {
  const p=await panel(['one'],{conversationMain:matchedMain},{message:true});
  await pickIssue(p); await p.event('git-backlog','click');
  assert.match(p.get('git-goal').value,/https:\/\/github.com\/org\/repo 저장소.*모든 열린 이슈와 PR.*마지막 페이지.*기존 PR.*의존성.*검증 근거/);
  assert.doesNotMatch(p.get('git-goal').value,/Review input boundaries/);
  assert.equal(p.host.messages.length,0); assert.equal(p.calls.some(c=>c.name==='oms_start_main'),false);
  await p.event('git-send','click'); assert.equal(p.host.messages.length,1);
  await pickIssue(p);
  p.host.tools.oms_panel_git_status=()=>({...workData,github:{...workData.github,work:{issues:{status:'available',items:[]}}}});
  await p.event('refresh','click');
  assert.equal(p.get('git-new').disabled,true); assert.equal(p.get('git-send').disabled,true);
  await p.event('git-new','click'); await p.event('git-send','click');
  assert.equal(p.host.messages.length,1); assert.equal(p.get('brief').value,'');
});

test('first-fold whole-backlog action discovers the repository on demand and stays in the draft', async () => {
  const p=await panel(['one'],{conversationMain:matchedMain},{message:true});
  p.host.tools={oms_panel_git_status:()=>workData};
  assert.ok(!p.calls.some(c=>c.name==='oms_panel_git_status'));
  await p.event('overview-backlog','click');
  assert.equal(p.get('pane-git').hidden,false); assert.equal(p.get('git-editor').hidden,false);
  assert.match(p.get('git-goal').value,/org\/repo.*oms_panel_git_status.*nextCursor/);
  assert.equal(p.host.messages.length,0); assert.equal(p.calls.some(c=>c.name==='oms_start_main'),false);
});

test('GitHub Projects permission copy requires a typed scope error; other repository collections remain available', async () => {
  const p=await panel(['one']); let permission;
  p.host.tools={oms_panel_git_status:()=>({...workData,github:{...workData.github,work:{...workData.github.work,
    projects:{status:'unavailable',reason:permission,items:[]}}}})};
  await p.event('tab-git','click'); p.get('git-kind').value='projects'; await p.event('git-kind','change');
  assert.doesNotMatch(p.get('git-catalog-status').textContent,/read:project/);
  permission='insufficient_scope'; await p.event('refresh','click');
  assert.match(p.get('git-catalog-status').textContent,/read:project/);
  p.get('git-kind').value='issues'; await p.event('git-kind','change');
  assert.match(p.get('git-catalog').textContent,/Review input boundaries/);
});

test('connection settings separate host facts, runtime policy, and draft-only harness choice', async () => {
  const p = await panel(['one'], {}, {host:'Claude Desktop',version:'2.4.1',mode:'inline',readOnly:true});
  const columns = ['harness','installed','auth','transport','resumePolicy'].map(key => ({key,label:key}));
  const data = {view:'connections',page:1,columns,status:'ready',rows:[
    {harness:'pi',installed:false,auth:null,transport:'CodexGateway',resumePolicy:'unsupported'},
    {harness:'claude',installed:true,auth:'required',dependenciesPresent:true}
  ]};
  p.host.tools = {oms_panel_tui_view:() => data};
  assert.equal(p.calls.some(c => c.name === 'oms_panel_tui_view'), false);
  p.get('source-details').open = true; p.get('connection-harness').value = 'pi';
  await p.event('source-details','toggle');
  assert.match(p.get('connection-host').textContent,/Claude Desktop 2.4.1.*inline/);
  assert.match(p.get('connection-runtime').textContent,/OMS 연결됨.*읽기 전용/);
  assert.match(p.get('connection-rows').textContent,/미설치.*미확인.*CodexGateway.*미지원/);
  assert.equal(p.get('connection-draft').hidden,true);
  p.get('connection-harness').value = 'claude'; await p.event('connection-harness','change');
  assert.match(p.get('connection-rows').textContent,/인증 필요/);
  p.notify({method:'ui/notifications/host-context-changed',params:{displayMode:'fullscreen'}});
  assert.match(p.get('connection-host').textContent,/fullscreen/);
  await p.event('connection-draft','click');
  assert.equal(p.get('launch').elements.main.value,'codex');
  const q = await panel(['one'], {}, {host:'private-host-sentinel',version:'unknown-private-version'});
  q.host.tools = {oms_panel_tui_view:() => data};
  q.get('source-details').open = true; q.get('connection-harness').value = 'claude';
  await q.event('source-details','toggle');
  assert.doesNotMatch(q.get('connection-host').textContent,/private|Claude/);
  await q.event('connection-draft','click');
  assert.equal(q.get('launch').elements.main.value,'claude'); assert.equal(q.get('launcher').open,true);
  assert.equal([...p.calls,...q.calls].some(c => c.name === 'oms_start_main'),false);
});

test('settings page, permission failure and project change discard prior catalog facts', async () => {
  const p = await panel(['one']);
  p.host.tools = {oms_panel_tui_view:({view,page}) => ({view,page,status:'ready',columns:[{key:'model',label:'모델'}],
    rows:[{model:'catalog-page-' + page}],nextPage:page === 1 ? 2 : null,incomplete:true})};
  p.get('source-details').open = true; p.get('connection-view').value = 'modelcatalog'; await p.event('source-details','toggle');
  assert.match(p.get('connection-rows').textContent,/catalog-page-1/);
  await p.event('connection-next','click');
  assert.match(p.get('connection-rows').textContent,/catalog-page-2/); assert.match(p.get('connection-status').textContent,/일부 조회/);
  p.host.delayTool = 'oms_panel_tui_view'; await p.event('connection-refresh','click');
  p.host.tools.oms_panel_snapshot = () => { throw new Error('access revoked'); };
  await p.event('refresh','click'); p.host.release(); await setImmediate();
  assert.equal(p.get('connection-rows').textContent,'');
  assert.match(p.get('connection-runtime').textContent,/조회 실패/);
  assert.equal(p.get('view').hidden,true);
  assert.doesNotMatch(p.get('connection-runtime').textContent,/연결됨/);
  await p.open('input',{arguments:{project:'absent'}});
  assert.equal(p.get('connection-rows').textContent,'');
  assert.equal(p.calls.filter(c => c.name === 'oms_start_main').length,0);
});


test('Project pages use the linked board ID, expose field hints and only offer a draft for eligible issues',async () => {
  const p=await panel(['one'],{conversationMain:matchedMain},{message:true});
  const board={id:'PVT_scope',number:2,url:'https://github.com/orgs/org/projects/2',title:'Release'};
  const calls=[];
  p.host.tools={oms_panel_git_status:args=>{
    if (!args.boardId) return {...workData,github:{...workData.github,work:{projects:{status:'available',items:[board]}}}};
    calls.push(args);
    return {github:{repository:'org/repo',board:{board:{...board,fields:{missing:['Phase']}},status:'available',nextCursor:args.cursor?null:'board.next',
      items:[{...openItem,class:args.cursor?'blocked':'candidate',fields:{Status:{name:'Ready'}}}]}}};
  }};
  await p.event('tab-git','click');p.get('git-kind').value='projects';await p.event('git-kind','change');
  const box=p.get('git-catalog').children.at(-1).children.at(-1);
  const [next,back,note,rows]=box.children.filter(x=>typeof x==='object');
  assert.equal(calls.length,0);next.dispatchEvent(new Event('click'));await setImmediate();
  assert.deepEqual({...calls[0]},{boardId:'PVT_scope',project:'one'});
  assert.match(rows.textContent,/검토 후보.*Status Ready.*작업 목표 작성/);
  assert.match(note.textContent,/Phase/);
  next.dispatchEvent(new Event('click'));await setImmediate();
  assert.equal(calls[1].cursor,'board.next');assert.match(rows.textContent,/선행 이슈 진행 중/);
  assert.doesNotMatch(rows.textContent,/작업 목표 작성/);assert.equal(next.hidden,true);assert.equal(back.hidden,false);
  assert.equal(p.host.messages.length,0);assert.ok(!p.calls.some(c=>c.name==='oms_start_main'));
});

test('overview represents each scoped Lead assignment once and opens its exact member; other members remain in chat',async()=>{
  const lead={room:'room',participant:'search-lead',provider:'codex',role:'worker'};
  const run={...lead,role:'lead',task:'수정 작업',state:'verifying'};
  const advisor={room:'room',participant:'qa',provider:'claude',role:'reviewer'};
  const p=await panel(['one'],{conversationBinding:'matched',active:1,
    conversationMain:{source:'room-session',state:'joined',provider:'codex',room:'room',participant:'owner',participants:[lead,advisor]},
    activeAttempts:[run],attempts:[run,{...advisor,task:'검토 기록',state:'done'}]});
  p.host.tools={oms_panel_chat:args=>({...args,source:'room',messages:[]})};
  assert.match(p.get('leads').textContent,/Codex Lead수정 작업검증 중/);
  assert.doesNotMatch(p.get('thread-rows').textContent,/수정 작업/);
  assert.match(p.get('thread-rows').textContent,/검토 기록/);
  p.get('leads').children[0].dispatchEvent(new Event('click'));await setImmediate();
  assert.equal(p.calls.at(-1).args.participant,'search-lead');
  assert.equal(p.calls.at(-1).args.room,'room');
  assert.match(p.get('chat-members').textContent,/qa/);
});

test('connection details surface bounded non-atomic observation revision across refreshes and scopes without cluttering overview', async () => {
  assert.match(html, /<noscript>[\s\S]*oms_panel_snapshot[\s\S]*oms dashboard[\s\S]*<\/noscript>/);
  const revOne = 'obs-v1:0123456789abcdef01234567';
  const revTwo = 'obs-v1:fedcba9876543210fedcba98';
  let snapOne = {
    project:'one', updated:'2026-11-24T10:00:00Z',
    observation:{schema:1, kind:'multi-source-observation', atomic:false, revision:revOne,
      observedAt:'2026-11-24T10:00:00Z', completeness:'complete',
      sources:{git:'ok', task:'ok', plan:'ok', attempts:'ok', artifacts:'ok', acceptance:'ok', conversation:'unconfirmed'},
      evidence:{scope:'repository', head:'1234567890ab', room:null, latestArtifact:'art-1', omitted:0, trimmed:0}}
  };
  let snapTwo = {
    project:'two', updated:'2026-11-24T10:02:00Z', degraded:true,
    observation:{schema:1, kind:'multi-source-observation', atomic:false, revision:revTwo,
      observedAt:'2026-11-24T10:02:00Z', completeness:'partial',
      sources:{git:'ok', task:'ok', plan:'ok', attempts:'ok', artifacts:'unavailable', acceptance:'ok', conversation:'unconfirmed'},
      evidence:{scope:'repository', head:'abcdef123456', room:null, latestArtifact:null, omitted:3, trimmed:1}}
  };
  const p = await panel(['one', 'two']);
  p.host.tools = {oms_panel_snapshot:({project:alias}) => ({dashboard: alias === 'two' ? snapTwo : snapOne})};
  p.get('project-choice').value = 'one';
  await p.event('project-choice', 'change');
  assert.equal(p.get('observation-revision').textContent, revOne);
  assert.match(p.get('observation-basis').textContent, /다중 원장 관측 \(비원자적\)/);
  assert.equal(p.get('observation-completeness').textContent, '전체 관측');
  assert.match(p.get('observation-sources').textContent, /HEAD 1234567890ab · 저장소 최근 결과물 art-1/);
  assert.doesNotMatch(p.get('goal').textContent, /obs-v1:/);
  const firstStamp = p.get('updated').textContent;
  // Unchanged read at a later wall-clock time keeps the same revision and updates observedAt.
  snapOne = {...snapOne, updated:'2026-11-24T10:15:00Z', observation:{...snapOne.observation, observedAt:'2026-11-24T10:15:00Z'}};
  await p.event('refresh', 'click');
  assert.equal(p.get('observation-revision').textContent, revOne);
  assert.notEqual(p.get('updated').textContent, firstStamp);
  // Switching to a foreign project replaces with the foreign partial observation without writes.
  p.get('project-choice').value = 'two';
  await p.event('project-choice', 'change');
  assert.equal(p.get('observation-revision').textContent, revTwo);
  assert.match(p.get('observation-completeness').textContent, /일부 관측 · 생략 3건 · 축약 1건/);
  assert.match(p.get('observation-sources').textContent, /확인 필요 · 결과물 \(미확인\)/);
  assert.equal(p.calls.some(c => ['oms_start_main','oms_decide_approval'].includes(c.name)), false);
  // Unknown or malformed revisions stay fail-closed as 미확인.
  snapTwo = {...snapTwo, observation:{schema:1, kind:'multi-source-observation', atomic:false, revision:'/etc/passwd', completeness:'unknown'}};
  await p.event('refresh', 'click');
  assert.equal(p.get('observation-revision').textContent, '미확인');
  assert.equal(p.get('observation-completeness').textContent, '관측 상태 미확인');
});

// Localization: Korean (default) and English, chosen by the user or negotiated.
const LANG_KEY = 'oms.panel.language';
const memoryStorage = initial => ({data:new Map(initial ? [[LANG_KEY, initial]] : []), sets:0,
  getItem(key) { return this.data.get(key) ?? null; }, setItem(key, value) { this.sets++; this.data.set(key, value); }});
const deniedStorage = () => ({getItem() { throw new Error('denied'); }, setItem() { throw new Error('denied'); }});
async function choose(p, value) {
  p.get('language-choice').value = value;
  await p.event('language-choice', 'change');
}
const focusCount = p => [...p.elements.values()].reduce((sum, element) => sum + (element.focused ?? 0), 0);

test('Korean and English catalogs match, and every static node and message key resolves', async () => {
  const p = await panel(['one']);
  const ko = p.run('MSG.ko'), en = p.run('MSG.en');
  assert.deepEqual(Object.keys(en).sort(), Object.keys(ko).sort());
  for (const key of Object.keys(ko)) {
    const holders = [...ko[key].matchAll(/\{(\w+)\}/g)].map(m => m[1]).sort(), english = [...en[key].matchAll(/\{(\w+)\}/g)].map(m => m[1]).sort();
    assert.deepEqual(english, holders, key);
  }
  for (const node of p.staticNodes) {
    const key = node.attributes.get('data-i18n');
    if (key) { assert.ok(Object.hasOwn(ko, key), key); assert.equal(ko[key], node.initialText, key); }
    for (const pair of (node.attributes.get('data-i18n-attr') ?? '').split(';').filter(Boolean)) {
      const [name, attrKey] = pair.split(':');
      assert.equal(ko[attrKey], node.attributes.get(name), attrKey);
    }
  }
  const prefixes = new Set(Object.keys(ko).filter(key => key.includes('.')).map(key => key.split('.')[0]));
  const literals = [...script.matchAll(/'([A-Za-z]+(?:\.[A-Za-z_]+)+)'/g)].map(m => m[1]).filter(key => prefixes.has(key.split('.')[0]));
  assert.ok(literals.length > 40);
  for (const key of literals) assert.ok(Object.hasOwn(ko, key), key);
  for (const key of [...script.matchAll(/\bt\(\s*'([^']+)'/g)].map(m => m[1]).filter(key => !key.endsWith('.'))) assert.ok(Object.hasOwn(ko, key), key);
  const families = {state:['working','starting','verifying','review','in_review','done','blocked','waiting_for_input'],
    'git.class':['candidate','blocked','unverified','plan_linked','board_in_progress','closed_board','duplicate','archived','unknown','draft','foreign','epic','closed','pull_request'],
    'git.check':['failed','pending','passed','other','unknown'], ev:['info','events','files','diff','transcript'],
    src:['git','task','plan','attempts','artifacts','acceptance','conversation'], srcState:['unavailable','partial','ambiguous','stale'],
    sv:['connections','modelcatalog','routes','attribution'], obs:['complete','partial','unknown']};
  for (const [family, members] of Object.entries(families)) for (const member of members) assert.ok(Object.hasOwn(ko, family + '.' + member), family + '.' + member);
  assert.ok(Buffer.byteLength(html) < 192 * 1024);
});

for (const [name, options, expected] of [
  ['Korean navigator', {navigator:{languages:['ko-KR','en']}}, 'ko'],
  ['English navigator', {navigator:{languages:['en-GB']}}, 'en'],
  ['first supported navigator language', {navigator:{languages:['ja','en-US']}}, 'en'],
  ['unsupported navigator falls back to Korean', {navigator:{languages:['ja']}}, 'ko'],
  ['single navigator.language', {navigator:{language:'en'}}, 'en'],
  ['no navigator', {}, 'ko'],
  ['host locale over navigator', {navigator:{languages:['ko']}, locale:'en-US'}, 'en'],
  ['unsupported host locale falls to navigator', {navigator:{languages:['en']}, locale:'ja-JP'}, 'en'],
  ['stored choice over host and navigator', {navigator:{languages:['en']}, locale:'en', storage:memoryStorage('ko')}, 'ko'],
  ['invalid stored value is ignored', {navigator:{languages:['en']}, storage:memoryStorage('fr')}, 'en'],
  ['storage denied falls back to navigator', {navigator:{languages:['en']}, storage:deniedStorage()}, 'en'],
]) {
  test('language negotiation: ' + name, async () => {
    const p = await panel(['one'], {goal:'목표 goal'}, options);
    assert.equal(p.run('lang'), expected);
    assert.equal(p.get('language-choice').value, expected);
    assert.equal(p.get('tab-chat').textContent, expected === 'en' ? 'Chat' : '대화');
    assert.equal(p.get('goal').textContent, '목표 goal');
    assert.equal(p.get('status').textContent, '');
    assert.equal(p.get('current-scope').textContent, expected === 'en' ? 'Current Main' : '현재 Main');
  });
}

test('changing language is instant: no refetch, no reset, draft/focus/side effects untouched, user content intact', async () => {
  const storage = memoryStorage();
  const lead = {provider:'claude', role:'lead', task:'작업 task', room:'r', state:'working', participant:'lead-1'};
  const p = await panel(['one','two'], {goal:'Ship 한국어 / English', conversationMain:{...matchedMain, participants:[{room:'r', participant:'lead-1', provider:'claude'}]},
    activeAttempts:[lead], active:1, attempts:[], degraded:true, acceptance:{available:true, verified:1, total:3, complete:false}}, {message:true, storage});
  p.get('project-choice').value = 'one'; await p.event('project-choice', 'change');
  const row = {id:'ap1', version:1, digest:'d1', summary:'승인 summary 그대로', action:'Main scope decision', object:'room-one', sender:'codex'};
  p.host.tools = {oms_list_approvals:() => ({approvals:[row]}),
    oms_panel_git_status:() => ({git:{available:true, branch:'main', dirty:false}, github:{status:'available', repository:'org/repo',
      pullRequests:[{number:7, title:'PR 제목', headBranch:'topic', baseBranch:'main', scope:'linkedWorktree', matchesLocalHead:false, draft:true, checks:{passed:1, failed:0, pending:2, other:0}}]}}),
    oms_panel_usage:() => ({providers:[{provider:'codex', windows:[{used:0, name:'주간'}]}], attempts:[{id:'a1', provider:'codex', tokens:null, source:null}], providerTotals:[]})};
  await p.event('refresh', 'click');
  await p.event('tab-git', 'click'); await p.event('tab-usage', 'click'); await p.event('tab-overview', 'click');
  p.get('brief').value = '내 초안 draft';
  assert.equal(p.get('approvals-heading').textContent, '승인 요청 1건');
  assert.match(p.get('leads').textContent, /Claude Lead작업 task작업 중/);
  assert.match(p.get('git-content').textContent, /연결된 워크트리 · 로컬 HEAD 불일치/);

  const calls = p.calls.length, messages = p.host.messages.length, focused = focusCount(p), projectName = p.get('project').textContent;
  await choose(p, 'en');
  assert.equal(p.calls.length, calls);
  assert.equal(p.host.messages.length, messages);
  assert.equal(focusCount(p), focused);
  assert.equal(p.run('lang'), 'en');
  assert.equal(storage.data.get(LANG_KEY), 'en');
  assert.equal(p.get('project').textContent, projectName);
  assert.equal(p.get('brief').value, '내 초안 draft');
  assert.equal(p.get('view').hidden, false);
  // UI-owned text switches...
  assert.equal(p.get('approvals-heading').textContent, 'Approval requests (1)');
  assert.equal(p.get('tab-overview').textContent, 'Overview');
  assert.equal(p.get('goal-heading').textContent, 'Project goal');
  assert.equal(p.get('status').textContent, 'Some status could not be collected.');
  assert.match(p.get('leads').textContent, /Claude Lead.*Working/);
  assert.match(p.get('criteria').textContent, /Completion criteria verified · 1 \/ 3 · Incomplete/);
  assert.match(p.get('git-content').textContent, /Linked worktree · Differs from local HEAD/);
  assert.match(p.get('git-content').textContent, /Checks · passed 1 · failed 0 · pending 2 · other 0/);
  assert.match(p.get('usage-content').textContent, /0% used/);
  assert.match(p.get('usage-content').textContent, /Tokens Unknown/);
  assert.equal(p.document.title, 'OMS · Project control');
  // ...while user and backend text is untouched.
  assert.equal(p.get('goal').textContent, 'Ship 한국어 / English');
  assert.match(p.get('leads').textContent, /작업 task/);
  assert.match(p.get('approvals').textContent, /승인 summary 그대로/);
  assert.match(p.get('git-content').textContent, /PR 제목/);
  assert.match(p.get('usage-content').textContent, /주간/);
  // Approval buttons are labelled but never fired by a language change.
  assert.match(p.get('approvals').textContent, /Approve this requestDeny this request/);
  assert.equal(p.calls.some(c => ['oms_decide_approval', 'oms_start_main'].includes(c.name)), false);

  await choose(p, 'ko');
  assert.equal(p.calls.length, calls);
  assert.equal(p.get('approvals-heading').textContent, '승인 요청 1건');
  assert.equal(p.get('status').textContent, '일부 상태를 수집하지 못했습니다.');
  assert.match(p.get('git-content').textContent, /연결된 워크트리 · 로컬 HEAD 불일치/);
  assert.equal(p.get('goal').textContent, 'Ship 한국어 / English');
  assert.equal(p.get('brief').value, '내 초안 draft');
});

test('language choice survives reopen, tolerates denied or absent storage, and host changes yield to it', async () => {
  const storage = memoryStorage();
  const first = await panel(['one'], {}, {navigator:{languages:['ko']}, storage});
  await choose(first, 'en');
  assert.equal(storage.sets, 1);
  const reopened = await panel(['one'], {}, {navigator:{languages:['ko']}, storage});
  assert.equal(reopened.run('lang'), 'en');
  assert.equal(reopened.get('language-choice').value, 'en');
  assert.equal(reopened.get('refresh').textContent, 'Refresh');
  assert.equal(storage.sets, 1);

  const denied = await panel(['one'], {}, {navigator:{languages:['ko']}, storage:deniedStorage()});
  await choose(denied, 'en');
  assert.equal(denied.get('tab-chat').textContent, 'Chat');
  await choose(denied, 'ko');
  assert.equal(denied.get('tab-chat').textContent, '대화');

  // Without an explicit choice the host follows its own locale; with one it does not.
  const auto = await panel(['one'], {}, {navigator:{languages:['ko']}});
  auto.notify({method:'ui/notifications/host-context-changed', params:{locale:'en-US'}});
  assert.equal(auto.run('lang'), 'en');
  await choose(auto, 'ko');
  auto.notify({method:'ui/notifications/host-context-changed', params:{locale:'en-US'}});
  assert.equal(auto.run('lang'), 'ko');
  assert.equal(auto.get('language-choice').value, 'ko');
});

test('localized errors follow the language while host and backend text stays verbatim', async () => {
  const p = await panel(['one'], {}, {navigator:{languages:['ko']}});
  p.host.tools = {oms_panel_usage:() => ({}), oms_panel_git_status:() => { throw new Error('rate limited 원문'); }};
  await p.event('tab-usage', 'click');
  assert.equal(p.get('usage-status').textContent, '사용량 응답을 확인할 수 없습니다.');
  await p.event('tab-git', 'click');
  assert.equal(p.get('git-status').textContent, 'rate limited 원문');
  await choose(p, 'en');
  assert.equal(p.get('usage-status').textContent, 'Cannot read the usage response.');
  assert.equal(p.get('git-status').textContent, 'rate limited 원문');
  const connected = await panel(['one'], {}, {navigator:{languages:['en']}});
  assert.equal(connected.get('connection-runtime').textContent, 'OMS connected · project status loaded · Last checked ' + connected.run('checkedAt'));
  assert.match(connected.get('connection-host').textContent, /^HTML rendered · /);
});

test('a Git draft keeps its typed text across languages and a later provider change swaps the prefix once', async () => {
  const p = await panel(['one'], {conversationMain:matchedMain}, {message:true, navigator:{languages:['ko']}});
  await pickIssue(p);
  p.get('git-provider').value = 'codex'; await p.event('git-provider', 'change');
  const draft = p.get('git-goal').value + ' 직접 쓴 문장';
  p.get('git-goal').value = draft; await p.event('git-goal', 'input');
  assert.match(draft, /^Codex Lead에게 다음 작업을 배정해 주세요\. /);
  assert.equal(p.get('git-item').textContent, '이슈 #9 · Review input boundaries');
  const calls = p.calls.length;
  await choose(p, 'en');
  assert.equal(p.get('git-goal').value, draft);
  assert.equal(p.get('git-item').textContent, 'Issue #9 · Review input boundaries');
  assert.equal(p.get('git-send').textContent, 'Send to current Main conversation');
  assert.equal(p.calls.length, calls);
  assert.equal(p.host.messages.length, 0);
  p.get('git-provider').value = 'claude'; await p.event('git-provider', 'change');
  assert.match(p.get('git-goal').value, /^Please assign the following work to the Claude Lead\. .*직접 쓴 문장$/);
  assert.equal((p.get('git-goal').value.match(/Lead/g) || []).length, 1);
  p.get('git-provider').value = 'codex'; await choose(p, 'ko'); await p.event('git-provider', 'change');
  assert.match(p.get('git-goal').value, /^Codex Lead에게 다음 작업을 배정해 주세요\. .*직접 쓴 문장$/);
});

test('read-only peer is explicitly requested once, recovered by ID and opened in the exact participant chat',async()=>{
  const main = {source:'room-session',state:'joined',provider:'codex',room:'work',participant:'main',incomplete:false,participants:[]};
  const p = await panel(['one'],{conversationBinding:'matched',conversationMain:main});
  assert.equal(p.get('peer-assignment').hidden,false);
  const starts = () => p.calls.filter(c => c.name === 'oms_assign_provider');
  assert.equal(starts().length,0);
  p.host.tools = {oms_assign_provider:()=>{throw new Error('연결 끊김');}};
  p.get('peer-provider').value='agy'; p.get('peer-task').value='Review project compatibility';
  await p.event('peer-form','submit'); await p.event('peer-form','submit');
  assert.equal(starts().length,1);
  const req=starts()[0].args;
  assert.equal(req.project,'one'); assert.equal(req.room,'work'); assert.equal(req.provider,'agy');
  p.host.tools.oms_assign_result = a => ({...a,provider:'antigravity',role:'researcher',readOnly:true,
    participant:'ask-'+a.requestId,status:'started',operation:'ask-confirmed'});
  await p.event('peer-check','click');
  assert.deepEqual({...p.calls.at(-1).args},{room:'work',requestId:req.requestId,project:'one'});
  p.host.tools.oms_panel_chat = a=>({...a,source:'room',messages:[],result:{kind:'read-only provider',text:'Recorded answer'}});
  await p.event('peer-chat','click');
  assert.equal(p.calls.at(-1).args.participant,'ask-'+req.requestId);
  assert.match(p.get('chat-result').textContent,/Recorded answer/); assert.equal(starts().length,1);
});
test('foreign, ambiguous, partially observed, and generic views cannot submit a peer',async()=>{
  const main={source:'room-session',state:'joined',provider:'codex',room:'work',participant:'main',incomplete:false};
  for(const [binding,record,readOnly] of [['unconfirmed',null,false],['ambiguous',main,false],['matched',{...main,incomplete:true},false],['matched',main,true]]){
    const p=await panel(['one'],{conversationBinding:binding,conversationMain:record},{readOnly});
    assert.equal(p.get('peer-assignment').hidden,true);
    p.get('peer-provider').value='claude';p.get('peer-task').value='Inspect'; await p.event('peer-form','submit');
    assert.equal(p.calls.filter(c=>c.name==='oms_assign_provider').length,0);
  }
});

const reviewMember = {role:'researcher',provider:'claude',room:'review-room',participant:'ask-07510448-d1c8-4766-8e1a-ae4451c41dc0'};
const reviewAnswer = '검증한 결과: partially localized → <evidence> & raw text.';
async function reviewedPanel(options = {}, response = {}) {
  const main = {source:'room-session',state:'joined',provider:'codex',room:reviewMember.room,participant:'owner',incomplete:false,participants:[reviewMember]};
  const p = await panel(['one'],{conversationMain:main,conversationBinding:'matched'},options);
  p.host.tools = {oms_panel_chat:a=>({...a,source:'room',messages:[],provider:'claude',assignment:{state:'started'},result:{
    kind:'read-only provider',attempt:'ask-20261124T090000Z-123abcd0',text:reviewAnswer,truncated:true},...response})};
  await p.event('tab-chat','click');
  const buttons = p.get('chat-members').children;
  buttons.find(b => b.textContent.includes(reviewMember.participant)).dispatchEvent(new Event('click'));
  await setImmediate();
  return p;
}

test('review preview and cancel are local, translate instantly, and share only the selected answer once',async()=>{
  const p = await reviewedPanel({message:true});
  assert.equal(p.get('review-share').hidden,false); assert.equal(p.host.messages.length,0);
  await p.event('review-choose','click'); assert.equal(p.get('review-text').value,reviewAnswer);
  const calls = p.calls.length;
  await choose(p,'en'); assert.match(p.get('review-share-status').textContent,/Only the portion/);
  assert.equal(p.get('review-text').value,reviewAnswer); assert.equal(p.calls.length,calls);
  await p.event('review-cancel','click'); assert.equal(p.get('review-preview').hidden,true); assert.equal(p.get('review-text').value,'');
  await p.event('review-submit','click'); assert.equal(p.host.messages.length,0);
  await p.event('review-choose','click'); await choose(p,'ko');
  await p.event('review-submit','click'); await p.event('review-submit','click');
  assert.equal(p.host.messages.length,1); assert.equal(p.host.messages[0].content[0].text,reviewAnswer);
  assert.equal(p.get('review-preview').hidden,true); assert.match(p.get('review-share-status').textContent,/현재 대화로 보냈/);
  assert.equal(p.calls.length,calls);
});
test('review copy fallback, host refusal and stale or unrelated result never implicitly transmit a review',async()=>{
  for(const message of [true,false]){
    const p=await reviewedPanel({message}); await choose(p,'en');
    await p.event('review-choose','click');
    if(message){p.host.rejectMessage=true; await p.event('review-submit','click'); assert.match(p.get('review-share-status').textContent,/host declined/);}
    else {assert.equal(p.get('review-submit').hidden,true); await p.event('review-submit','click');assert.match(p.get('review-share-status').textContent,/Copy the answer/);}
    await p.event('review-copy','click'); assert.equal(p.get('review-text').selected,true); assert.equal(p.host.messages.length,message?1:0);
    assert.equal(p.get('review-text').value,reviewAnswer);
    await p.event('tab-overview','click'); assert.equal(p.get('review-preview').hidden,true);
    await p.event('review-submit','click');assert.equal(p.host.messages.length,message?1:0);
    await p.open('input',{arguments:{project:'outside'}}); assert.equal(p.get('review-text').value,'');assert.equal(p.get('review-share').hidden,true);
  }
  for(const overrides of [{assignment:null},{provider:'antigravity'},{result:{kind:'native-report',text:reviewAnswer}}]){
    const p=await reviewedPanel({message:true},overrides);
    assert.equal(p.get('review-share').hidden,true); await p.event('review-choose','click');await p.event('review-submit','click');
    assert.equal(p.host.messages.length,0);
  }
});
