// Run with node --test crates/oms-mcp-server/tests/conversation-panel.test.mjs.
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { setImmediate } from 'node:timers/promises';
import test from 'node:test';
import { runInNewContext } from 'node:vm';

const html = readFileSync(new URL('../src/conversation-panel.html', import.meta.url), 'utf8');
const script = html.match(/<script>([\s\S]+?)<\/script>/)[1];

class Element extends EventTarget {
  value = '';
  children = [];
  get textContent() { return this.children.map(child => child?.textContent ?? child).join(''); }
  set textContent(value) { this.children = [String(value ?? '')]; }
  add() {}
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  querySelectorAll() { return []; }
  setCustomValidity() {}
}

async function panel(projects, dashboard = {}) {
  const elements = new Map([...html.matchAll(/\bid="([^"]+)"/g)].map(([, id]) => [id, new Element()]));
  const get = id => elements.get(id);
  get('launch').elements = {main:{value:'codex'}};
  get('launch').reset = () => { get('brief').value = ''; };
  const window = new EventTarget();
  const calls = [];
  const host = {status:null, failStart:true, replies:[]};
  function notify(payload) {
    window.dispatchEvent(Object.assign(new Event('message'), {
      source:window.parent, origin:'https://oms.example', data:{jsonrpc:'2.0', ...payload}
    }));
  }
  function tool(name, args) {
    calls.push({name, args});
    switch (name) {
      case 'oms_list_projects': return {projects};
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
    if (id == null) return;
    let result = {};
    if (method === 'tools/call') {
      try { result = {structuredContent:tool(params.name, params.arguments)}; }
      catch (error) { result = {isError:true, content:[{type:'text', text:error.message}]}; }
      if (params.name === 'oms_panel_snapshot' && host.delay === params.arguments.project && host.delay) {
        host.delay = null;
        host.delayedId = id;
        host.release = () => notify({id, result});
        return;
      }
    }
    queueMicrotask(() => notify({id, result}));
  }};
  const document = {getElementById:get, createElement:() => new Element()};
  runInNewContext(script, {window, document, Option:Element, setTimeout, clearTimeout, crypto:{randomUUID}});
  await setImmediate();
  return {get, calls, host, notify,
    async event(id, type) {
      get(id).dispatchEvent(new Event(type, {cancelable:true}));
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
  assert.match(cards, /Claude Leadworking · live-task/);
  assert.doesNotMatch(cards, /Codex Lead|old-task|unrelated/);
  assert.match(p.get('activity').textContent, /old-task · room-one/);
  assert.equal(p.get('room').textContent, 'room-one');
  assert.equal(p.get('participant').textContent, 'chat-owner');
  assert.equal(p.get('current-main').textContent, 'Codex Main');
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
  assert.equal(p.get('current-main').textContent, 'Codex Main');
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
