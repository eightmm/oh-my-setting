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
  const host = {status:null, failStart:true};
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
  window.parent = {postMessage({id, method, params}) {
    if (id == null) return;
    let result = {};
    if (method === 'tools/call') {
      try { result = {structuredContent:tool(params.name, params.arguments)}; }
      catch (error) { result = {isError:true, content:[{type:'text', text:error.message}]}; }
    }
    queueMicrotask(() => notify({id, result}));
  }};
  const document = {getElementById:get, createElement:() => new Element()};
  runInNewContext(script, {window, document, Option:Element, setTimeout, clearTimeout, crypto:{randomUUID}});
  await setImmediate();
  return {get, calls, host,
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
    scope:'repository', currentRoom:'room-one', active:2,
    activeAttempts:[
      {provider:'claude', role:'lead', task:'live-task', room:'room-two', state:'working'},
      {provider:'codex', role:'advisor', tool:'peer-review', state:'working'}
    ],
    attempts:[{provider:'codex', role:'lead', task:'old-task', room:'room-one', state:'done'}],
    acceptance:{available:true, verified:1, total:3, complete:false},
    reviews:{available:true, passed:2, failed:1, unknown:0},
    operations:[{kind:'verify', artifactId:'check-17', status:'failed', exit:1}]
  });
  const cards = p.get('leads').textContent;
  assert.match(cards, /Claude Leadworking · live-task · room-two/);
  assert.match(cards, /Codex Lead진행 중인 Lead 없음/);
  assert.doesNotMatch(cards, /old-task/);
  assert.match(p.get('activity').textContent, /old-task · room-one/);
  assert.match(p.get('room').textContent, /room-one/);
  assert.match(p.get('criteria').textContent, /1 \/ 3개 · 미완료/);
  assert.match(p.get('reviews').textContent, /보관된 리뷰 기록 · 통과 2 · 실패 1 · 판정 없음 0/);
  assert.match(p.get('operations').textContent, /verify · check-17failed · 종료 코드 1/);
});

test('unbound partial snapshots leave unseen activity and absent verification unknown', async () => {
  const p = await panel(['one'], {active:9, activeAttempts:[], currentRoom:null,
    acceptance:{available:false}, reviews:{available:false}, degraded:true});
  assert.match(p.get('leads').textContent, /진행 여부 미확인/);
  assert.match(p.get('room').textContent, /작업방 미확인/);
  assert.match(p.get('status').textContent, /일부 상태/);
  assert.match(p.get('criteria').textContent, /완료 기준 미확인/);
  assert.match(p.get('reviews').textContent, /리뷰 기록 미확인/);
  assert.match(p.get('operations').textContent, /결과물 미확인/);
});
