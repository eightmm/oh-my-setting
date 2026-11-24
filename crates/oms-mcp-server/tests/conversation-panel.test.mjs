// Run with node --test crates/oms-mcp-server/tests/conversation-panel.test.mjs.
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { setImmediate } from 'node:timers/promises';
import test from 'node:test';
import { createContext, runInContext, runInNewContext } from 'node:vm';

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
  add(child) { this.children.push(child); }
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
  const host = {contexts:[],status:null, failStart:true, replies:[], messages:[], notices:[], tools:options.tools};
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
      result = {hostInfo:{name:options.host || 'Codex',version:options.version},hostContext:{displayMode:options.mode,locale:options.locale},hostCapabilities:{message:options.message ? {text:{}} : undefined,experimental:options.compose ? {"openai/message":{}} : {},updateModelContext:options.context ? {text:{},structuredContent:{}} : undefined}};
    }
    if(method==='ui/update-model-context'){host.contexts.push(params);result={isError:host.rejectContext===true};}
    if (method === 'ui/message') {
      host.messages.push(params);
      if (host.holdMessage) {host.finishMessage=ack=>notify({id,...ack});return;}
      if (host.messageError) { queueMicrotask(() => notify({id,error:host.messageError})); return; }
      result = {isError:host.rejectMessage === true};
    }
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
  class Option extends Element { constructor(text = '', value = text) { super(); this.textContent = text; this.value = value; } }
  const context = createContext({window, document, Option, TextEncoder, setTimeout, clearTimeout, crypto:{randomUUID},
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
  assert.match(cards, /Claude Leadlive-task▶ 실행 중/);
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
  assert.match(p.get('operations').textContent, /verifycheck-17× 실패 · 종료 코드 1/);
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
  assert.ok(p.host.notices.includes('ui/notifications/initialized')); assert.equal(p.host.initialize.appInfo.version,'0.6.0');
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
  assert.match(p.get('header-main').textContent,/Codex Main$/); assert.match(p.get('leads').textContent,/Codex LeadReview▶ 실행 중/);
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
  assert.match(p.get('leads').textContent,/Codex Lead수정 작업⟳ 검증 중/);
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

test('Overview binds its map before the dashboard finishes; node navigation and filtering stay read-only', async () => {
  const p = await panel(['one','two'],{}, {context:true});
  p.host.tools = {
    oms_panel_project_graph:({project,node:focus}) => ({
      schema:1,kind:'oms-panel-project-graph',provenance:{source:'git-ls-files',indexState:'unindexed'},scope:{focusNode:focus || 'dir:.'},status:'ready',selection:focus && focus!=='dir:.' ? {status:'found',node:{id:focus,kind:'directory',privateDetail:'omit-this'}} : null,nodes:focus && focus!=='dir:.' ? [{id:'src',label:'src',kind:'directory'},{id:'src/main.rs',label:'main.rs',kind:'file'}] :
        [{id:'dir:.',label:project,kind:'repository'},{id:'src',label:'src',kind:'directory',file_count:5,omitted_children:2},{id:'docs',label:'docs',kind:'directory'}],
      edges:[{source:focus || 'dir:.',target:focus && focus!=='dir:.' ? 'src/main.rs' : 'src',relation:'contains',confidence:'STRUCTURAL'}]
    }),
    oms_panel_capabilities:() => ({schema:1,commands:[],total:0})
  };
  p.host.delay='one';p.get('project-choice').value='one';await p.event('project-choice','change');
  assert.equal(p.get('map-field').hidden,false);
  assert.equal(p.get('map-root').textContent,'one');assert.match(p.get('map-status').textContent,/색인 없음/);
  assert.match(p.get('map-children').textContent,/src.*파일 5개/);
  assert.doesNotMatch(p.get('map-children').textContent,/docs|가져오기/);
  p.host.release(); await setImmediate();
  p.get('map-children').children[0].children.at(-1).dispatchEvent(new Event('click'));await setImmediate();
  assert.equal(p.get('pane-project').hidden,false);
  assert.match(p.get('graph-path').textContent,/src/);
  assert.match(p.get('graph-list').textContent,/main.rs/);
  assert.ok(!p.host.contexts.some(m=>JSON.stringify(m).includes('"node"')));
  p.host.rejectContext=true;await p.event('graph-attach','click');assert.match(p.get('graph-attached').textContent,/확인할 수 없습니다/);
  p.host.rejectContext=false;await p.event('graph-attach','click');
  const attached=p.host.contexts.at(-1);assert.equal(attached.structuredContent,undefined);
  assert.deepEqual(JSON.parse(attached.content[0].text),{project:'one',node:'src',kind:'directory'});
  assert.match(p.get('graph-attached').textContent,/전달했습니다/);
  p.get('graph-filter').value='missing';await p.event('graph-filter','input');
  assert.equal(p.get('graph-list').children.length,0);
  p.get('graph-path').children[0].dispatchEvent(new Event('click')); await setImmediate();
  assert.equal(p.get('graph-filter').value,'');
  assert.equal(p.calls.some(c=>['oms_start_main','oms_decide_approval'].includes(c.name)),false);
  assert.ok(p.calls.filter(c=>c.name==='oms_panel_project_graph').every(c=>c.args.project==='one'));
  p.get('project-choice').value='two';await p.event('project-choice','change');
  assert.equal(p.get('graph-attach').hidden,true);assert.equal(p.get('graph-attached').textContent,'');
  assert.ok(p.host.contexts.some(m=>Array.isArray(m.content)&&m.content.length===0));
});

test('host command directory searches every page in Korean and English; a help draft requires an explicit send', async () => {
  const p=await panel(['one','two'],{}, {message:true,compose:true});
  const calls=[];
  p.host.tools={oms_panel_capabilities:args=>{
    calls.push(args);
    return args.offset ? {schema:1,total:2,commands:[{name:'doctor',description:'Inspect the installed runtime'}]} :
      {schema:1,total:2,nextOffset:1,commands:[{name:'new-entry',description:'Read extension status'}]};
  }};
  p.get('project-choice').value='one';await p.event('project-choice','change');
  await p.event('feature-open','click'); await setImmediate();
  assert.deepEqual(calls.map(c=>c.offset),[undefined,1]);
  assert.ok(calls.every(c=>c.project==='one'));
  p.get('feature-filter').value='진단';await p.event('feature-filter','input');
  assert.match(p.get('feature-list').textContent,/oms doctor/);
  p.get('feature-list').children[0].children.at(-1).dispatchEvent(new Event('click'));await setImmediate();
  assert.match(p.get('feature-draft').value,/oms doctor/);assert.equal(p.host.messages.length,0);
  p.host.rejectMessage=true;await p.event('feature-send','click');assert.match(p.get('feature-outcome').textContent,/복사/);
  p.host.rejectMessage=false;await p.event('feature-send','click');assert.equal(p.host.messages.length,2);
  assert.deepEqual({...p.host.messages[1]._meta['openai/message']},{target:'active',send:false});
  p.get('feature-filter').value='extension';await p.event('feature-filter','input');
  assert.match(p.get('feature-list').textContent,/Read extension status/);
  p.get('project-choice').value='two';await p.event('project-choice','change');
  assert.equal(p.get('feature-draft').value,'');assert.equal(p.get('feature-editor').hidden,true);
});

test('all current public verbs have a Korean search label and new host verbs can use the native description', () => {
  const registry=readFileSync(new URL('../../oms/src/registry.rs',import.meta.url),'utf8');
  const names=JSON.parse(registry.match(/const PUBLIC: &str = (".*?");/)[1]).split('\n').sort();
  const glossary=runInNewContext('('+script.match(/const commandKorean = (\{[\s\S]+?\n\});/)[1]+')');
  assert.deepEqual(Object.keys(glossary).sort(),names);
  assert.ok(Object.values(glossary).every(label=>/[가-힣]/.test(label)));
});

test('selected function shows distinct direction, relation and confidence once per returned neighbor',async()=>{
  const p=await panel(['one'],{}, {locale:'en',tools:{oms_panel_project_graph:()=>({
    schema:1,kind:'oms-panel-project-graph',status:'ready',scope:{focusNode:'symbol:a::f'},nodes:[{id:'symbol:a::f',kind:'function',label:'f'},{id:'symbol:a::g',label:'g',kind:'function'}],provenance:{source:'indexed-graph'},
    edges:[{source:'symbol:a::f',target:'symbol:a::g',relation:'calls',confidence:'EXTRACTED'},{source:'symbol:a::g',target:'symbol:a::f',relation:'calls',confidence:'EXTRACTED'}],
    selection:{omittedNeighbors:7,neighbors:[{id:'symbol:a::g',direction:'out',relation:'calls',confidence:'EXTRACTED'},{id:'symbol:a::g',direction:'in',relation:'references',confidence:'INFERRED'},{id:'symbol:a::g',direction:'out',relation:'tests',confidence:'EXTRACTED'}]}
  })}});
  await p.event('tab-project','click');p.get('graph-confidence').value='all';await p.event('graph-confidence','change');
  const row=p.get('graph-list').children[0],info=row.textContent;
  for(const label of ['→ Calls · Extracted','← Calls · Extracted','← References · Inferred','→ Test link · Extracted']) assert.equal(info.split(label).length,2);
  assert.match(p.get('graph-info').textContent,/7 links omitted/);
  assert.doesNotMatch(p.get('map-children').textContent,/References/);
  p.get('language-choice').value='ko';await p.event('language-choice','change');
  assert.match(p.get('graph-list').textContent,/← 참조 · 추론/);
});

test('standard hosts retain selectable command help; provenance and inferred associations are explicit', async () => {
  const p=await panel(['one','two'],{}, {message:true,readOnly:true});
  p.host.tools={
    oms_panel_capabilities:()=>({schema:1,total:1,commands:[{name:'land',description:'Publish a tested branch'}]}),
    oms_panel_project_graph:({node,query})=>({
      schema:1,kind:'oms-panel-project-graph',status:query?'not_found':'ready',completeness:'partial',
      provenance:{source:'indexed-graph',indexState:'present',indexFreshness:'stale'},scope:{focusNode:node || 'file:a'},
      nodes:query?[]:[{id:'file:a',label:'a',kind:'file'},{id:'file:b',label:'b',kind:'file'},{id:'file:c',label:'c',kind:'file'}],
      edges:[{source:'file:a',target:'file:b',relation:'imports',confidence:'EXTRACTED'},{source:'file:a',target:'file:c',relation:'imports',confidence:'INFERRED'}]
    })
  };
  p.get('project-choice').value='one';await p.event('project-choice','change');await p.event('feature-open','click');
  p.get('map-children').children[0].children.at(-1).dispatchEvent(new Event('click'));await setImmediate();
  assert.equal(p.get('map-children').children.length,0);
  assert.match(p.get('map-status').textContent,/오래된 색인/);assert.match(p.get('map-dep-list').textContent,/→.*추출/);assert.doesNotMatch(p.get('map-dep-list').textContent,/추론/);
  p.get('graph-confidence').value='all';await p.event('graph-confidence','change');assert.match(p.get('map-dep-list').textContent,/추론/);
  p.get('graph-filter').value='not-there';await p.event('graph-search','click');assert.match(p.get('map-status').textContent,/일치하는 노드 없음/);
  p.get('feature-list').children[0].children.at(-1).dispatchEvent(new Event('click'));await p.event('feature-send','click');
  assert.equal(p.host.messages.length,0);assert.equal(p.get('feature-draft').selected,true);
});

test('Git composer preview does not consume the existing explicit send action', async () => {
  const p=await panel(['one'],{conversationMain:matchedMain},{message:true,compose:true});
  await pickIssue(p);
  await p.event('git-compose','click');
  assert.deepEqual({...p.host.messages[0]._meta['openai/message']},{target:'active',send:false});
  assert.equal(p.get('git-send').disabled,false);
  await p.event('git-send','click');
  assert.equal(p.host.messages.length,2);assert.equal(p.host.messages[1]._meta,undefined);

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
  assert.ok(Buffer.byteLength(html) < 240 * 1024);
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
  assert.match(p.get('leads').textContent, /Claude Lead작업 task▶ 실행 중/);
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
  assert.equal(p.get('tab-overview').textContent, 'Now');
  assert.equal(p.get('goal-heading').textContent, 'Project goal');
  assert.equal(p.get('status').textContent, '~ Partial · Some status could not be collected.');
  assert.match(p.get('leads').textContent, /Claude Lead.*▶ Running/);
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
  assert.equal(p.get('status').textContent, '~ 일부 · 일부 상태를 수집하지 못했습니다.');
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

test('an ambiguous host send failure keeps the selected answer copyable without repeating the message',async()=>{
  const p=await reviewedPanel({message:true});
  p.host.messageError={code:-32603,message:'Host failed after delivery'};
  await choose(p,'en');await p.event('review-choose','click');await p.event('review-submit','click');
  assert.match(p.get('review-share-status').textContent,/Delivery is unconfirmed/);
  await p.event('review-submit','click');await p.event('review-copy','click');
  assert.equal(p.host.messages.length,1); assert.equal(p.get('review-text').selected,true);
});

for (const lang of ['en','ko']) test(`unknown review delivery stays copyable after dismiss and tab return, never resends (${lang})`,async()=>{
  const p=await reviewedPanel({message:true});
  p.host.messageError={code:-32603,message:'Host failed after delivery'};
  await choose(p,lang);
  await p.event('review-choose','click');await p.event('review-submit','click');
  const unknown=p.get('review-share-status').textContent; assert.match(unknown,lang==='en'?/Delivery is unconfirmed/:/미확인/);
  await p.event('review-cancel','click');
  assert.equal(p.get('review-choose').disabled,false);
  await p.event('tab-overview','click');await p.event('tab-chat','click');
  assert.equal(p.get('review-choose').disabled,false);
  await p.event('review-choose','click');
  assert.equal(p.get('review-submit').hidden,true); assert.equal(p.get('review-text').value,reviewAnswer);
  await p.event('review-submit','click');await p.event('review-copy','click');
  assert.equal(p.get('review-text').selected,true); assert.equal(p.host.messages.length,1);
});
test('explicit review delivery success moves focus to the chat heading and locks the answer',async()=>{
  const p=await reviewedPanel({message:true});
  await p.event('review-choose','click');const before=p.get('chat-title').focused??0;await p.event('review-submit','click');
  assert.equal(p.get('review-choose').disabled,true); assert.equal(p.host.messages.length,1);
  assert.equal(p.get('chat-title').focused,before+1);
});

test('a late delivery is reconciled after the preview is dismissed without moving focus across tabs',async()=>{
  for(const reply of [{result:{}},{error:{code:-32603,message:'Host response lost'}}]) for(const returnEarly of [true,false]){
    const p=await reviewedPanel({message:true}); await choose(p,'en');
    p.host.holdMessage=true; await p.event('review-choose','click'); await p.event('review-submit','click');
    await p.event('tab-overview','click'); const focus=p.get('chat-title').focused;
    assert.equal(p.get('review-choose').disabled,true);
    if(returnEarly) await p.event('tab-chat','click');
    p.host.finishMessage(reply); await setImmediate();assert.equal(p.get('chat-title').focused,focus);
    if(!returnEarly) await p.event('tab-chat','click');
    assert.equal(p.get('review-choose').disabled,!reply.error);
    if(reply.error) {
      await p.event('review-choose','click'); assert.equal(p.get('review-cancel').disabled,false);
      assert.equal(p.get('review-submit').hidden,true);assert.equal(p.get('review-text').value,reviewAnswer);
    }
    assert.equal(p.host.messages.length,1);
  }
});

const workFixture = () => ({schema:1, omitted:2, availability:'partial', items:[
  {id:'t1', title:'Same', state:'working', owner:'lead-a', depends:[], issue:'#12', result:'completed', verification:null, revision:null},
  {id:'t2', title:'Same', state:'done', owner:null, depends:['t1','ghost'], issue:null, result:null, verification:'not_run', revision:'r2'},
  {id:'t3', title:'Loop A', state:'pending', owner:'lead-a', depends:['t4'], result:null, verification:null, revision:null},
  {id:'t4', title:'Loop B', state:'review', owner:'lead-b', depends:['t3'], result:null, verification:null, revision:null},
  {id:'dup', title:'x'}, {id:'dup', title:'y'}, {title:'no id'}]});
const click = async (p, key) => { p.run(`taskRows.get(${JSON.stringify(key)})`).dispatchEvent(new Event('click')); await setImmediate(); };
const press = async (p, id, key) => { p.get(id).dispatchEvent(Object.assign(new Event('keydown', {cancelable:true}), {key})); await setImmediate(); };

test('one task detail opens by real id from every primary tab, returns focus and writes nothing', async () => {
  const p = await panel(['one'], {}, {tools:{oms_panel_work_items:workFixture}});
  const mark = p.calls.length, contexts = p.host.contexts.length;
  p.get('brief').value = 'draft goal';
  const seen = [];
  for (const [tab, key] of [['overview','now:t1'], ['work','work:t1'], ['results','results:t1'], ['relations','rel-owner:t1'], ['relations','rel-dep:t1']]) {
    await p.event('tab-' + tab, 'click');
    await click(p, key);
    assert.equal(p.run('selectedTask.id'), 't1');
    assert.equal(p.get('pane-task').hidden, false); assert.equal(p.get('pane-' + tab).hidden, true);
    assert.equal(p.get('task-heading').focused > 0, true);
    seen.push(p.get('task-body').textContent);
    await p.event('task-back', 'click');
    assert.equal(p.get('pane-task').hidden, true); assert.equal(p.get('pane-' + tab).hidden, false);
    assert.ok(p.run(`taskRows.get(${JSON.stringify(key)})`).focused > 0, key);
    await click(p, key); await press(p, 'pane-task', 'Escape');
    assert.equal(p.get('pane-task').hidden, true);
  }
  assert.equal(new Set(seen).size, 1);
  assert.match(seen[0], /#12/);
  // Same title, distinct ids; repeated and id-less items never become selectable rows.
  assert.equal(p.run('taskRows.has("work:t2")'), true); assert.equal(p.run('workData.byId.has("dup")'), false);
  assert.match(p.get('work-status').textContent, /2개는 표시 범위 밖/); assert.match(p.get('work-status').textContent, /1개/);
  assert.ok(p.calls.slice(mark).every(c => c.name === 'oms_panel_work_items'));
  assert.equal(p.host.messages.length, 0); assert.equal(p.host.contexts.length, contexts);
  assert.equal(p.get('brief').value, 'draft goal');
});

test('absent evidence is unavailable and relationships keep owner linkage apart from dependency edges', async () => {
  const p = await panel(['one'], {}, {tools:{oms_panel_work_items:workFixture}});
  await p.event('tab-work', 'click'); await click(p, 'work:t1');
  await p.event('task-tab-verification', 'click');
  assert.match(p.get('task-body').textContent, /검증 근거가 기록되지 않았습니다/);
  await p.event('task-tab-changes', 'click');
  assert.match(p.get('task-body').textContent, /결과 완료 기록/); assert.match(p.get('task-body').textContent, /diff가 포함되지 않았습니다/);
  await p.event('task-tab-history', 'click');
  assert.match(p.get('task-body').textContent, /확인할 수 없어 표시하지 않습니다/);
  await p.event('task-back', 'click');
  await p.event('tab-results', 'click');
  assert.match(p.get('results-list').textContent, /검증 근거 없음/);
  await p.event('tab-relations', 'click');
  assert.match(p.get('owners-list').textContent, /담당 미확인/); assert.doesNotMatch(p.get('owners-list').textContent, /ghost/);
  assert.match(p.get('owners-note').textContent, /추론하지 않습니다/);
  assert.match(p.get('deps-list').textContent, /ghost \(목록에 없음\)/);
  assert.match(p.get('deps-list').textContent, /순환 의존.*t3.*t4|순환 의존.*t4.*t3/);
});

test('unavailable task inventory leaves counts and relationships unasserted', async () => {
  const p=await panel(['one'],{}, {locale:'en',tools:{oms_panel_work_items:()=>({schema:1,items:[],omitted:0,availability:'unavailable'})}});
  assert.match(p.get('now-status').textContent,/unavailable/);
  assert.doesNotMatch(p.get('now-status').textContent,/0 work items/);assert.equal(p.get('owners-list').textContent,'');
});

test('tool failure shows unavailable, never a dashboard-derived list', async () => {
  const p = await panel(['one'], {tasks:3, tasksVerified:1});
  assert.match(p.get('now-status').textContent, /사용할 수 없습니다/);
  assert.equal(p.run('taskRows.size'), 0); assert.equal(p.run('workData'), undefined);
  const q = await panel(['one'], {}, {tools:{oms_panel_work_items:() => ({schema:2, items:[]})}});
  assert.match(q.get('work-status').textContent, /형식을 확인할 수 없습니다/);
});

test('selection survives refresh and language change, vanishes honestly, and clears with the project', async () => {
  let data = workFixture();
  const p = await panel(['one','two'], {}, {tools:{oms_panel_work_items:() => data}, storage:{getItem:() => null, setItem() {}}});
  p.get('project-choice').value = 'one'; await p.event('project-choice', 'change');
  await p.event('tab-work', 'click'); await click(p, 'work:t2');
  p.get('brief').value = 'keep me';
  p.get('language-choice').value = 'en'; await p.event('language-choice', 'change');
  assert.equal(p.run('selectedTask.id'), 't2'); assert.equal(p.get('task-back').textContent, '← Back to list');
  assert.equal(p.get('tab-overview').textContent, 'Now'); assert.equal(p.get('brief').value, 'keep me');
  data = {...workFixture(), items:workFixture().items.map(i => i.id === 't2' ? {...i, revision:'r3'} : i)};
  await p.event('refresh', 'click');
  assert.match(p.get('task-note').textContent, /r2 → r3/);
  await p.event('task-tab-verification','click');
  assert.doesNotMatch(p.get('task-body').textContent,/Verification not run/);assert.match(p.get('task-body').textContent,/Open current version/);
  data = {...data, items:data.items.filter(i => i.id !== 't2')};
  await p.event('refresh', 'click');
  assert.equal(p.run('selectedTask.id'), 't2'); assert.match(p.get('task-note').textContent, /not in the current observation: t2/);
  p.get('project-choice').value = 'two'; await p.event('project-choice', 'change');
  assert.equal(p.run('selectedTask'), undefined); assert.equal(p.get('pane-task').hidden, true);
});

test('primary and secondary tablists each rove with arrows and every v10 view stays reachable', async () => {
  const p = await panel(['one'], {}, {tools:{oms_panel_work_items:workFixture}});
  await press(p, 'tab-overview', 'ArrowRight');
  assert.equal(p.run('currentTab'), 'work'); assert.ok(p.get('tab-work').focused > 0);
  await press(p, 'tab-work', 'End'); assert.equal(p.run('currentTab'), 'relations');
  assert.equal(p.get('tab-project').tabIndex, 0);
  await p.event('tab-project', 'click'); assert.equal(p.get('more-views').open, true);
  await press(p, 'tab-project', 'ArrowRight'); assert.equal(p.run('currentTab'), 'chat');
  for (const tab of ['project', 'chat', 'activity', 'usage', 'git']) { await p.event('tab-' + tab, 'click'); assert.equal(p.get('pane-' + tab).hidden, false); }
  await p.event('tab-overview', 'click');
  assert.equal(p.get('tab-overview').tabIndex, 0); assert.equal(p.get('tab-project').tabIndex, 0);
});

const styleCss = html.match(/<style>([\s\S]+?)<\/style>/)[1];
function findNodes(root, predicate, found = []) {
  if (!root || typeof root !== 'object') return found;
  if (predicate(root)) found.push(root);
  for (const child of root.children || []) findNodes(child, predicate, found);
  return found;
}
const stateChips = root => findNodes(root, el => /\bstate-chip\b/.test(el.className || ''));

test('first-screen order prioritizes decision, running work, latest results, and project map last without double-nested details', async () => {
  const overview = html.slice(html.indexOf('id="pane-overview"'), html.indexOf('id="pane-work"'));
  const order = ['id="approval-card"', 'id="alerts"', 'id="room-leads"', 'id="now-work"', 'id="goal-card"', 'id="latest-results"', 'id="map-preview"'];
  const positions = order.map(marker => overview.indexOf(marker));
  assert.ok(positions.every(pos => pos >= 0), 'all overview sections exist');
  for (let i = 1; i < positions.length; i++) assert.ok(positions[i - 1] < positions[i], `${order[i - 1]} before ${order[i]}`);
  // No double-nested <details> inside #pane-overview.
  let depth = 0, maxDepth = 0;
  for (const [, close] of overview.matchAll(/<(\/?)details\b/g)) {
    depth += close ? -1 : 1;
    if (depth > maxDepth) maxDepth = depth;
  }
  assert.equal(maxDepth, 1);
  // approval-idle is outside #project-records summary.
  const recordsSummary = html.match(/<details[^>]*id="project-records"[\s\S]*?<summary[^>]*>[\s\S]*?<\/summary>/)[0];
  assert.doesNotMatch(recordsSummary, /id="approval-idle"/);

  // Single approval expands automatically; alerts show inline count badge and first reason; top 3 artifacts surface directly.
  const row1 = {id:'ap1', version:1, digest:'d1', summary:'Merge PR #9', action:'Main scope decision', object:'room-1', sender:'codex'};
  const row2 = {id:'ap2', version:1, digest:'d2', summary:'Run migration', action:'Run task', object:'room-1', sender:'claude'};
  let approvals = [row1];
  const p = await panel(['one'], {
    attention:[{summary:'CI gate failed', count:1}, {summary:'Stale lease', count:1}],
    operations:[
      {kind:'verify', artifactId:'art-1', status:'failed', exit:1},
      {kind:'review', artifactId:'art-2', status:'accepted'},
      {kind:'patch', artifactId:'art-3', status:'done'},
      {kind:'extra', artifactId:'art-4', status:'queued'}
    ]
  }, {tools:{oms_list_approvals:() => ({approvals})}});
  assert.equal(p.get('approvals').children[0].open, true);
  assert.equal(p.get('alerts-badge').hidden, false);
  assert.equal(p.get('alerts-count').textContent, '2건');
  assert.equal(p.get('alerts-preview').textContent, 'CI gate failed');
  assert.equal(p.get('latest-operations').hidden, false);
  assert.equal(p.get('latest-operations').children.length, 3);
  assert.doesNotMatch(p.get('latest-operations').textContent, /art-4/);
  await choose(p, 'en');
  assert.equal(p.get('alerts-count').textContent, '2 items');
  approvals = [row1, row2];
  await p.event('refresh', 'click');
  assert.equal(p.get('approvals').children.length, 2);
  assert.notEqual(p.get('approvals').children[0].open, true);
});

test('state-chip renders canonical glyph and word across every state family in Korean and English with no raw backend tokens', async () => {
  const lead = {room:'r', participant:'lead-1', provider:'claude', role:'lead'};
  const advisor = {room:'r', participant:'qa-1', provider:'agy', role:'reviewer'};
  const p = await panel(['one'], {
    conversationBinding:'matched',
    conversationMain:{source:'room-session', state:'joined', provider:'codex', room:'r', participant:'owner', participants:[lead, advisor]},
    activeAttempts:[
      {...lead, task:'Fix auth', state:'working'},
      {...advisor, task:'Check logs', state:'timed_out'}
    ],
    attempts:[{id:'a1', provider:'codex', tool:'run', task:'Build', room:'r', state:'done'}],
    operations:[{kind:'verify', artifactId:'op-1', status:'failed', exit:1}],
    taskStatus:'waiting_approval',
    commandStatus:{available:true, total:3, active:1, executing:1, byState:{working:1, failed:1, timed_out:1}}
  }, {tools:{
    oms_list_approvals:() => ({approvals:[{id:'ap1', version:1, digest:'d1', summary:'Approve step', action:'Step', object:'r'}]}),
    oms_panel_work_items:workFixture,
    oms_panel_git_status:() => ({git:{available:true, branch:'main', dirty:false}, github:{status:'available', repository:'org/repo',
      pullRequests:[{number:4, title:'Checks PR', headBranch:'feat', baseBranch:'main', checks:{passed:1, failed:1, pending:1, other:0,
        items:[{name:'gate', status:'failed'}, {name:'lint', status:'passed'}, {name:'focused', status:'pending'}]}}]}})
  }});
  await p.event('tab-git', 'click');
  await p.event('tab-activity', 'click');

  // Verify state-chip elements exist across every family: leads, threads, activity/records, git checks, work items, approval.
  const leadChips = stateChips(p.get('leads'));
  const threadChips = stateChips(p.get('thread-rows'));
  const timelineChips = stateChips(p.get('command-list'));
  const opsChips = stateChips(p.get('operations'));
  const activityChips = stateChips(p.get('activity'));
  const taskStatusChips = stateChips(p.get('taskStatus'));
  const checkChips = stateChips(p.get('git-content'));
  const workChips = stateChips(p.get('now-active'));
  assert.ok(leadChips.length >= 1 && threadChips.length >= 1 && timelineChips.length >= 2 && opsChips.length >= 1 && activityChips.length >= 1 && taskStatusChips.length === 1 && checkChips.length === 3 && workChips.length >= 1);
  assert.equal(p.get('approval-badge').hidden, false);
  assert.match(html, /id="approval-badge" class="state-chip" data-state="waiting" data-role="attention"/);
  const approvalWord = p.staticNodes.find(n => n.attributes.get('data-i18n') === 'state.waiting');
  assert.equal(approvalWord.textContent, '확인 필요');

  // Korean assertions: canonical glyph + Korean word, zero raw English backend tokens.
  assert.equal(leadChips[0].textContent, '▶ 실행 중');
  assert.equal(threadChips[0].textContent, '◷ 시간 초과');
  assert.equal(opsChips[0].textContent, '× 실패');
  assert.equal(activityChips[0].textContent, '✓ 완료');
  assert.equal(taskStatusChips[0].textContent, '! 확인 필요');
  assert.deepEqual(checkChips.map(c => c.textContent), ['× 실패', '✓ 통과', '· 대기 중']);
  assert.doesNotMatch(p.get('command-list').textContent, /\b(working|failed|done|blocked|timed_out)\b/);
  assert.doesNotMatch(p.get('command-state').textContent, /\b(working|failed|timed_out)\b/);
  assert.match(p.get('command-state').textContent, /실행 중 1 · 실패 1 · 시간 초과 1/);

  // English switch updates every state-chip in place.
  await choose(p, 'en');
  assert.equal(leadChips[0].textContent, '▶ Running');
  assert.equal(threadChips[0].textContent, '◷ Timed out');
  assert.equal(opsChips[0].textContent, '× Failed');
  assert.equal(activityChips[0].textContent, '✓ Done');
  assert.equal(taskStatusChips[0].textContent, '! Needs you');
  assert.deepEqual(checkChips.map(c => c.textContent), ['× Failed', '✓ Passed', '· Pending']);
  assert.equal(approvalWord.textContent, 'Needs you');
  assert.doesNotMatch(styleCss, /\.thread-dot|\.work-dot|\.lead-state::before|#approval-card\.awaiting-action h2::before/);
});

test('honest state mapping preserves distinct terminal/live states, unknown raw tokens, neutral Leads count, and relabels attempt options', async () => {
  const leads = [
    {room:'r', participant:'lead-1', provider:'claude', role:'lead', task:'Claimed task', state:'claimed'},
    {room:'r', participant:'lead-2', provider:'codex', role:'lead', task:'Landing task', state:'landing'},
    {room:'r', participant:'lead-3', provider:'agy', role:'lead', task:'Custom task', state:'paused_x'}
  ];
  const p = await panel(['one'], {
    conversationBinding:'matched',
    conversationMain:{source:'room-session', state:'joined', provider:'codex', room:'r', participant:'owner', participants:leads},
    activeAttempts:leads,
    attempts:[
      {id:'a-cancelled', provider:'codex', tool:'run', task:'t1', room:'r', state:'cancelled'},
      {id:'a-canceled', provider:'codex', tool:'run', task:'t2', room:'r', state:'canceled'},
      {id:'a-abandoned', provider:'codex', tool:'run', task:'t3', room:'r', state:'abandoned'},
      {id:'a-interrupted', provider:'codex', tool:'run', task:'t4', room:'r', state:'interrupted'},
      {id:'a-closed', provider:'codex', tool:'run', task:'t5', room:'r', state:'closed'},
      {id:'a-completed', provider:'codex', tool:'run', task:'t6', room:'r', state:'completed'},
      {id:'a-accepted', provider:'codex', tool:'run', task:'t7', room:'r', state:'accepted'},
      {id:'a-done', provider:'codex', tool:'run', task:'t8', room:'r', state:'done'},
      {id:'a-exited', provider:'codex', tool:'run', task:'t9', room:'r', state:'exited'},
      {id:'a-unknown', provider:'codex', tool:'run', task:'t10', room:'r', state:'paused_x'}
    ]
  }, {tools:{
    oms_panel_chat:a => ({room:a.room, participant:a.participant, source:'room', messages:[],
      attempts:[
        {id:'att-1', tool:'peer-review', state:'done', sections:['info']},
        {id:'att-2', tool:'run', state:'cancelled', sections:['info']},
        {id:'att-3', tool:'run', state:'landing', sections:['info']},
        {id:'att-4', tool:'run', state:'paused_x', sections:['info']}
      ]})
  }});

  // Leads heading badge shows only a neutral count with an accessible name; individual rows carry their own chips.
  const leadsBadge = p.get('work-dot');
  assert.equal(leadsBadge.hidden, false);
  assert.equal(leadsBadge.textContent, '3');
  assert.equal(leadsBadge.getAttribute('aria-label'), 'Lead 3개');
  assert.doesNotMatch(leadsBadge.textContent, /실행|Running|▶|⟳|▷|!/);
  assert.doesNotMatch(leadsBadge.className || '', /\bstate-chip\b/);

  const leadChips = stateChips(p.get('leads'));
  assert.deepEqual(leadChips.map(c => c.textContent), ['▷ 점유됨', '⟳ 착지 중', '? paused_x']);

  const chips = stateChips(p.get('activity'));
  assert.deepEqual(chips.map(c => c.textContent), [
    '⊘ 취소됨', '⊘ 취소됨', '⊘ 중단됨', '◷ 중단됨', '· 닫힘',
    '✓ 완료', '✓ 검증 완료', '✓ 완료', '✓ 종료됨', '? paused_x'
  ]);
  assert.equal(chips[0].getAttribute('data-role'), 'muted');
  assert.equal(chips[0].getAttribute('data-state'), 'cancelled');
  assert.equal(chips[1].getAttribute('data-role'), 'muted');
  assert.equal(chips[2].getAttribute('data-role'), 'muted');
  assert.equal(chips[3].getAttribute('data-role'), 'warning');
  assert.equal(chips[4].getAttribute('data-role'), 'muted');
  assert.equal(chips[5].getAttribute('data-state'), 'completed');
  assert.notEqual(chips[5].getAttribute('data-state'), chips[6].getAttribute('data-state'));
  assert.equal(chips[7].getAttribute('data-state'), 'done');
  assert.equal(chips[8].getAttribute('data-state'), 'exited');
  assert.equal(chips[9].getAttribute('data-role'), 'muted');
  assert.equal(p.run('MSG.ko["state.done"]'), p.run('MSG.ko["g.done"]'));
  assert.equal(p.run('MSG.en["state.done"]'), p.run('MSG.en["g.done"]'));

  // Open chat to populate evidence-attempt select options, then switch language.
  await p.event('header-main', 'click');
  const optionTexts = () => p.get('evidence-attempt').children.map(o => o.textContent);
  assert.deepEqual(optionTexts(), [
    '호출 선택',
    'peer-review · att-1 · 완료',
    'run · att-2 · 취소됨',
    'run · att-3 · 착지 중',
    'run · att-4 · ? paused_x'
  ]);

  await choose(p, 'en');
  assert.equal(leadsBadge.textContent, '3');
  assert.equal(leadsBadge.getAttribute('aria-label'), '3 Leads');
  assert.deepEqual(leadChips.map(c => c.textContent), ['▷ Claimed', '⟳ Landing', '? paused_x']);
  assert.deepEqual(chips.map(c => c.textContent), [
    '⊘ Cancelled', '⊘ Cancelled', '⊘ Abandoned', '◷ Interrupted', '· Closed',
    '✓ Completed', '✓ Verified', '✓ Done', '✓ Exited', '? paused_x'
  ]);
  assert.deepEqual(optionTexts(), [
    'Select a call',
    'peer-review · att-1 · Done',
    'run · att-2 · Cancelled',
    'run · att-3 · Landing',
    'run · att-4 · ? paused_x'
  ]);
  assert.doesNotMatch(optionTexts().join('\n'), /완료|취소됨|착지 중|호출 선택/);

  await choose(p, 'ko');
  assert.equal(leadsBadge.getAttribute('aria-label'), 'Lead 3개');
  assert.deepEqual(optionTexts(), [
    '호출 선택',
    'peer-review · att-1 · 완료',
    'run · att-2 · 취소됨',
    'run · att-3 · 착지 중',
    'run · att-4 · ? paused_x'
  ]);
});

test('round 3 advisories: unified git check states, distinct states, Korean ready label, parent row above focus, lone approval collapse memory', async () => {
  // 1, 2, 6: Git checks unified lookup, distinct states (orphaned/guard_stop/partial/degraded/error), state.ready Korean label.
  const storage = memoryStorage();
  const ap1 = {id:'ap1', version:1, digest:'d1', summary:'First approval', action:'Main scope decision', object:'r1', sender:'codex'};
  const ap2 = {id:'ap2', version:1, digest:'d2', summary:'Second approval', action:'Run task', object:'r1', sender:'claude'};
  let approvals = [ap1];
  let graphPayload = {
    schema:1, kind:'oms-panel-project-graph', status:'ready', provenance:{source:'indexed-graph'},
    scope:{focusNode:'dir:src'},
    nodes:[
      {id:'dir:root', label:'root', kind:'directory'},
      {id:'pkg:ws', label:'workspace', kind:'module'},
      {id:'dir:src', label:'src', kind:'directory'},
      {id:'file:src/lib.rs', label:'lib.rs', kind:'file'},
      {id:'file:src/mod.rs', label:'mod.rs', kind:'file'},
      {id:'file:core.rs', label:'core.rs', kind:'file'}
    ],
    edges:[
      {source:'dir:root', target:'dir:src', relation:'contains', confidence:'STRUCTURAL'},
      {source:'pkg:ws', target:'dir:src', relation:'owns', confidence:'STRUCTURAL'},
      {source:'dir:src', target:'file:src/lib.rs', relation:'contains', confidence:'STRUCTURAL'},
      {source:'dir:src', target:'file:src/mod.rs', relation:'owns', confidence:'STRUCTURAL'},
      {source:'dir:src', target:'file:core.rs', relation:'imports', confidence:'EXTRACTED'}
    ]
  };
  const p = await panel(['one'], {
    conversationBinding:'matched',
    conversationMain:{source:'room-session', state:'joined', provider:'codex', room:'r', participant:'owner'},
    activeAttempts:[{room:'r', participant:'lead-1', provider:'claude', role:'lead', task:'Ready task', state:'ready'}],
    attempts:[
      {id:'s-orphaned', provider:'codex', tool:'run', task:'t1', room:'r', state:'orphaned'},
      {id:'s-guard', provider:'codex', tool:'run', task:'t2', room:'r', state:'guard_stop'},
      {id:'s-partial', provider:'codex', tool:'run', task:'t3', room:'r', state:'partial'},
      {id:'s-degraded', provider:'codex', tool:'run', task:'t4', room:'r', state:'degraded'},
      {id:'s-error', provider:'codex', tool:'run', task:'t5', room:'r', state:'error'},
      {id:'s-ready', provider:'codex', tool:'run', task:'t6', room:'r', state:'ready'}
    ]
  }, {storage, tools:{
    oms_list_approvals:() => ({approvals}),
    oms_panel_project_graph:() => graphPayload,
    oms_panel_git_status:() => ({git:{available:true, branch:'main', dirty:false}, github:{status:'available', repository:'org/repo',
      pullRequests:[{number:9, title:'Check matrix', headBranch:'fix', baseBranch:'main', checks:{passed:1, failed:1, pending:1, other:1,
        items:[
          {name:'c-fail', status:'failed'},
          {name:'c-pass', status:'passed'},
          {name:'c-pend', status:'pending'},
          {name:'c-other', status:'other'},
          {name:'c-cancel', status:'cancelled'},
          {name:'c-err', status:'error'},
          {name:'c-weird', status:'custom_check'},
          {name:'c-empty', status:''}
        ]}}]}})
  }});
  await p.event('tab-git', 'click');

  // Distinct states + Korean state.ready + error stays failed.
  const actChips = stateChips(p.get('activity'));
  assert.deepEqual(actChips.map(c => c.textContent), [
    '? 연결 끊김', '! 가드 정지', '~ 일부', '~ 저하', '× 실패', '▷ 준비됨'
  ]);
  assert.deepEqual(actChips.map(c => c.getAttribute('data-state')), [
    'orphaned', 'guard_stop', 'partial', 'degraded', 'failed', 'ready'
  ]);
  assert.deepEqual(actChips.map(c => c.getAttribute('data-role')), [
    'warning', 'warning', 'warning', 'warning', 'error', 'accent'
  ]);
  assert.equal(p.run('MSG.ko["state.ready"]'), '준비됨');
  assert.equal(p.get('work-dot').getAttribute('aria-label'), 'Lead 1개');

  // Git checks: one lookup, general-table fallback, ? raw for unknown.
  const checkChips = stateChips(p.get('git-content'));
  assert.deepEqual(checkChips.map(c => c.textContent), [
    '× 실패', '✓ 통과', '· 대기 중', '· 기타', '⊘ 취소됨', '× 실패', '? custom_check', '? 미확인'
  ]);
  assert.deepEqual(checkChips.map(c => c.getAttribute('data-role')), [
    'error', 'success', 'muted', 'muted', 'muted', 'error', 'muted', 'muted'
  ]);

  // 3: Direction-aware ownership — parents above focus, never in children tree.
  const fieldHtml = html.slice(html.indexOf('id="map-field"'), html.indexOf('id="map-status"'));
  assert.ok(fieldHtml.indexOf('id="map-parents"') > 0 && fieldHtml.indexOf('id="map-parents"') < fieldHtml.indexOf('id="map-root"'));
  assert.ok(fieldHtml.indexOf('id="map-root"') < fieldHtml.indexOf('id="map-children"'));
  const exploreHtml = html.slice(html.indexOf('id="explore-map"'), html.indexOf('id="graph-attach"'));
  assert.ok(exploreHtml.indexOf('id="explore-parents"') > 0 && exploreHtml.indexOf('id="explore-parents"') < exploreHtml.indexOf('id="explore-root"'));
  assert.equal(p.get('map-parents').hidden, false);
  assert.equal(p.get('explore-parents').hidden, false);
  assert.match(p.get('map-parent-list').textContent, /root/);
  assert.match(p.get('map-parent-list').textContent, /workspace/);
  assert.match(p.get('explore-parent-list').textContent, /root/);
  assert.match(p.get('map-children').textContent, /lib\.rs/);
  assert.match(p.get('map-children').textContent, /mod\.rs/);
  assert.doesNotMatch(p.get('map-children').textContent, /root|workspace/);
  assert.doesNotMatch(p.get('explore-children').textContent, /root|workspace/);

  // English switch updates distinct states, git checks, parent label, and Lead count aria-label.
  await choose(p, 'en');
  assert.deepEqual(actChips.map(c => c.textContent), [
    '? Orphaned', '! Guard stop', '~ Partial', '~ Degraded', '× Failed', '▷ Ready'
  ]);
  assert.deepEqual(checkChips.map(c => c.textContent), [
    '× Failed', '✓ Passed', '· Pending', '· Other', '⊘ Cancelled', '× Failed', '? custom_check', '? Unknown'
  ]);
  assert.equal(p.get('work-dot').getAttribute('aria-label'), '1 Lead');
  assert.equal(p.get('map-parents-label').textContent, 'Parent');
  assert.equal(p.get('explore-parents-label').textContent, 'Parent');
  await choose(p, 'ko');

  // 4: Lone approval auto-opens once per id; collapsing stays collapsed across refreshes (memory only, no storage writes).
  const setsBefore = storage.sets;
  assert.equal(p.get('approvals').children[0].open, true);
  await p.event('refresh', 'click');
  assert.equal(p.get('approvals').children[0].open, true);
  p.get('approvals').children[0].open = false;
  p.get('approvals').children[0].dispatchEvent(new Event('toggle'));
  await p.event('refresh', 'click');
  assert.equal(p.get('approvals').children[0].open, false);
  await p.event('refresh', 'click');
  assert.equal(p.get('approvals').children[0].open, false);
  // New lone approval id auto-opens once.
  approvals = [ap2];
  await p.event('refresh', 'click');
  assert.equal(p.get('approvals').children[0].open, true);
  // Returning to collapsed ap1 keeps it collapsed; no storage writes for approvals.
  approvals = [ap1];
  await p.event('refresh', 'click');
  assert.equal(p.get('approvals').children[0].open, false);
  assert.equal(storage.sets, setsBefore);

  // Leaf with incoming contains only (no outgoing children, plus an unconnected node) never puts parent or unconnected node in children tree.
  graphPayload = {
    schema:1, kind:'oms-panel-project-graph', status:'ready', provenance:{source:'indexed-graph'},
    scope:{focusNode:'file:src/leaf.rs'},
    nodes:[
      {id:'dir:src', label:'src', kind:'directory'},
      {id:'file:src/leaf.rs', label:'leaf.rs', kind:'file'},
      {id:'file:stray.rs', label:'stray.rs', kind:'file'}
    ],
    edges:[{source:'dir:src', target:'file:src/leaf.rs', relation:'contains', confidence:'STRUCTURAL'}]
  };
  await p.event('refresh', 'click');
  assert.equal(p.get('map-parents').hidden, false);
  assert.match(p.get('map-parent-list').textContent, /src/);
  assert.equal(p.get('map-children').hidden, true);
  assert.equal(p.get('map-children').children.length, 0);
});

test('inline project map separates ownership children from dependency chips with directional arrows', async () => {
  const p = await panel(['one'], {}, {locale:'en', tools:{oms_panel_project_graph:() => ({
    schema:1, kind:'oms-panel-project-graph', status:'ready', provenance:{source:'indexed-graph'},
    scope:{focusNode:'dir:src'},
    nodes:[
      {id:'dir:src', label:'src', kind:'directory'},
      {id:'file:src/lib.rs', label:'lib.rs', kind:'file'},
      {id:'file:core.rs', label:'core.rs', kind:'file'},
      {id:'file:caller.rs', label:'caller.rs', kind:'file'}
    ],
    edges:[
      {source:'dir:src', target:'file:src/lib.rs', relation:'contains', confidence:'STRUCTURAL'},
      {source:'dir:src', target:'file:core.rs', relation:'imports', confidence:'EXTRACTED'},
      {source:'file:caller.rs', target:'dir:src', relation:'calls', confidence:'EXTRACTED'}
    ]
  })}});
  assert.equal(p.get('map-children').className, 'map-row connected');
  assert.match(p.get('map-children').textContent, /lib\.rs/);
  assert.doesNotMatch(p.get('map-children').textContent, /core\.rs|caller\.rs/);
  assert.equal(p.get('map-deps').hidden, false);
  assert.match(p.get('map-dep-list').textContent, /→core\.rsImports · Extracted/);
  assert.match(p.get('map-dep-list').textContent, /←caller\.rsCalls · Extracted/);
});

test('degraded status renders a calm notice with source attribution and role=status; header shows project once', async () => {
  const p = await panel(['one', 'two'], {
    degraded:true,
    observation:{schema:1, kind:'multi-source-observation', atomic:false, revision:'obs-v1:0123456789abcdef',
      observedAt:'2026-11-24T10:00:00Z', completeness:'partial',
      sources:{git:'ok', task:'ok', plan:'stale', attempts:'ok', artifacts:'unavailable', acceptance:'ok', conversation:'unconfirmed'}}
  });
  p.get('project-choice').value = 'one'; await p.event('project-choice', 'change');
  assert.equal(p.get('scope').hidden, false);
  assert.equal(p.get('project').hidden, true);
  assert.equal(p.get('status').className, 'degraded');
  assert.match(html, /<p id="status" role="status">/);
  assert.match(p.get('status').textContent, /^~ 일부 · 계획 \(만료된 기록\), 결과물 \(미확인\)/);
  await choose(p, 'en');
  assert.match(p.get('status').textContent, /^~ Partial · Plan \(Stale record\), Artifacts \(Unknown\)/);
});

test('CSS design contracts: control-border token, no suppressed focus outline, forced-colors, target sizes, wrap tabs, single-line checks', () => {
  // Control border token defined in both schemes and applied to buttons, selects, and inputs.
  assert.match(styleCss, /:root\{[^}]*--control-border:#767e87/);
  assert.match(styleCss, /prefers-color-scheme:dark\)\{:root\{[^}]*--control-border:#6b7280/);
  assert.match(styleCss, /button\{[^}]*border:1px solid var\(--control-border\)/);
  assert.match(styleCss, /select\{[^}]*border:1px solid var\(--control-border\)/);
  assert.match(styleCss, /input:not\(\[type="radio"\]\):not\(\[type="checkbox"\]\),textarea,\.form-input\{[^}]*border:1px solid var\(--control-border\)/);
  // Never suppress focus outline; keep :focus-visible and forced-colors support.
  assert.doesNotMatch(styleCss, /outline\s*:\s*(?:none|0)\b/);
  assert.match(styleCss, /:focus-visible\{outline:2px solid var\(--brand\)/);
  assert.match(styleCss, /@media \(forced-colors:active\)/);
  // Target heights: summary >= 36px (2.25rem), header controls >= 32px (2rem).
  assert.match(styleCss, /details>summary\{[^}]*min-height:2\.25rem/);
  assert.match(styleCss, /#refresh\{[^}]*min-height:2rem/);
  assert.match(styleCss, /#language-choice\{[^}]*min-height:2rem/);
  assert.match(styleCss, /#project-choice\{[^}]*min-height:2rem/);
  // Tabs wrap instead of silently clipping; .entry stays two columns at 320px.
  assert.match(styleCss, /\.tabs\{[^}]*flex-wrap:wrap/);
  assert.doesNotMatch(styleCss, /scrollbar-width\s*:\s*none/);
  assert.match(styleCss, /\.entry\{[^}]*grid-template-columns:minmax\(0,1fr\) auto/);
  const narrowMedia = styleCss.match(/@media \(max-width:28rem\)\{([^}]*(?:\{[^}]*\}[^}]*)*)\}/)?.[1] || '';
  assert.doesNotMatch(narrowMedia, /\.entry\b/);
  // Degraded notice style exists; progress vendor pseudo-elements stay in two separate rules.
  assert.match(styleCss, /#status\.degraded\{[^}]*border-left:3px solid var\(--warm\)/);
  assert.doesNotMatch(styleCss, /::-webkit-progress-value\s*,\s*progress::-moz-progress-bar/);
  assert.match(styleCss, /progress::-webkit-progress-value\{[^}]*background:var\(--brand\)/);
  assert.match(styleCss, /progress::-moz-progress-bar\{[^}]*background:var\(--brand\)/);
});
