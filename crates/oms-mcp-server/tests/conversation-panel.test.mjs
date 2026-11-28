// Run with node --test crates/oms-mcp-server/tests/conversation-panel.test.mjs.
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
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
  style = {};
  scrollTop = 0;
  get scrollHeight() { return this.children.length; }
  get textContent() { return this.children.map(child => child?.textContent ?? child).join(''); }
  set textContent(value) { this.children = [String(value ?? '')]; }
  add(child) { this.children.push(child); }
  append(...children) { this.children.push(...children); }
  prepend(...children) { this.children.unshift(...children); }
  setAttribute(name,value) { this.attributes.set(name,value); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  removeAttribute(name) { this.attributes.delete(name); }
  focus() { this.focused = (this.focused ?? 0) + 1; }
  select() { this.selected = true; }
  replaceChildren(...children) { this.children = children; }
  querySelectorAll() { return []; }
  setCustomValidity() {}
  reportValidity() {}
  showModal() { this.open=true; }
  close() { this.open=false; this.dispatchEvent(new Event('close')); }
}

// Script-created nodes keep their tag and become reachable by id once the script names them.
function createdElement(tag, registry) {
  const element = new Element();
  element.tagName = tag;
  Object.defineProperty(element, 'id', {set(id) {
    assert.ok(!registry.has(id), 'duplicate id ' + id);
    registry.set(id, element);
  }});
  return element;
}

async function panel(projects, dashboard = {}, options = {}) {
  const elements = new Map([...html.matchAll(/\bid="([^"]+)"/g)].map(([, id]) => [id, new Element()]));
  const get = id => elements.get(id);
  get('launch').elements = {main:{value:'codex'}};
  get('launch').reset = () => { get('brief').value = ''; };
  const window = new EventTarget();
  window.scrollTo = (_x,y) => { window.scrollY = y; };
  const calls = [];
  const host = {modes:[],contexts:[],status:null, failStart:true, replies:[], links:[], messages:[], notices:[], tools:options.tools, toolError:options.toolError};
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
      case 'oms_lead_conversation_status': return leadStatus(args);
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
    if (id == null) {
      host.notices.push(method);
      if (method === 'ui/notifications/initialized' && options.initialOpen) {
        for (const [kind, openParams] of options.initialOpen) queueMicrotask(() => notify({method:'ui/notifications/tool-' + kind, params:openParams}));
      }
      return;
    }
    let result = {};
    if (method === 'ui/initialize') {
      host.initialize = params;
      result = {hostInfo:{name:options.host || 'Codex',version:options.version},hostContext:options.hostContext??{displayMode:options.mode,availableDisplayModes:options.modes,locale:options.locale},hostCapabilities:{openLinks:options.openLinks ? {} : undefined,message:options.message ? {text:{}} : undefined,experimental:options.compose ? {"openai/message":{}} : {},updateModelContext:options.context ? {text:{},structuredContent:{}} : undefined}};
    }
    if (method === 'ui/request-display-mode') {
      host.modes.push(params.mode);
      if (options.denyMode) { queueMicrotask(() => notify({id,error:{code:-32000,message:'denied'}})); return; }
      if (options.holdMode) { host.releaseMode=()=>notify({id,result:{mode:params.mode}}); return; }
      result = {mode:params.mode};
    }
    if(method==='ui/update-model-context'){host.contexts.push(params);result={isError:host.rejectContext===true};}
    if (method === 'ui/open-link') host.links.push(params.url);
    if (method === 'ui/message') {
      host.messages.push(params);
      if (host.holdMessage) {host.finishMessage=ack=>notify({id,...ack});return;}
      if (host.messageError) { queueMicrotask(() => notify({id,error:host.messageError})); return; }
      result = {isError:host.rejectMessage === true};
    }
    if (method === 'tools/call') {
      const rpcError = typeof host.toolError === 'function' ? host.toolError(params) : host.toolError;
      if (rpcError) { queueMicrotask(() => notify({id, error:rpcError})); return; }
      try {
        const value = tool(params.name, params.arguments);
        if(['oms_worker_output','oms_lead_settings','oms_update_lead_settings'].includes(params.name))result=value;
        else result = ['oms_lead_conversation_status','oms_read_lead_stream'].includes(params.name) && value._private ? {_meta:{leadConversation:value._private},structuredContent:{projectKey:value.projectKey,room:value.room,leads:value.leads,turn:value.turn}} : params.name === 'oms_panel_chat' ? {_meta:{chat:value}, structuredContent:{count:0,...(value.projectKey?{projectKey:value.projectKey}:{})}} : {structuredContent:value};
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
  const root=new Element(), colors=new Map();
  root.style={setProperty:(k,v)=>colors.set(k,v),removeProperty:k=>colors.delete(k),getPropertyValue:k=>colors.get(k)??''};
  const document = {documentElement:root,getElementById:get, createElement:tag => createdElement(tag, elements), title:'',
    querySelectorAll: selector => staticNodes.filter(node => node.attributes.has(selector.slice(1, -1)))};
  class Option extends Element { tagName = 'option'; constructor(text = '', value = text) { super(); this.textContent = text; this.value = value; } }
  // Opt-in only: the default document stays plain so the panel's optional calls stay exercised.
  if (options.visibility) {
    const events = new EventTarget();
    Object.assign(document, {hidden:false, addEventListener:(...args) => events.addEventListener(...args),
      show(visible) { document.hidden = !visible; events.dispatchEvent(new Event('visibilitychange')); }});
  }
  const cssSupports=(k,v)=>typeof v==='string' && (k==='color'?/^#[0-9a-f]{6}$/i.test(v):
    k==='border-radius'?v.trim().split(/\s+/).every((p,i)=>i<4 && /^(?:0|(?:\d+|\d*\.\d+)(?:px|r?em|%))$/i.test(p)):
    k==='font-family' && v.split(',').every(f=>/^(?:"[^"]+"|'[^']+'|[a-z_-][a-z0-9_-]*(?:\s+[a-z_-][a-z0-9_-]*)*)$/i.test(f.trim())));
  const context = createContext({window, document, Option, TextEncoder, CSS:{supports:cssSupports}, setTimeout, clearTimeout,
    crypto:{randomUUID,subtle:{async digest(algorithm,bytes){assert.equal(algorithm,'SHA-256');return createHash('sha256').update(bytes).digest();}}},
    ...options.timers, ...(options.navigator && {navigator:options.navigator}), ...(options.storage && {localStorage:options.storage})});
  runInContext(script, context);
  await setImmediate();
  return {get, elements, calls, host, notify, staticNodes, document, window, run: expression => runInContext(expression, context),
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

test('goal disclosure follows actual overflow and keeps focus when the collapsed goal fits', async () => {
  const p = await panel(['one'], {goal:'A measured project goal'});
  const text=p.get('goal'), toggle=p.get('goal-expand');
  Object.defineProperty(text,'scrollHeight',{value:100});
  text.clientHeight=56;
  p.run('measureGoal()');
  assert.equal(toggle.hidden,false);
  await p.event('goal-expand','click');
  text.clientHeight=100;
  p.run('measureGoal()');
  assert.equal(toggle.hidden,false);
  assert.equal(toggle.getAttribute('aria-expanded'),'true');
  p.document.activeElement=toggle;
  await p.event('goal-expand','click');
  assert.equal(toggle.getAttribute('aria-expanded'),'false');
  assert.equal(toggle.hidden,true);
  assert.equal(text.focused,1);
  p.document.activeElement=p.get('refresh');
  p.run('measureGoal()');
  assert.equal(text.focused,1);
});

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
    if (status === 'recorded') assert.match(p.get('receipt').textContent, /Main 시작이 확인/);
  });
}

test('Main launch uses all selected Lead preferences only when the user submits', async () => {
  const p = await panel(['one']);
  const mark = p.calls.length;
  p.get('launch').elements.main.value='claude';
  for (const provider of ['codex','claude','agy']) {
    p.get('lead-' + provider).checked = true;
    await p.event('lead-' + provider,'change');
  }
  assert.equal(p.calls.length,mark);
  p.host.failStart=false;
  p.get('brief').value='Check the release.';
  await p.event('launch','submit');
  const request=p.calls.find(c=>c.name==='oms_start_main').args;
  assert.equal(request.main,'claude');
  assert.equal(request.goal,'Requested provider Leads: Codex, Claude, AGY. Check the release.');
});

for (const edit of ['none','whitespace','brief','leads','main']) {
  test('recorded status reconciles the original launch with Lead preferences after ' + edit + ' edit', async () => {
    const p = await panel(['one']);
    const brief = p.get('brief'), lead = p.get('lead-claude');
    brief.value = 'Review the change'; lead.checked = true;
    await p.event('launch', 'submit');
    const starts = () => p.calls.filter(c => c.name === 'oms_start_main');
    assert.equal(starts().length,1);
    assert.equal(starts()[0].args.goal,'Requested provider Leads: Claude. Review the change');
    assert.equal(brief.value,'Review the change', 'unconfirmed write preserves its input');
    if (edit === 'whitespace') brief.value = '  Review the change  ';
    if (edit === 'brief') brief.value = 'A different draft';
    if (edit === 'leads') lead.checked = false;
    if (edit === 'main') p.get('launch').elements.main.value = 'pi';
    const draft = brief.value;
    p.host.status = 'recorded';
    await p.event('refresh','click');
    assert.equal(p.calls.filter(c=>c.name==='oms_start_status').at(-1).args.requestId,starts()[0].args.requestId);
    assert.match(p.get('receipt').textContent,/Main 시작이 확인/);
    assert.equal(brief.value,['none','whitespace'].includes(edit) ? '' : draft);
    if (['none','whitespace'].includes(edit)) await p.event('launch', 'submit');
    assert.equal(starts().length,1);
  });
}

for (const status of ['recorded','reserved','unknown','not_found']) {
  test('a distinct submission reconciles the original ' + status + ' request before replacing it', async () => {
    const p = await ambiguousStart();
    const starts = () => p.calls.filter(c => c.name === 'oms_start_main');
    const old = starts()[0].args.requestId;
    const brief = p.get('brief'); brief.value = 'A different assignment';
    p.host.status = status;
    const mark = p.calls.length;
    await p.event('launch','submit');
    assert.equal(p.calls.slice(mark)[0].name,'oms_start_status');
    assert.equal(p.calls.slice(mark)[0].args.requestId,old);
    assert.equal(starts().length,1);
    assert.equal(brief.value,'A different assignment');
    if (status === 'not_found') {
      // Changing the proposed replacement needs a fresh confirmation of that draft.
      brief.value = 'Another replacement'; await p.event('launch','submit');
      assert.equal(starts().length,1);
      // A subsequent ambiguous check must disarm the earlier replacement.
      p.host.status='unknown'; await p.event('launch','submit');
      assert.equal(starts().length,1);
      p.host.status='not_found'; await p.event('launch','submit');
      assert.equal(starts().length,1);
    }
    p.host.failStart = false;
    await p.event('launch','submit');
    const allowed = ['recorded','not_found'].includes(status);
    assert.equal(starts().length, allowed ? 2 : 1);
    if (allowed) assert.notEqual(starts().at(-1).args.requestId, old);
    else assert.equal(p.run('retryStart.requestId'), old);
  });
}

test('returning to the original unreserved request keeps its UUID', async () => {
  const p = await ambiguousStart();
  const starts = () => p.calls.filter(c=>c.name === 'oms_start_main');
  const original = starts()[0].args;
  p.host.status = 'not_found';
  p.get('brief').value='Changed'; await p.event('launch','submit');
  assert.equal(starts().length,1);
  p.get('brief').value=original.goal; p.host.failStart=false;
  await p.event('launch','submit');
  assert.deepEqual(starts().map(c=>c.args.requestId),[original.requestId,original.requestId]);
});

test('automatic reserved receipt clears the initial launch error style', async () => {
  const p = await ambiguousStart('reserved');
  assert.match(p.get('receipt').textContent,/같은 시작 요청이 접수/);
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

test('project registry distinguishes unbound setup from pinned, cwd and catalog reads', async () => {
  let registry={binding:'unbound',projects:[],readOnly:true};
  const p=await panel([],{}, {locale:'en',tools:{oms_list_projects:()=>registry}});
  assert.equal(p.get('view').hidden,true);
  assert.match(p.get('status').textContent,/No project linked/);
  await p.event('refresh','click');
  assert.ok(p.calls.every(c=>c.name==='oms_list_projects'));
  await choose(p,'ko'); assert.match(p.get('status').textContent,/연결된 프로젝트 없음/);
  for (const binding of ['cwd','pinned','catalog']) {
    registry={binding,projects:binding==='catalog'?['bound']:[],readOnly:true};
    await p.event('refresh','click');
    assert.equal(p.get('view').hidden,false);
    assert.deepEqual({...p.calls.filter(c=>c.name==='oms_panel_snapshot').at(-1).args}, binding==='catalog'?{project:'bound'}:{});
  }
  const mark=p.calls.length;
  registry={binding:'unbound',projects:[],readOnly:true}; await p.event('refresh','click');
  assert.equal(p.get('view').hidden,true); assert.equal(p.run('dashboardData'),undefined);
  assert.equal(p.get('project').textContent,'');
  assert.deepEqual(p.calls.slice(mark).map(c=>c.name),['oms_list_projects']);
  for (const invalid of [{binding:'new',projects:[]},{binding:'catalog',projects:[]}]) {
    registry=invalid;await p.event('refresh','click');assert.match(p.get('status').textContent,/OMS에 연결하지 못했습니다/);
  }
  assert.ok(!p.calls.some(c=>c.name==='oms_list_approvals'));
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


async function choose(p,lang){p.get('language-choice').value=lang;await p.event('language-choice','change');}
test('Main chat opens only the verified native owner conversation', async () => {
  const room='r', participant='owner';
  const uri='codex://threads/01234567-89ab-cdef-0123-456789abcdef';
  const p=await panel(['one','two'],{
    conversationBinding:'matched', conversationMain:{source:'room-session',state:'joined',provider:'codex',room,participant}
  },{openLinks:true,tools:{oms_panel_chat:args=>({...args,source:'room',provider:'codex',self:true,nativeUri:uri,messages:[]})}});
  await p.open('result',{structuredContent:{panel:'OMS',project:'one'}});
  assert.equal(p.get('header-main').hidden,false);
  await p.event('header-main','click');
  assert.deepEqual(p.host.links,[uri]);
  assert.equal(p.calls.filter(c=>c.name==='oms_panel_chat').length,1);
  p.host.tools.oms_panel_chat=args=>({...args,source:'room',provider:'codex',self:false,nativeUri:uri,messages:[]});
  await p.event('header-main','click');
  assert.deepEqual(p.host.links,[uri]);
  assert.equal(p.get('conversation').open,true);
  await p.event('chat-back','click');
  p.host.tools.oms_panel_chat=args=>({...args,source:'room',provider:'codex',self:true,nativeUri:uri,messages:[]});
  p.host.delayTool='oms_panel_chat';
  await p.event('header-main','click');
  p.get('project-choice').value='two';
  await p.event('project-choice','change');
  p.host.release();
  await setImmediate();
  assert.deepEqual(p.host.links,[uri]);
});

test('display mode follows host support, user choice and teardown independently of OMS writes', async()=>{
  const modes=['inline','pip','fullscreen'];
  const p=await panel(['one'],{}, {mode:'inline',modes});
  assert.deepEqual(p.host.modes,['fullscreen']);
  const button=p.get('display-mode');
  assert.equal(button.hidden,false);assert.match(button.textContent,/축소/);
  await p.event('display-mode','click');assert.deepEqual(p.host.modes,['fullscreen','inline']);
  await choose(p,'en');assert.equal(button.textContent,'Expand');
  p.notify({method:'ui/notifications/host-context-changed',params:{availableDisplayModes:['inline','pip']}});
  await p.event('display-mode','click');assert.equal(p.host.modes.at(-1),'pip');
  for (const options of [{mode:'inline'}, {mode:'inline',modes:['inline','unknown']}, {mode:'fullscreen',modes}]) {
    const plain=await panel(['one'],{},options);
    assert.deepEqual(plain.host.modes,[]);
    assert.equal(plain.get('display-mode').hidden,options.mode==='inline');
  }
  const denied=await panel(['one'],{},{mode:'inline',modes,denyMode:true});
  assert.deepEqual(denied.host.modes,['fullscreen']);
  assert.equal(denied.get('view').hidden,false);
  assert.equal(denied.get('display-mode').disabled,false);
  const closing=await panel(['one'],{},{mode:'inline',modes:['inline','pip'],holdMode:true});
  assert.deepEqual(closing.host.modes,['pip']);assert.equal(closing.get('display-mode').disabled,true);
  closing.notify({method:'ui/resource-teardown',id:'close-display',params:{}});
  closing.host.releaseMode();await setImmediate();
  await closing.event('display-mode','click');
  assert.equal(closing.get('display-mode').disabled,true);assert.deepEqual(closing.host.modes,['pip']);
  for (const opened of [p,denied,closing]) {
    assert.ok(opened.calls.every(c=>['oms_list_projects','oms_panel_snapshot','oms_list_approvals'].includes(c.name)));
    assert.equal(opened.host.messages.length,0);
  }
});


const main={source:'room-session',state:'joined',provider:'codex',harness:'codex',room:'room-a',participant:'owner',incomplete:false};
const lead={room:main.room,participant:'lead-one',parent:main.participant,role:'worker',provider:'claude',joined:true};
const dashboard={conversationBinding:'matched',conversationMain:{...main,participants:[lead]},
  conversationAttempts:[
    {room:main.room,id:'m',participant:'owner',role:'main',provider:'codex',state:'working',taskTitle:'Coordinate edits'},
    {room:main.room,id:'l',participant:lead.participant,role:'lead',provider:'claude',state:'blocked',taskTitle:'Check indexing'},
    {room:main.room,id:'delegated',parentAttempt:'m',tool:'panel-native-assignment',provider:'codex',state:'verifying',task:'opaque'},
    {room:main.room,id:'alien',parentAttempt:'old-main',role:'lead',provider:'pi',state:'running',taskTitle:'Foreign task'}
  ]};
const managedRead='oms_lead_conversation_status', managedOpen='oms_open_lead_conversations';
function leadStatus(args,status='not_started',projectKey='b'.repeat(64)){
  const leads=['codex','claude','agy'].map((provider,i)=>({provider,status,requestId:'12345678-1234-8234-9234-123456789ab'+i}));
  return {projectKey,room:args.room,leads:leads.filter(r=>(!args.provider || r.provider===args.provider) && (!args.providers || args.providers.includes(r.provider)))};
}
const startup=['oms_list_projects','oms_panel_snapshot','oms_list_approvals',managedRead];
test('host appearance follows initialization and partial updates without reloading the project',async()=>{
  const primary='--color-background-primary', ink='--color-text-primary';
  const p=await panel(['one'],dashboard,{hostContext:{theme:'dark',styles:{variables:{[primary]:'#272727',[ink]:'#ffffff','--other':'#123456'}}}});
  const root=p.document.documentElement, color=k=>root.style.getPropertyValue(k), calls=p.calls.length;
  assert.equal(root.getAttribute('data-theme'),'dark');assert.equal(color(primary),'#272727');assert.equal(color('--other'),'');
  await p.event('new-goal','click');const brief=p.get('brief');brief.value='Keep keyboard focus';p.document.activeElement=brief;
  const change=params=>p.notify({method:'ui/notifications/host-context-changed',params});
  change({theme:'light'});assert.equal(root.getAttribute('data-theme'),'light');assert.equal(color(ink),'#ffffff');
  change({styles:{variables:{[primary]:'url(https://invalid.example)',[ink]:'#222222'}}});
  assert.equal(color(primary),'');assert.equal(color(ink),'#222222');
  const tokens={'--border-radius-lg':'12px','--font-mono':'"Host Mono", monospace','--color-background-inverse':'#0d0d0d','--color-text-inverse':'#fafafa'};
  const unsafe={'--font-display':'Serif','--border-radius-md':'url(x)','--border-radius-sm':'1px;color:red','--font-sans':'x'.repeat(201),'--font-mono':5};
  change({styles:{variables:{...tokens,...unsafe,'--font-mono':'expression(alert(1))'}}});
  for(const k of Object.keys(unsafe))assert.equal(color(k),'',k);
  change({styles:{variables:{...tokens,...unsafe,'--font-mono':tokens['--font-mono']}}});
  for(const [k,v] of Object.entries(tokens))assert.equal(color(k),v);
  for(const prior of [false,true]){
    if(prior){change({styles:{variables:{...tokens,'--font-sans':'system-ui, sans-serif','--border-radius-md':'8px'}}});
      assert.equal(color('--font-sans'),'system-ui, sans-serif');assert.equal(color('--border-radius-md'),'8px');}
    change({styles:{variables:{...tokens,'--font-sans':'   ','--border-radius-md':'none'}}});
    assert.equal(color('--font-sans'),'');assert.equal(color('--border-radius-md'),'');
    for(const [k,v] of Object.entries(tokens))assert.equal(color(k),v);
  }
  change({styles:{variables:{...tokens,'--font-mono':5}}});assert.equal(color('--font-mono'),'');
  change({theme:null,styles:null});assert.equal(root.getAttribute('data-theme'),null);assert.equal(color(ink),'');assert.equal(color('--border-radius-lg'),'');
  assert.equal(p.calls.length,calls);assert.equal(brief.value,'Keep keyboard focus');assert.equal(p.document.activeElement,brief);
  const malformed=await panel(['one'],dashboard,{hostContext:'unrecognized'});
  assert.equal(malformed.document.documentElement.getAttribute('data-theme'),null);
  assert.deepEqual(malformed.calls.map(c=>c.name),startup);
});
test('the first screen shows proven participants and only requests registry, snapshot and actionable approvals',async()=>{
  const p=await panel(['one'],dashboard,{locale:'en'});
  assert.deepEqual(p.calls.map(c=>c.name),startup);
  assert.equal(p.host.initialize.appInfo.version,readFileSync(new URL('../../../VERSION',import.meta.url),'utf8').trim());
  assert.equal(p.get('main-name').textContent,'OMS Main · Codex');
  assert.match(p.get('main-status').textContent,/Working · Coordinate edits/);
  const cards=p.get('leads').children;
  assert.deepEqual(cards.map(c=>c.tagName),['section','section','section']);
  assert.deepEqual(cards.map(slot=>slot.children.find(e=>e.className==='slot-heading').children.find(e=>e.tagName==='h3').textContent),['OMS Lead · Codex','OMS Lead · Claude','OMS Lead · AGY']);
  assert.match(cards[2].textContent,/Waiting for assignment/);
  assert.equal(cards[0].children[2].tagName,'article');
  assert.match(cards[0].children[2].textContent,/Codex assignment.*Verifying/);
  assert.equal(cards[1].children[2].tagName,'button');
  assert.match(cards[1].children[2].textContent,/OMS Lead · Claude.*Check indexing.*Blocked.*View assignment record/);
  assert.doesNotMatch(p.get('leads').textContent,/opaque|Foreign task|lead-one/);
  assert.equal(p.get('approval-card').hidden,true);
  assert.equal(p.get('outcome').hidden,true);
  await choose(p,'ko');assert.match(cards[1].textContent,/차단됨.*배정 기록 보기/);assert.match(cards[2].textContent,/배정 대기/);
  assert.ok(!html.includes('role="tablist"'));
  for(const tool of ['oms_panel_project_graph','oms_panel_work_items','oms_panel_usage','oms_panel_capabilities','oms_panel_tui_view'])
    assert.ok(!p.calls.some(call=>call.name===tool),tool);
  const generic=await panel([],dashboard,{locale:'en',readOnly:true});
  assert.deepEqual(generic.calls.map(c=>c.name),startup.slice(0,2));
  assert.equal(generic.get('new-goal').hidden,true);assert.equal(generic.get('header-main').hidden,true);
  assert.equal(generic.get('leads').children.length,3);
  assert.ok(generic.get('leads').children.every(slot=>!slot.textContent.includes('Assignment unknown') && slot.children.length===2));
});
test('Main identity and conversation use the recorded harness without treating the dispatch owner as the harness',async()=>{
  const d={...dashboard,conversationMain:{...main,harness:'pi'}};
  const p=await panel(['one'],d,{locale:'en',tools:{oms_panel_chat:args=>({...args,source:'room',messages:[]})}});
  assert.equal(p.get('main-name').textContent,'OMS Main · Pi');
  assert.equal(p.get('avatar').getAttribute('data-provider'),'pi');
  await p.event('header-main','click');
  assert.equal(p.get('chat-title').textContent,'OMS Main · Pi');
  assert.deepEqual({...p.calls.at(-1).args},{room:main.room,participant:main.participant,project:'one'});
  for(const harness of [null,'unrecognized',undefined]){
    d.conversationMain.harness=harness;await p.event('refresh','click');
    assert.equal(p.get('main-name').textContent,'OMS Main · via Codex');
    assert.equal(p.get('chat-title').textContent,'OMS Main · via Codex');
    assert.equal(p.get('avatar').textContent,'–');
  }
  await choose(p,'ko');
  assert.equal(p.get('main-name').textContent,'OMS Main · Codex 연결');
  assert.equal(p.get('chat-title').textContent,p.get('main-name').textContent);
  assert.ok(p.calls.every(c=>[...startup,'oms_panel_chat'].includes(c.name)));
});
test('explicit control opens three managed Leads, preserves member chat and reads only the selected verified transcript',async()=>{
  let started=false,spoof=false;
  const p=await panel(['one'],dashboard,{locale:'en',tools:{
    [managedRead]:a=>{
      const v=leadStatus(a,started?'ready':'not_started');
      if(a.provider)v._private={...v.leads[0],room:spoof?'wrong':a.room,transcript:{turns:[{role:'claude',text:'Recorded review'}]}};
      return v;
    },
    [managedOpen]:a=>{started=true;assert.deepEqual({...a},{project:'one',room:main.room,projectKey:'b'.repeat(64)});return leadStatus(a,'ready');}
  }});
  assert.deepEqual(p.calls.map(c=>c.name),startup);
  await p.event('open-leads','click');
  assert.equal(p.calls.filter(c=>c.name===managedOpen).length,1);
  const claude=p.get('leads').children[1], opener=claude.children[1].children.find(e=>e.type==='button');
  assert.equal(opener.textContent,'Ask Claude');assert.ok(!opener.hidden);
  assert.equal(claude.children[2].tagName,'button');
  opener.dispatchEvent(new Event('click'));await setImmediate();
  assert.deepEqual({...p.calls.at(-1).args},{project:'one',room:main.room,provider:'claude'});
  assert.equal(p.get('chat-title').textContent,'OMS Lead · Claude');
  assert.match(p.get('chat-messages').textContent,/Recorded review/);
  await p.event('chat-back','click');assert.ok(opener.focused);
  spoof=true;opener.dispatchEvent(new Event('click'));await setImmediate();
  assert.equal(p.get('chat-messages').textContent,'');assert.match(p.get('chat-status').textContent,/Could not load/);
  await choose(p,'ko');assert.match(opener.textContent,/Claude에게 질문/);
  assert.equal(p.host.messages.length,0);
});
test('managed Lead composes privately, keeps scoped drafts and reconciles a lost acknowledgement with the same turn',async()=>{
  const send='oms_send_lead_message', older=randomUUID(), greeting={role:'claude',text:'Ready'};
  const turns=[greeting];let status='not_found', sent;
  const p=await panel(['one','two'],dashboard,{locale:'en',tools:{
    [managedRead]:a=>{
      const v=leadStatus(a,'ready');
      if(a.provider)v._private={room:a.room,...v.leads[0],transcript:{turns:a.before?[{role:'claude',text:'Earlier'}]:turns,
        cursor:a.before?null:older,hasMore:!a.before}};
      if(a.turnId)v.turn={turnId:a.turnId,status};
      return v;
    },
    [send]:a=>{
      if(!sent){sent={...a};throw Error('acknowledgement lost');}
      assert.deepEqual({...a},sent);
      status='replied';turns.push({role:'user',text:a.text,turnId:a.turnId},{role:'claude',text:'Here is the reply.',turnId:a.turnId});
      return {...leadStatus(a,'ready'),turn:{turnId:a.turnId,status}};
    }
  }});
  await p.open('input',{arguments:{project:'one'}});
  const open=async()=>{const b=p.get('leads').children[1].children[1].children.find(e=>e.type==='button');
    b.dispatchEvent(new Event('click'));await setImmediate();};
  const type=async value=>{p.get('chat-text').value=value;await p.event('chat-text','input');};
  await open();await type('Draft');await p.event('chat-back','click');await open();assert.equal(p.get('chat-text').value,'Draft');
  await p.event('chat-older','click');assert.equal(p.calls.at(-1).args.before,older);
  assert.match(p.get('chat-messages').textContent,/Earlier.*Ready/);
  for(const invalid of ['한'.repeat(1334),'text'+String.fromCharCode(0x85)]){await type(invalid);await p.event('chat-compose','submit');assert.ok(!sent);}
  await type('한'.repeat(1334));assert.equal(p.get('chat-count').textContent,'4,002 / 4,000 bytes');assert.equal(p.get('chat-count').getAttribute('data-tone'),'warn');
  await type('한'.repeat(1333));assert.equal(p.get('chat-count').textContent,'3,999 / 4,000 bytes');assert.equal(p.get('chat-send').disabled,false);
  await type('My private question');await p.event('chat-compose','submit');
  assert.deepEqual({...sent,turnId:''},{project:'one',room:main.room,projectKey:'b'.repeat(64),provider:'claude',
    leadRequestId:leadStatus({provider:'claude'},'ready').leads[0].requestId,turnId:'',text:'My private question'});
  assert.match(sent.turnId,/^[a-f0-9]{8}-[a-f0-9]{4}-4/);
  assert.match(p.get('chat-check').textContent,/Retry original/);
  await type('A new draft');await p.event('chat-compose','submit');
  assert.equal(p.calls.filter(c=>c.name===send).length,1);
  await p.event('chat-check','click');assert.equal(p.calls.filter(c=>c.name===send).length,2);
  assert.equal(p.get('chat-text').value,'A new draft');
  assert.match(p.get('chat-messages').textContent,/Earlier.*Ready.*YouMy private question.*ClaudeHere is the reply/);
  assert.equal(p.host.messages.length,0);assert.equal(p.host.contexts.length,0);
  await p.open('input',{arguments:{project:'two'}});
  assert.equal(p.get('chat-text').value,'');assert.equal(p.get('chat-messages').textContent,'');
  assert.ok(!p.get('conversation').open);
});

test('Lead settings are private and lazy, with model-specific effort and explicit revision-checked saves',async()=>{
  const read='oms_lead_settings',write='oms_update_lead_settings';
  let revision='1'.repeat(64),selection={model:null,effort:null},lost=false,conflict=false;
  const options=[{model:'configurable',source:'catalog',fixed:false,requiresEffort:false,efforts:['low','high']},
    {model:'select-level',source:'catalog',fixed:false,requiresEffort:true,efforts:['standard','deep']},
    {model:'fixed',source:'policy',fixed:true,requiresEffort:false,efforts:[]}];
  const response=args=>{
    const {project,...scope}=args;
    const data={projectKey:scope.projectKey,provider:scope.provider,room:scope.room,leadRequestId:scope.leadRequestId,revision};
    return {structuredContent:data,_meta:{leadSettings:{...data,options,selection,lastServed:null,nativeTitle:null,catalogAgeSeconds:120,
      scope:{target:'lead-conversation',appliesTo:'next-message',inheritsToWorkers:false,mode:'read-only-consultation',continuity:'oms-thread'},account:null,usage:null,failover:null}}};
  };
  const p=await panel(['one'],dashboard,{locale:'en',tools:{
    [managedRead]:a=>{const v=leadStatus(a,'ready');if(a.provider)v._private={room:a.room,...v.leads[0],transcript:{turns:[{role:'claude',text:'Earlier answer'}]}};return v;},
    [read]:response,
    [write]:a=>{assert.equal(a.revision,revision);revision=(revision[0]==='1'?'2':'3').repeat(64);
      selection=conflict?{model:'fixed',effort:null}:{model:a.model,effort:a.effort};
      if(lost || conflict)throw Error('Inconclusive response');return response(a);}
  }});
  const open=p.get('leads').children[1].children[1].children.find(e=>e.type==='button');open.dispatchEvent(new Event('click'));await setImmediate();
  assert.equal(p.calls.filter(c=>c.name===read).length,0);
  p.get('chat-text').value='Keep my draft';await p.event('chat-text','input');p.get('chat-scroll').scrollTop=72;
  await p.event('settings-toggle','click');
  assert.deepEqual({...p.calls.at(-1).args},{project:'one',provider:'claude',room:main.room,projectKey:'b'.repeat(64),leadRequestId:leadStatus({provider:'claude'},'ready').leads[0].requestId});
  assert.equal(p.get('settings-save').disabled,true);assert.match(p.get('settings-saved').textContent,/OMS routing default/);
  assert.match(p.get('settings-scope').textContent,/read-only consultation.*next message.*not inherited/);
  assert.match(p.get('settings-catalog').textContent,/120 seconds/);
  p.get('settings-model').value='fixed';await p.event('settings-model','change');assert.ok(p.get('settings-effort').disabled);
  p.get('settings-model').value='select-level';await p.event('settings-model','change');assert.equal(p.get('settings-save').disabled,true);
  assert.ok(!p.get('settings-effort').children.some(o=>o.value==='high'));
  p.get('settings-effort').value='deep';await p.event('settings-effort','change');
  const mark=p.calls.length;await p.event('chat-refresh','click');assert.deepEqual(p.calls.slice(mark).map(c=>c.name),[managedRead]);
  await choose(p,'ko');assert.equal(p.get('settings-model').value,'select-level');assert.equal(p.get('settings-effort').value,'deep');
  assert.equal(p.get('chat-text').value,'Keep my draft');assert.equal(p.get('chat-scroll').scrollTop,72);
  p.host.delayTool=write;await p.event('settings-save','click');await p.event('chat-compose','submit');await p.event('settings-save','click');
  assert.equal(p.calls.filter(c=>c.name===write).length,1);assert.equal(p.calls.filter(c=>c.name==='oms_send_lead_message').length,0);
  assert.equal(p.get('chat-send').disabled,true);p.host.release();await setImmediate();
  assert.ok(!p.get('chat-send').disabled);assert.match(p.get('settings-saved').textContent,/select-level · deep/);
  lost=true;p.get('settings-model').value='configurable';await p.event('settings-model','change');await p.event('settings-save','click');
  assert.match(p.get('settings-status').textContent,/일치/);assert.equal(p.calls.at(-1).name,read);
  conflict=true;p.get('settings-model').value='';await p.event('settings-model','change');await p.event('settings-save','click');
  assert.equal(p.get('settings-model').value,'fixed');assert.match(p.get('settings-status').textContent,/다시 불러왔습니다/);
  assert.equal(p.calls.filter(c=>c.name===write).length,3);
  assert.equal(p.get('chat-text').value,'Keep my draft');assert.match(p.get('chat-messages').textContent,/Earlier answer/);
  await p.event('settings-toggle','click');assert.equal(p.get('settings-toggle').getAttribute('aria-expanded'),'false');assert.ok(p.get('settings-toggle').focused);
  assert.equal(p.host.messages.length,0);assert.equal(p.host.contexts.length,0);
});

test('Lead settings show only private provider evidence for the observed request',async()=>{
  const id='531cb793-8e16-4761-82c8-064497585548',model='claude-opus-4-6';
  const observed={lastServed:{model,effort:null},nativeSession:{id,source:'provider-result',scope:'invocation',resumable:true}};
  let evidence=observed,turns=[];
  const p=await panel(['one'],dashboard,{locale:'en',tools:{
    [managedRead]:a=>{const v=leadStatus(a,'ready');if(a.provider)v._private={room:a.room,...v.leads[0],transcript:{turns}};return v;},
    oms_lead_settings:a=>{const {project,...scope}=a,data={...scope,revision:'b'.repeat(64)};
      return {structuredContent:{...data,...observed},_meta:{leadSettings:{...data,options:[],selection:{model:null,effort:null},...evidence}}};}
  }});
  const details=p.get('settings-actual');
  const open=async index=>{
    const button=p.get('leads').children[index].children[1].children.find(e=>e.type==='button');
    button.dispatchEvent(new Event('click'));await setImmediate();await p.event('settings-toggle','click');
  };
  await open(1);
  assert.ok(!details.hidden);assert.match(details.textContent,/Reply model reported by Claude · claude-opus-4-6\nThe next message continues this Claude session/);
  assert.ok(!p.get('chat-settings').textContent.includes(id));
  await choose(p,'ko');assert.match(details.textContent,/보고한 응답 모델 · claude-opus-4-6\n다음 메시지는 이 Claude 세션에서 이어짐/);
  turns=[{role:'user',turnId:id,text:'A later request',status:'failed'}];await p.event('chat-refresh','click');
  assert.ok(details.hidden);assert.ok(!p.get('settings-fields').hidden);
  await p.event('settings-toggle','click');
  evidence={lastServed:{model:'provider-default',effort:null},nativeSession:{...observed.nativeSession,resumable:false}};
  await p.event('settings-toggle','click');assert.ok(!details.hidden);assert.equal(details.textContent,'실제 사용 모델 미확인');
  await p.event('settings-toggle','click');evidence={};
  await p.event('settings-toggle','click');assert.ok(!details.hidden);assert.equal(details.textContent,'실제 사용 모델 미확인');
  await p.event('chat-back','click');evidence=observed;await open(2);
  assert.equal(details.textContent,'실제 사용 모델 미확인\n다음 메시지는 이 AGY 세션에서 이어짐');assert.ok(!details.hidden);
  await p.event('chat-back','click');await open(0);assert.ok(!details.hidden);assert.equal(details.textContent,'실제 사용 모델 미확인\n다음 메시지는 이 Codex 세션에서 이어짐');
  assert.equal(p.host.messages.length,0);assert.equal(p.host.contexts.length,0);
});

test('settings refuse foreign receipts and discard late reads on project changes',async()=>{
  const read='oms_lead_settings',write='oms_update_lead_settings';let spoof=true;
  const p=await panel(['one','two'],dashboard,{locale:'en',tools:{
    [managedRead]:a=>{const v=leadStatus(a,'ready');if(a.provider)v._private={room:a.room,...v.leads[0],transcript:{turns:[]}};return v;},
    [read]:a=>{const {project,...scope}=a;const data={...scope,revision:'a'.repeat(64)};
      return {structuredContent:data,_meta:{leadSettings:{...data,room:spoof?'foreign':a.room,selection:{model:null,effort:null},options:[],lastServed:null,nativeTitle:null}}};}
  }});
  await p.open('input',{arguments:{project:'one'}});
  const open=p.get('leads').children[0].children[1].children.find(e=>e.type==='button');open.dispatchEvent(new Event('click'));await setImmediate();
  await p.event('settings-toggle','click');assert.match(p.get('settings-status').textContent,/could not be verified/);
  await p.event('settings-save','click');assert.ok(!p.calls.some(c=>c.name===write));
  spoof=false;p.host.delayTool=read;await p.event('settings-read','click');
  await p.open('input',{arguments:{project:'two'}});p.host.release();await setImmediate();
  assert.ok(!p.get('conversation').open);assert.ok(p.get('chat-settings').hidden);assert.equal(p.get('settings-saved').textContent,'');
  const q=await panel(['one'],dashboard,{readOnly:true});await q.event('settings-toggle','click');assert.ok(!q.calls.some(c=>c.name===read || c.name===write));
});

test('recorded Lead turns poll reads only while open and keep a draft typed while sending',async()=>{
  let state='reserved',sent;const timers=new Map();
  const p=await panel(['one'],dashboard,{locale:'en',timers:{
    setTimeout(fn,ms){const id=Symbol();timers.set(id,{fn,ms});return id;},clearTimeout(id){timers.delete(id);}
  },tools:{
    [managedRead]:a=>{
      const v=leadStatus(a,'ready');
      if(a.provider)v._private={room:a.room,...v.leads[0],transcript:{turns:[]}};
      if(a.turnId)v.turn={turnId:a.turnId,status:state};
      return v;
    },
    oms_send_lead_message:a=>{sent={...a};return {...leadStatus(a,'ready'),turn:{turnId:a.turnId,status:state}};}
  }});
  const polls=()=>[...timers.values()].filter(v=>v.ms===4000);
  const open=p.get('leads').children[2].children[1].children.find(e=>e.type==='button');open.dispatchEvent(new Event('click'));await setImmediate();
  p.host.delayTool='oms_send_lead_message';p.get('chat-text').value='First';await p.event('chat-text','input');
  await p.event('chat-compose','submit');await p.event('chat-compose','submit');
  p.get('chat-text').value='Next';await p.event('chat-text','input');p.host.release();await setImmediate();
  assert.equal(sent.provider,'agy');assert.equal(p.get('chat-text').value,'Next');
  assert.equal(polls().length,1);assert.match(p.get('chat-delivery').textContent,/awaiting execution/);
  state='running';polls()[0].fn();await setImmediate();assert.match(p.get('chat-delivery').textContent,/Waiting for the reply/);
  const stale=polls()[0];await p.event('chat-back','click');const mark=p.calls.length;stale.fn();await setImmediate();assert.equal(p.calls.length,mark);
  state='failed';open.dispatchEvent(new Event('click'));await setImmediate();assert.match(p.get('chat-delivery').textContent,/ended without a reply/);
  assert.equal(p.get('chat-text').value,'Next');assert.equal(polls().length,0);
  assert.equal(p.calls.filter(c=>c.name==='oms_send_lead_message').length,1);
});

test('a Lead reply renders as Markdown nodes, Enter sends, and Stop ends a running reply',async()=>{
  const id=randomUUID();let state='running',sent,stopped;
  const reply='## Plan\nUse `oms land` **now**.\n- one\n- two\n```\n<b>raw</b>\n```';
  const p=await panel(['one'],dashboard,{locale:'en',timers:{setTimeout:()=>0,clearTimeout(){}},tools:{
    [managedRead]:a=>{const v=leadStatus(a,'ready');
      if(a.provider)v._private={room:a.room,...v.leads[0],transcript:{turns:[{role:'user',text:'Old',turnId:id,status:'replied'},{role:'antigravity',text:reply,turnId:id}]}};
      if(a.turnId)v.turn={turnId:a.turnId,status:state};return v;},
    oms_send_lead_message:a=>{sent={...a};return {...leadStatus(a,'ready'),turn:{turnId:a.turnId,status:state}};},
    oms_stop_lead_message:a=>{stopped={...a};state='stopped';return {...leadStatus(a,'ready'),turn:{turnId:a.turnId,status:state}};}
  }});
  p.get('leads').children[2].children[1].children.find(e=>e.type==='button').dispatchEvent(new Event('click'));await setImmediate();
  const md=kids(kids(p.get('chat-messages'))[1])[1];
  assert.deepEqual(kids(md).map(e=>e.tagName.toLowerCase()),['p','p','ul','pre']);
  assert.equal(md.textContent,'PlanUse oms land now.onetwo<b>raw</b>');assert.ok(!kids(md).some(e=>/^b$/i.test(e.tagName)));
  assert.equal(kids(kids(md)[1]).map(e=>e.tagName?.toLowerCase()).filter(Boolean).join(),'code,strong');
  assert.match(p.get('chat-text').placeholder,/Message AGY · Enter to send/);
  const key=async extra=>{p.get('chat-text').dispatchEvent(Object.assign(new Event('keydown',{cancelable:true}),{key:'Enter',...extra}));await setImmediate();};
  p.get('chat-text').value='Go';await p.event('chat-text','input');
  await key({shiftKey:true});assert.equal(sent,undefined);
  await key({});assert.equal(sent.text,'Go');
  assert.ok(!p.get('chat-typing').hidden);assert.match(p.get('chat-typing-text').textContent,/AGY is replying/);
  assert.ok(!p.get('chat-stop').hidden);assert.ok(p.get('chat-send').hidden);
  await p.event('chat-stop','click');
  assert.equal(stopped.turnId,sent.turnId);assert.equal(stopped.leadRequestId,sent.leadRequestId);
  assert.ok(p.get('chat-typing').hidden);assert.ok(p.get('chat-stop').hidden);assert.ok(!p.get('chat-send').hidden);
  assert.match(p.get('chat-delivery').textContent,/Stopped/);
  assert.ok(!p.calls.some(c=>c.name==='oms_read_lead_stream'));
});

test('a running Claude reply streams scanned partial Markdown and a failed stream read keeps the turn',async()=>{
  let state='running',reads=[],fail=false;const timers=new Map(),log=['Partial **one**\n','more\n'];
  const p=await panel(['one'],dashboard,{locale:'en',timers:{
    setTimeout(fn,ms){const id=Symbol();timers.set(id,{fn,ms});return id;},clearTimeout(id){timers.delete(id);}
  },tools:{
    [managedRead]:a=>{const v=leadStatus(a,'ready');
      if(a.provider)v._private={room:a.room,...v.leads[0],transcript:{turns:state==='replied'?[{role:'user',text:'Go',turnId:a.turnId,status:state},{role:'claude',text:'Final answer',turnId:a.turnId}]:[]}};
      if(a.turnId)v.turn={turnId:a.turnId,status:state};return v;},
    oms_send_lead_message:a=>({...leadStatus(a,'ready'),turn:{turnId:a.turnId,status:state}}),
    oms_read_lead_stream:a=>{reads.push(a.cursor);if(fail)throw new Error('down');const i=a.cursor?1:0;
      return {...leadStatus(a,'ready'),_private:{room:a.room,...leadStatus(a,'ready').leads[0],stream:{turnId:a.turnId,status:state,text:log[i]||'',tool:i?null:'Read AGENTS.md',end:null,cursor:a.cursor+10}}};}
  }});
  p.get('leads').children[1].children[1].children.find(e=>e.type==='button').dispatchEvent(new Event('click'));await setImmediate();
  p.get('chat-text').value='Go';await p.event('chat-text','input');await p.event('chat-compose','submit');await setImmediate();
  const partial=p.get('chat-partial'),streams=()=>[...timers.values()].filter(v=>v.ms===1200);
  assert.deepEqual(reads,[0]);assert.ok(!partial.hidden);assert.match(partial.textContent,/Partial one/);
  assert.ok(kids(kids(partial)[0])[0].children.some(e=>e.tagName?.toLowerCase()==='strong'));
  assert.match(p.get('chat-typing-text').textContent,/Claude · Read AGENTS\.md/);
  streams()[0].fn();await setImmediate();assert.deepEqual(reads,[0,10]);assert.match(partial.textContent,/Partial one\s*more/);
  fail=true;streams()[0].fn();await setImmediate();
  assert.ok(!p.get('chat-typing').hidden);assert.ok(!p.get('chat-compose').hidden);assert.doesNotMatch(p.get('chat-status').textContent,/mismatch|could not/i);
  fail=false;state='replied';streams()[0].fn();await setImmediate();await setImmediate();
  assert.ok(partial.hidden);assert.ok(p.get('chat-typing').hidden);assert.match(p.get('chat-messages').textContent,/Final answer/);assert.equal(streams().length,0);
});

test('a verified unresolved message from another view blocks a new dispatch until its turn settles',async()=>{
  const id=randomUUID();let state='unconfirmed';
  const p=await panel(['one'],dashboard,{locale:'en',tools:{[managedRead]:a=>{
    const v=leadStatus(a,'ready');
    if(a.provider)v._private={room:a.room,...v.leads[0],transcript:{turns:[{role:'user',text:'From another view',turnId:id,status:state},
      ...(state==='replied'?[{role:'antigravity',text:'A bounded answer',turnId:id,truncated:true}]:[])]}};
    return v;
  }}});
  const button=p.get('leads').children[2].children[1].children.find(e=>e.type==='button');button.dispatchEvent(new Event('click'));await setImmediate();
  p.get('chat-text').value='My draft';await p.event('chat-text','input');await p.event('chat-compose','submit');
  assert.equal(p.get('chat-send').disabled,true);assert.match(p.get('chat-delivery').textContent,/previous message/);
  state='replied';await p.event('chat-check','click');assert.equal(p.get('chat-send').disabled,false);
  assert.match(p.get('chat-messages').textContent,/AGY.*A bounded answer.*Partial reply/);
  assert.equal(p.get('chat-text').value,'My draft');assert.ok(!p.calls.some(c=>c.name==='oms_send_lead_message'));
});

test('a wrong receipt or late reply cannot authorize a fresh send or cross conversations',async()=>{
  const send='oms_send_lead_message';let sent=0;
  const p=await panel(['one','two'],dashboard,{tools:{
    [managedRead]:a=>{
      const v=leadStatus(a,'ready');
      if(a.provider)v._private={room:a.room,...v.leads[0],transcript:{turns:[]}};
      if(a.turnId)v.turn={turnId:randomUUID(),status:'replied'};
      return v;
    },
    [send]:a=>{sent++;return {...leadStatus(a,'ready'),turn:{turnId:randomUUID(),status:'replied'}};}
  }});
  await p.open('input',{arguments:{project:'one'}});
  const button=p.get('leads').children[0].children[1].children.find(e=>e.type==='button');
  button.dispatchEvent(new Event('click'));await setImmediate();
  p.get('chat-text').value='ask once';await p.event('chat-text','input');await p.event('chat-compose','submit');
  assert.equal(p.get('chat-text').value,'ask once');assert.equal(p.get('chat-send').disabled,true);
  await p.event('chat-compose','submit');assert.equal(sent,1);
  p.host.delayTool=managedRead;await p.event('chat-check','click');
  await p.open('input',{arguments:{project:'two'}});p.host.release();await setImmediate();
  assert.ok(!p.get('conversation').open);assert.equal(p.get('chat-text').value,'');assert.equal(sent,1);
});

test('one-time Lead opening is separate from future auto-open and refresh never replays an uncertain write',async()=>{
  const memory=new Map(),storage={getItem:k=>memory.get(k),setItem:(k,v)=>memory.set(k,v)};
  const tools={[managedOpen]:()=>{throw Error('acknowledgement lost');}};
  const p=await panel(['one'],dashboard,{storage,tools});
  const input={arguments:{project:'one',leadConversations:true}}, result={structuredContent:{panel:'OMS',project:'one',leadConversations:true}};
  await p.open('input',input);assert.ok(!p.calls.some(c=>c.name===managedOpen));
  await p.open('result',result);await p.open('result',result);await p.event('refresh','click');
  assert.equal(p.calls.filter(c=>c.name===managedOpen).length,1);
  const q=await panel(['one'],dashboard,{storage,tools:{[managedOpen]:a=>leadStatus(a,'starting')}});
  assert.equal(q.calls.filter(c=>c.name===managedOpen).length,0);
  q.get('lead-auto').checked=true;await q.event('lead-auto','change');await q.event('refresh','click');
  assert.equal(q.calls.filter(c=>c.name===managedOpen).length,0,'saving a future preference does not start work now');
  q.notify({method:'ui/resource-teardown',id:'close-auto',params:{}});
  const reopened=await panel(['one'],dashboard,{storage,tools:{[managedOpen]:a=>leadStatus(a,'starting')}});
  assert.equal(reopened.calls.filter(c=>c.name===managedOpen).length,1);assert.match(reopened.get('leads').textContent,/시작 중/);
  reopened.get('lead-auto').checked=false;await reopened.event('lead-auto','change');
  reopened.notify({method:'ui/resource-teardown',id:'close-reopened',params:{}});
  const after=await panel(['one'],dashboard,{storage});assert.ok(!after.calls.some(c=>c.name===managedOpen));
  const readOnly=await panel(['one'],dashboard,{readOnly:true,storage,tools});
  await readOnly.open('input',input);await readOnly.open('result',result);await readOnly.event('open-leads','click');
  assert.deepEqual(readOnly.calls.map(c=>c.name),startup.slice(0,2));
  const deniedStorage={getItem(){throw Error('disabled');},setItem(){throw Error('disabled');}};
  const q2=await panel(['one'],dashboard,{storage:deniedStorage,tools});
  await q2.event('open-leads','click');assert.equal(q2.calls.filter(c=>c.name===managedOpen).length,1);
  const after2=await panel(['one'],dashboard,{storage:deniedStorage,tools});assert.ok(!after2.calls.some(c=>c.name===managedOpen));
});
test('starting Leads use bounded sequential status reads until ready, scoped away, or closed',async()=>{
  let now=0;
  const callbacks=new Map(), timers={
    Date:{now:()=>now},
    setTimeout(fn,ms){const id=Symbol();callbacks.set(id,{fn,ms,at:now+ms});return id;},
    clearTimeout(id){callbacks.delete(id);}
  };
  const polls=()=>[...callbacks].filter(([,v])=>v.ms>=8000 && v.ms<=15000);
  async function tick(){
    const jobs=polls();assert.equal(jobs.length,1);
    const [id,job]=jobs[0];callbacks.delete(id);now=job.at;job.fn();await setImmediate();
  }
  let status='not_started';
  const tools={[managedRead]:a=>leadStatus(a,status),[managedOpen]:a=>{status='starting';return leadStatus(a,status);}};
  const p=await panel(['one'],dashboard,{locale:'en',timers,tools});
  assert.equal(polls().length,0);
  await p.event('open-leads','click');
  assert.equal(polls()[0][1].ms,8000);
  p.host.delayTool=managedRead;await tick();assert.equal(polls().length,0);
  p.host.release();await setImmediate();assert.equal(polls()[0][1].ms,12000);
  status='ready';await tick();assert.equal(polls().length,0);
  assert.match(p.get('leads').textContent,/Ask Claude/);
  assert.equal(p.calls.filter(c=>c.name===managedOpen).length,1);
  status='starting';await p.event('refresh','click');const start=now;
  while(polls().length){await tick();assert.ok(now-start<=120000);}
  assert.match(p.get('lead-control-status').textContent,/Automatic checks paused/);assert.equal(p.get('lead-refresh').hidden,false);
  status='starting';
  const gated={getItem:key=>key.startsWith('oms.lead-conversations.')?'on':null};
  const snapshot={...dashboard};
  const q=await panel(['one','two'],snapshot,{timers,tools,storage:gated});
  await q.open('input',{arguments:{project:'one'}});
  status='not_started';await tick();assert.ok(!q.calls.some(c=>c.name===managedOpen));
  status='starting';await q.event('refresh','click');const stale=polls()[0][1].fn;
  await q.open('input',{arguments:{project:'unknown'}});
  assert.equal(polls().length,0);const mark=q.calls.length;stale();await setImmediate();assert.equal(q.calls.length,mark);
  await q.open('input',{arguments:{project:'two'}});assert.equal(polls().length,1);
  const oldMainPoll=polls()[0][1].fn;
  snapshot.conversationMain=null;await q.event('refresh','click');assert.equal(polls().length,0);
  const lostMain=q.calls.length;oldMainPoll();await setImmediate();assert.equal(q.calls.length,lostMain);
  snapshot.conversationMain=dashboard.conversationMain;await q.event('refresh','click');assert.equal(polls().length,1);
  q.notify({method:'ui/resource-teardown',id:'close-poll',params:{}});assert.equal(polls().length,0);
});
test('changed alias target, unconfirmed status and stale host or read responses cannot launch',async()=>{
  const storage={getItem:k=>k.endsWith('b'.repeat(64))?'on':null,setItem(){}};
  for(const [key,status] of [['c','not_started'],['b','unconfirmed'],['b','unavailable'],['b','ready']]){
    const q=await panel(['one'],dashboard,{storage,tools:{[managedRead]:a=>leadStatus(a,status,key.repeat(64))}});
    assert.ok(!q.calls.some(c=>c.name===managedOpen));
  }
  const p=await panel(['one','two'],dashboard,{});
  await p.open('input',{arguments:{project:'one',leadConversations:true}});
  p.get('project-choice').value='two';await p.event('project-choice','change');
  await p.open('result',{structuredContent:{panel:'OMS',project:'one',leadConversations:true}});
  assert.equal(p.get('project').textContent,'two');assert.ok(!p.calls.some(c=>c.name===managedOpen));
  p.host.delayTool=managedRead;await p.open('input',{arguments:{project:'one',leadConversations:true}});
  await p.open('result',{structuredContent:{panel:'OMS',project:'one',leadConversations:true}});
  await p.open('input',{arguments:{project:'two'}});
  p.host.release();await setImmediate();assert.ok(!p.calls.some(c=>c.name===managedOpen));
});
test('OMS Main · Pi keeps its run and parent-linked Leads without adopting another participant',async()=>{
  const run=(id,participant,extra)=>({room:main.room,id,...(participant&&{participant}),tool:'panel-main',role:'main',provider:'pi',state:'working',...extra});
  const child=(id,parentAttempt)=>({room:main.room,id,parentAttempt,role:'lead',provider:'claude',state:'running',taskTitle:id});
  // The harness may be unverified, and a fresh launch has no participant ref: its attempt ID is the participant.
  for(const [harness,owner] of [['pi',run('m','owner')],[null,run('m','owner')],['pi',run('owner')]]) {
    const d={...dashboard,conversationMain:{...main,harness,participants:[]},conversationAttempts:
      [run('other','someone-else'),{...owner,taskTitle:'Drive Pi'},child('mine',owner.id),child('foreign','other')]};
    const p=await panel(['one'],d,{locale:'en'});
    assert.match(p.get('main-status').textContent,/Working · Drive Pi/);
    const slots=p.get('leads').children;
    assert.equal(slots.length,3);
    assert.match(slots[1].textContent,/OMS Lead · Claude.*mine.*Running/);
    assert.doesNotMatch(p.get('leads').textContent,/foreign/);
  }
});
test('a degraded snapshot keeps a visible status and a partial one only marks the observed time',async()=>{
  const d={};
  const p=await panel(['one'],d,{locale:'en'});
  assert.equal(p.get('status').textContent,'');
  for(const [fields,expected] of [[{degraded:true},/not collected/],[{observation:{completeness:'unknown'}},/not collected/],
    [{observation:{completeness:'partial'}},/^$/],[{conversationAttemptsOmitted:2},/^$/],
    [{activeAttemptsTrimmed:1},/^$/],
    [{degraded:false,planProgress:{total:9,verified:9},planReadiness:{available:true,scope:'repository',omitted:1,tasks:Array.from({length:8},(_,i)=>({id:'t'+i,title:'Done '+i,reason:'recorded-done'}))},observation:{completeness:'complete',sources:{plan:'ok',acceptance:'ok'},evidence:{omitted:0,trimmed:0,readinessOmitted:1,evidenceOmitted:2}}},/^$/],
    [{degraded:false,observation:{completeness:'complete'}},/^$/]]) {
    for(const key of Object.keys(d))delete d[key];
    Object.assign(d,fields);await p.event('refresh','click');
    assert.match(p.get('status').textContent,expected);
  }
  d.updated='2026-06-01T09:18:00Z';d.activeAttemptsTrimmed=1;await p.event('refresh','click');
  assert.equal(p.get('status').textContent,'');assert.match(p.get('observed-at').textContent,/ · partial$/);
  d.degraded=true;await p.event('refresh','click');await choose(p,'ko');
  assert.match(p.get('status').textContent,/수집하지 못했습니다/);
});
test('a stored Issue receipt is not reported as delivered without a confirmed context update',async()=>{
  const reference='https://github.com/Example/Library/issues/7',hash='a'.repeat(64),requestId='aaaaaaaa-bbbb-8ccc-8ddd-eeeeeeeeeeee';
  const tools={oms_panel_git_status:()=>({issuePreview:{kind:'github-issue',state:'open',reference,revision:'2026-06-01T00:00:00Z',sha256:hash}}),
    oms_reviewed_issue_status:()=>({schema:1,kind:'oms-reviewed-issue-receipt',status:'recorded',requestId,room:main.room,participant:main.participant,issue:'Example/Library#7',sha256:hash})};
  for(const context of [false,true]) {
    const p=await panel(['one'],dashboard,{locale:'en',context,tools});
    p.host.rejectContext=true;
    await p.event('new-goal','click');p.get('brief').value=reference;await p.event('brief','input');
    await p.event('issue-review','click');await setImmediate();
    assert.match(p.get('issue-evidence').textContent,/Receipt recorded · delivery to Main not confirmed/);
    assert.doesNotMatch(p.get('issue-evidence').textContent,/added to|unconfirmed/);
    assert.equal(p.get('issue-record').disabled,false);
    if(context) {
      p.host.rejectContext=false;await p.event('issue-record','click');await setImmediate();
      assert.match(p.get('issue-evidence').textContent,/added to the current Main's context/);
      assert.equal(p.get('issue-record').disabled,true);
      assert.deepEqual({...p.host.contexts.at(-1).structuredContent},{project:'one',room:main.room,owner:main.participant,
        requestId,kind:'oms-reviewed-issue-receipt',status:'recorded',issue:'Example/Library#7',sha256:hash});
    }
    assert.ok(!p.calls.some(c=>c.name==='oms_record_reviewed_issue'));
  }
});
test('the team respects parent, provider, terminal membership and partial visibility',async()=>{
  const d={...dashboard,conversationMain:{...main,participants:[{...lead,parent:'someone-else'},{...lead,participant:'left',joined:false},
    {...lead,participant:'other',provider:'agy'}],participantsOmitted:1},conversationAttempts:
    ['lead-one','left','other'].map(participant=>({room:main.room,participant,id:participant,role:'lead',provider:'claude',state:'running'}))};
  const p=await panel(['one'],d,{locale:'en'});
  assert.equal(p.get('leads').children.length,3);
  assert.ok(p.get('leads').children.every(slot=>!slot.textContent.includes('Assignment unknown')));assert.match(p.get('lead-note').textContent,/part of/);
  assert.match(p.get('main-status').textContent,/Joined this room/);
  d.conversationMain.participantsOmitted=0;await p.event('refresh','click');
  assert.ok(p.get('leads').children.every(slot=>slot.textContent.includes('Waiting for assignment')));
  assert.equal(p.get('lead-note').textContent,'');
});
test('fixed roles keep distinct conversations together and leave unknown or unbound assignments passive',async()=>{
  const second={...lead,participant:'lead-two',label:'Documentation'};
  const agy={...lead,participant:'lead-three',provider:'agy'};
  const d={...dashboard,conversationMain:{...main,participants:[lead,second,agy],incomplete:true},conversationAttempts:[
    ...dashboard.conversationAttempts,
    {room:main.room,id:'l2',participant:second.participant,role:'lead',provider:'claude',state:'working',taskTitle:'Write the guide'},
    {room:main.room,id:'l3',participant:agy.participant,role:'lead',provider:'antigravity',state:'review'}]};
  const p=await panel(['one'],d,{locale:'en',tools:{oms_panel_chat:a=>({...a,source:'room',messages:[]})}});
  const slots=p.get('leads').children;
  assert.equal(slots.length,3);
  const buttons=slots[1].children.filter(e=>e.tagName==='button');
  assert.equal(buttons.length,2);assert.match(buttons[1].textContent,/Documentation.*Write the guide/);
  assert.match(slots[2].textContent,/OMS Lead · AGY.*In review/);assert.match(p.get('lead-note').textContent,/part of/);
  for(const [i,button] of buttons.entries()){
    button.dispatchEvent(new Event('click'));await setImmediate();
    assert.equal(p.calls.at(-1).args.participant,[lead,second][i].participant);
    await p.event('chat-back','click');assert.ok(button.focused);
  }
  const unconfirmed=await panel(['one'],{conversationBinding:'unconfirmed',activeAttempts:d.conversationAttempts},{locale:'ko'});
  assert.deepEqual(unconfirmed.calls.map(c=>c.name),startup.slice(0,3));
  assert.ok(unconfirmed.get('leads').children.every(slot=>!slot.textContent.includes('미확인') && slot.children.length===2));
  const unbound=await panel([],{}, {tools:{oms_list_projects:()=>({binding:'unbound',projects:[]})}});
  assert.equal(unbound.get('view').hidden,true);assert.equal(unbound.get('leads').children.length,0);
  assert.deepEqual(unbound.calls.map(c=>c.name),['oms_list_projects']);
});
test('workers nest under each verified Lead only and only matching Room members can open chat',async()=>{
  const second={...lead,participant:'second',label:'Documentation'};
  const worker={...lead,participant:'child-a',parent:lead.participant,provider:'agy',role:'worker',label:'Keyboard checks'};
  const enrolled={...worker,participant:'child-b',parent:second.participant,label:'Copy review'};
  const d={...dashboard,conversationMain:{...main,participants:[lead,second,worker,enrolled,{...enrolled,participant:'retired',joined:false}]},
    conversationAttempts:[...dashboard.conversationAttempts,
      {room:main.room,id:'l2',participant:second.participant,role:'lead',provider:'claude',state:'working'},
      {room:main.room,id:'w1',participant:worker.participant,parentAttempt:'l',role:'worker',provider:'antigravity',state:'verifying'},
      {room:main.room,id:'w3',participant:'retired',parentAttempt:'l2',role:'worker',provider:'agy',state:'done'},
      {room:null,id:'w2',parentAttempt:'l2',role:'worker',provider:'codex',state:'running',task:'internal-task'},
      ...[
        {room:'foreign',parentAttempt:'l'}, {room:null,parentAttempt:'unrelated'},
        {room:main.room,parentAttempt:'l2',participant:worker.participant},
        {room:main.room,parentAttempt:'l',participant:worker.participant,provider:'pi'},
        {room:main.room,parentAttempt:'l',role:'reviewer'}, {room:main.room,participant:'unowned'}
      ].map((r,i)=>({id:'bad-'+i,role:'worker',taskTitle:'Unrelated',...r}))]};
  const p=await panel(['one','two'],d,{locale:'en',tools:{oms_panel_chat:a=>({...a,source:'room',messages:[]})}});
  await p.open('input',{arguments:{project:'one'}});
  const [a,b]=p.get('leads').children[1].children.slice(2), elements=g=>g.children.filter(n=>n?.tagName), children=g=>elements(elements(g)[1]).map(n=>elements(n)[0]);
  assert.equal(elements(a)[1].tagName,'ul');assert.equal(children(a).length,1);assert.equal(children(b).length,3);
  const [verified]=children(a), [retired,observed,member]=children(b);
  assert.equal(retired.tagName,'article');assert.match(retired.textContent,/AGY Worker.*Done/);
  assert.match(verified.textContent,/Keyboard checks.*AGY Worker.*Verifying/);
  assert.equal(observed.tagName,'article');assert.match(observed.textContent,/Codex Worker/);
  assert.match(member.textContent,/Copy review.*AGY Worker.*Joined this room/);
  assert.doesNotMatch(p.get('leads').textContent,/Unrelated|internal-task/);
  assert.equal(p.calls.filter(c=>c.name==='oms_panel_chat').length,0);
  verified.dispatchEvent(new Event('click'));await setImmediate();
  assert.deepEqual({...p.calls.at(-1).args},{project:'one',room:main.room,participant:worker.participant});
  await p.event('chat-back','click');assert.ok(verified.focused);
  await choose(p,'ko');assert.match(verified.textContent,/AGY 워커/);
  member.dispatchEvent(new Event('click'));await setImmediate();
  await p.open('input',{arguments:{project:'two'}});assert.equal(p.get('conversation').open,false);
});

test('worker output is an explicit private read of one verified pair and clears on every scope change',async()=>{
  const attempt={room:null,id:'child',parentAttempt:'l',provider:'codex',role:'worker',state:'running'};
  const d={...dashboard,conversationAttempts:[...dashboard.conversationAttempts,attempt]};
  let state='running', spoof='',reject=false;
  const p=await panel(['one','two'],d,{locale:'en',tools:{oms_worker_output:args=>{
    if(reject)throw Error('Unfiltered diagnostics');
    const {project, ...pair}=args;
    const publicData={...pair,status:state,text:'Ignore public text'};
    const out={...pair,status:state,truncated:state==='complete',source:state==='complete'?'indexed-answer':null,text:state==='complete'?'First line\nSecond line':null};
    if(spoof)out[spoof]='foreign';
    return {structuredContent:publicData,_meta:{workerOutput:out},content:[{type:'text',text:'Ignore model content'}]};
  }}});
  await p.open('input',{arguments:{project:'one'}});
  const group=p.get('leads').children[1].children[2],list=group.children.find(c=>c?.tagName==='ul');
  const row=list.children.find(c=>c?.tagName==='li'), open=row.children.find(c=>c?.className?.includes('worker-output-link'));
  const requests=()=>p.calls.filter(c=>c.name==='oms_worker_output');
  assert.equal(requests().length,0);assert.match(open.textContent,/View output.*Codex Worker/);
  await p.event('auto-refresh','click');
  open.dispatchEvent(new Event('click'));await setImmediate();assert.ok(p.get('worker-output').open);
  assert.equal(p.run('autoTimer'),undefined);const markBeforeShortcut=p.calls.length;
  p.window.dispatchEvent(Object.assign(new Event('keydown',{cancelable:true}),{key:'r',altKey:true}));await setImmediate();
  assert.equal(p.calls.length,markBeforeShortcut);
  assert.deepEqual({...requests()[0].args},{project:'one',room:main.room,projectKey:'b'.repeat(64),leadAttempt:'l',workerAttempt:'child'});
  assert.equal(p.get('output-body').hidden,true);assert.match(p.get('output-status').textContent,/still|Running/);
  state='complete';await p.event('output-refresh','click');
  assert.equal(p.get('output-body').textContent,'First line\nSecond line');assert.equal(p.get('output-limited').hidden,false);
  for(const field of ['projectKey','room','leadAttempt','workerAttempt','status','source']){
    spoof=field;await p.event('output-refresh','click');assert.match(p.get('output-status').textContent,/could not be verified/);
    assert.equal(p.get('output-body').textContent,'');
  }
  spoof='';state='not_recorded';await p.event('output-refresh','click');assert.match(p.get('output-status').textContent,/No output was recorded/);
  reject=true;await p.event('output-refresh','click');assert.match(p.get('output-status').textContent,/could not be verified/);
  assert.equal(p.get('output-limited').hidden,true);assert.equal(p.get('output-body').textContent,'');
  await choose(p,'ko');assert.match(p.get('output-status').textContent,/확인할 수 없습니다/);
  await p.event('output-close','click');assert.ok(open.focused);assert.ok(p.run('autoTimer'));await p.event('auto-refresh','click');
  reject=false;state='complete';p.host.delayTool='oms_worker_output';open.dispatchEvent(new Event('click'));await setImmediate();
  await p.open('input',{arguments:{project:'two'}});p.host.release();await setImmediate();
  assert.equal(p.get('worker-output').open,false);assert.equal(p.get('output-body').textContent,'');assert.equal(p.get('output-worker').textContent,'');
  const mark=requests().length;open.dispatchEvent(new Event('click'));await setImmediate();assert.equal(requests().length,mark);
  assert.equal(p.host.contexts.length,0);assert.equal(p.host.messages.length,0);
});

test('scoped transcript paginates only on demand and erases private data after switching projects',async()=>{
  const p=await panel(['one','two'],dashboard,{locale:'en',tools:{oms_panel_chat:a=>({...a,source:'room',messages:
    [{seq:a.before==null?3:1,text:a.before==null?'Latest message':'Prior message'}],nextBefore:a.before==null?3:null})}});
  await p.open('input',{arguments:{project:'one'}});
  p.get('leads').children[1].children[2].dispatchEvent(new Event('click'));await setImmediate();
  assert.equal(p.get('chat-messages').textContent,'Latest message');
  assert.deepEqual({...p.calls.at(-1).args},{room:'room-a',participant:'lead-one',project:'one'});
  await p.event('chat-older','click');assert.equal(p.get('chat-messages').textContent,'Prior messageLatest message');
  await p.open('input',{arguments:{project:'two'}});
  assert.equal(p.get('chat-messages').textContent,'');assert.equal(p.get('conversation').open,false);
});
test('only explicit decisions use the exact approval receipt and a verified completion is separate',async()=>{
  const row={id:'receipt-2',version:4,digest:'f'.repeat(64),summary:'Publish the documentation?',action:'Main scope decision',object:main.room,sender:'internal-name'};
  for(const decision of ['approve','deny']){
    let rows=[row];const d={...dashboard,acceptance:{available:true,scope:'repository',complete:false,total:3,verified:2}};
    const p=await panel(['one'],d,{locale:'en',tools:{oms_list_approvals:()=>({approvals:rows}),oms_decide_approval:a=>{rows=[];return a;}}});
    assert.equal(p.get('approval-card').hidden,false);assert.match(p.get('approvals').textContent,/Publish the documentation/);
    assert.doesNotMatch(p.get('approvals').textContent,/receipt-2|ffff|room-a|internal-name/);
    assert.equal(p.get('outcome').hidden,true);
    d.acceptance={available:true,scope:'repository',complete:true,total:3,verified:3};
    const details=p.get('approvals').children[0];
    const buttons=details.children.at(-1).children.at(-1).children.filter(b=>b.type==='button');
    buttons[decision==='approve'?0:1].dispatchEvent(new Event('click'));await setImmediate();
    assert.deepEqual({...p.calls.find(c=>c.name==='oms_decide_approval').args},{project:'one',id:row.id,version:4,digest:row.digest,decision});
    assert.equal(p.get('approval-card').hidden,true);assert.match(p.get('outcome').textContent,/All 3 project completion criteria verified/);
  }
});
test('Issue handoff verifies the exact URL and recovers the same receipt after a fresh panel',async()=>{
  const reference='https://github.com/Example/Library/issues/7',hash='a'.repeat(64),key='aaaaaaaa-bbbb-8ccc-8ddd-eeeeeeeeeeee';
  let saved, writes=0;
  const tools={oms_panel_git_status:a=>{assert.equal(a.issuePreview,7);return {issuePreview:{kind:'github-issue',state:'open',reference,revision:'2026-06-01T00:00:00Z',sha256:hash}};},
    oms_reviewed_issue_status:()=>saved||{requestId:key,status:'not_found'},oms_record_reviewed_issue:a=>{
      writes++;saved={schema:1,kind:'oms-reviewed-issue-receipt',status:'recorded',requestId:a.requestId,room:a.room,participant:a.owner,issue:a.issue,sha256:a.sha256};
      assert.deepEqual({...a},{issue:'Example/Library#7',sha256:hash,room:main.room,owner:main.participant,requestId:key,project:'one'});throw Error('lost acknowledgement');}};
  for(const first of [true,false]){
    const p=await panel(['one'],dashboard,{locale:'en',context:true,tools});
    await p.event('new-goal','click');p.get('brief').value=reference.toLowerCase();await p.event('brief','input');
    assert.deepEqual(p.calls.map(c=>c.name),startup);await p.event('issue-review','click');await setImmediate();
    if(first){assert.match(p.get('issue-evidence').textContent,/ready to hand off/);await p.event('issue-record','click');assert.match(p.get('issue-evidence').textContent,/unconfirmed/);}
    else {assert.match(p.get('issue-evidence').textContent,/Issue recorded/);assert.equal(p.host.contexts.at(-1).structuredContent.requestId,key);}
    assert.equal(writes,1);assert.equal(p.host.messages.length,0);
  }
  const p=await panel(['one'],dashboard,{tools});
  await p.event('new-goal','click');p.get('brief').value='https://github.com/elsewhere/library/issues/7';await p.event('brief','input');
  await p.event('issue-review','click');assert.equal(p.get('issue-record').hidden,true);
  assert.ok(!p.calls.some(c=>c.name==='oms_reviewed_issue_status'));
});
test('refresh and translation retain the active draft and do not revive an old status or an approval from another project',async()=>{
  const scope={id:'ask',version:1,digest:'a'.repeat(64),summary:'Apply changes?',object:main.room,action:'Main scope decision'};
  const p=await panel(['one','two'],dashboard,{tools:{oms_list_approvals:args=>({approvals:args.project==='one'?[scope]:[]})}});
  await p.open('input',{arguments:{project:'one'}});
  const box=p.get('approvals').children[0].children.at(-1).children.at(-1);
  const old=box.children.find(c=>c.type==='button');
  await p.event('new-goal','click');p.get('brief').value='Save this draft';
  await choose(p,'en');assert.equal(p.get('brief').value,'Save this draft');assert.equal(p.get('status').textContent,'');
  await p.open('input',{arguments:{project:'two'}});
  assert.equal(p.get('brief').value,'');assert.equal(p.get('launcher').open,false);assert.equal(p.get('approval-card').hidden,true);
  old.dispatchEvent(new Event('click'));await setImmediate();assert.ok(!p.calls.some(c=>c.name==='oms_decide_approval'));
});
test('progress and goal missing states are independent of repository acceptance and legacy counts',async()=>{
  const d={tasks:5,tasksVerified:4,goal:null,acceptance:{available:false}};
  const p=await panel(['one'],d,{locale:'en'});
  assert.equal(p.get('goal').textContent,'Project goal unconfirmed');
  assert.equal(p.get('progress').hidden,true);
  d.goalAvailability='absent';d.planProgress={total:0,verified:0};await p.event('refresh','click');
  assert.equal(p.get('goal').textContent,'No goal recorded');assert.equal(p.get('progress').hidden,true);
  d.planProgress={total:3,verified:2};await p.event('refresh','click');
  assert.equal(p.get('progress').textContent,'Plan tasks 2 / 3');
  // Unconfirmed criteria leave the bar to the plan and show no count rather than zero.
  assert.equal(p.get('progress-track').hidden,false);assert.equal(p.get('criteria-summary').textContent,'');
  assert.equal(p.get('outcome').hidden,true);
});
test('language persists on reopen without fetching or replacing the focused draft',async()=>{
  const memory=new Map(),storage={getItem:key=>memory.get(key),setItem:(key,value)=>memory.set(key,value)};
  const p=await panel(['one'],dashboard,{locale:'ko',storage});
  await p.event('new-goal','click');const brief=p.get('brief');brief.value='Keyboard audit';p.document.activeElement=brief;
  const mark=p.calls.length;await choose(p,'en');
  assert.equal(p.calls.length,mark);assert.equal(p.document.activeElement,brief);assert.equal(brief.value,'Keyboard audit');
  assert.equal(p.get('new-goal').textContent,'Start new work');
  const q=await panel(['one'],dashboard,{locale:'ko',storage});assert.equal(q.get('new-goal').textContent,'Start new work');
});
test('partial approval retrieval stays visible without a decision button or a false empty state',async()=>{
  const p=await panel(['one'],{},{locale:'en',tools:{oms_list_approvals:()=>({approvals:[],incomplete:true})}});
  assert.equal(p.get('approval-card').hidden,false);assert.match(p.get('approvals').textContent,/Approval list partly read/);
  assert.ok(p.get('approvals').children.every(child=>child.tagName!=='details'));
  p.host.tools.oms_list_approvals=()=>{throw Error('failed read');};await p.event('refresh','click');
  assert.equal(p.get('approval-card').hidden,false);assert.equal(p.get('approvals').children[0].tagName,'p');
  assert.ok(!p.calls.some(c=>c.name==='oms_decide_approval'));
});

test('state badges carry a semantic tone and the progress bar follows the progress text',async()=>{
  const d={...dashboard,planProgress:{total:4,verified:1}};
  const p=await panel(['one'],d,{locale:'en'});
  const [codex,claude,agy]=p.get('leads').children;
  const badge=entry=>entry.children.find(e=>e.className==='state');
  assert.equal(badge(codex.children[2]).getAttribute('data-tone'),'active');
  assert.equal(badge(claude.children[2]).getAttribute('data-tone'),'decision');
  assert.equal(badge(claude.children[2]).textContent,'! Blocked');
  assert.equal(agy.children[1].children.find(e=>e.className==='state').getAttribute('data-tone'),'muted');
  const presence=slot=>slot.children.find(e=>e.className==='slot-heading').children.find(e=>e.className==='avatar').getAttribute('data-presence');
  assert.deepEqual([codex,claude,agy].map(presence),['working','decision','idle']);
  assert.equal(p.get('avatar').getAttribute('data-presence'),'working');
  assert.equal(p.get('progress-track').hidden,false);assert.equal(p.get('progress-fill').style.width,'25%');
  assert.equal(p.get('progress').textContent,'Plan tasks 1 / 4');
  d.planProgress={total:4,verified:4};await p.event('refresh','click');assert.equal(p.get('progress-fill').style.width,'100%');
  d.planProgress={total:5,verified:2};await p.event('refresh','click');
  const segments=p.get('progress-track').children.filter(e=>e.className?.startsWith('seg'));
  assert.equal(segments.length,5);assert.equal(p.get('progress-track').getAttribute('data-segments'),'true');
  assert.deepEqual(['seg done','seg current'].map(c=>segments.filter(e=>e.className===c).length),[2,1]);
  d.planProgress={total:0,verified:0};await p.event('refresh','click');
  assert.equal(p.get('progress').hidden,true);assert.equal(p.get('progress-track').hidden,true);
  d.conversationMain=undefined;await p.event('refresh','click');
  assert.equal(p.get('avatar').textContent,'–');assert.equal(p.get('avatar').getAttribute('data-presence'),null);
});
test('v33 tones split decision from failure, rank the presence dot and keep the header to three groups',async()=>{
  const p=await panel(['one'],dashboard,{locale:'en'});
  const tone=state=>p.run('stateTone('+JSON.stringify(state)+')');
  for(const state of ['waiting_for_input','waiting_for_review','waiting_approval','blocked'])assert.equal(tone(state),'decision',state);
  for(const state of ['failed','timed_out','guard_stop','rejected','interrupted','orphaned'])assert.equal(tone(state),'failure',state);
  for(const state of ['degraded','unconfirmed','unavailable','queued','',null])assert.equal(tone(state),'muted',String(state));
  assert.equal(tone('running'),'active');assert.equal(tone('verified'),'done');
  const rank=(...tones)=>p.run('presenceOf(new Set('+JSON.stringify(tones)+'),false,true)');
  assert.deepEqual([rank('done','active','decision','failure'),rank('active','decision'),rank('done','active'),rank('done'),
    p.run('presenceOf(new Set(["muted"]),false,false)')],['failure','decision','working','live','idle']);
  assert.doesNotMatch(html,/data-(tone|presence)=attention/);
  for(const name of ['decision','failure'])assert.match(html,new RegExp('\\.state\\[data-tone='+name+'\\]'));
  // Project, language and the refresh segment; Inspect moved to the closing row of the view.
  const tools=html.match(/<div class="tools">([\s\S]*?)<\/div><\/header>/)[1];
  assert.deepEqual([...tools.matchAll(/<(select|button|span) id="([^"]+)"|<span class="([^"]+)"/g)].map(m=>m[2]||m[3]),['language-choice','refresh-group','refresh','auto-refresh','display-mode']);
  assert.ok(html.indexOf('id="inspect-open"')>html.indexOf('id="more-results"'));
  assert.equal(p.get('leads-heading').textContent,'Leads 3');
  const ko=await panel(['one'],dashboard,{locale:'ko'});assert.equal(ko.get('leads-heading').textContent,'Lead 3');
  assert.equal(ko.get('leads-help').textContent,'질문은 읽기 전용 상담 · 작업 배정은 Main에서');
});
test('criteria lead the bar when the whole list is projected, and the plan bar returns when none are recorded',async()=>{
  const criterion=(id,status,stale=false)=>({id,label:'Criterion '+id,status,evidence:[{reference:'artifact:'+id,kind:'verification',status:'passed',stale}],evidenceOmitted:0});
  const d={...dashboard,planProgress:{total:5,verified:2},acceptance:{available:true,scope:'repository',total:3,verified:1,complete:false,omitted:0,
    criteria:[criterion('a','verified'),criterion('b','pending',true),criterion('c','pending')]}};
  const p=await panel(['one'],d,{locale:'en'});
  const segments=()=>p.get('progress-track').children.filter(e=>e.className?.startsWith('seg')).map(e=>e.className);
  assert.deepEqual(segments(),['seg done','seg stale','seg']);
  assert.equal(p.get('criteria-summary').textContent,'Criteria 1 / 3 verified');assert.equal(p.get('progress').textContent,'Plan tasks 2 / 5');
  d.acceptance={available:true,scope:'repository',total:0,verified:0,complete:false,omitted:0,criteria:[]};await p.event('refresh','click');
  assert.deepEqual(segments(),['seg done','seg done','seg current','seg','seg']);
  assert.equal(p.get('criteria-summary').textContent,'No repository completion criteria recorded.');
});
test('the observed time localizes beside the goal while a clean status stays empty',async()=>{
  const stamp='2026-06-01T03:04:00Z', local=new Date(stamp);
  const time=[local.getHours(),local.getMinutes()].map(n=>String(n).padStart(2,'0')).join(':');
  const d={updated:stamp};
  const p=await panel(['one'],d,{locale:'ko'});
  assert.equal(p.get('observed-at').textContent,'관측 '+time);assert.equal(p.get('observed-at').hidden,false);
  await choose(p,'en');assert.equal(p.get('observed-at').textContent,'Updated '+time);
  assert.equal(p.get('status').textContent,'');
  d.degraded=true;await p.event('refresh','click');assert.equal(p.get('status').getAttribute('data-tone'),'warn');
  d.updated='not a time';d.degraded=false;await p.event('refresh','click');
  assert.equal(p.get('observed-at').hidden,true);assert.equal(p.get('observed-at').textContent,'');
  assert.equal(p.get('status').textContent,'');
});
test('chat refresh rereads the open Room or managed Lead conversation in place',async()=>{
  let version=1;
  const p=await panel(['one'],dashboard,{locale:'en',tools:{
    oms_panel_chat:a=>({...a,source:'room',messages:[{seq:1,text:'Room '+version,...(version>1&&{at:'2026-06-01T03:04:00Z'})}]}),
    [managedRead]:a=>{
      const v=leadStatus(a,'ready');
      if(a.provider)v._private={...v.leads[0],room:a.room,transcript:{turns:[{role:'claude',text:'Lead '+version}]}};
      return v;
    }
  }});
  const slot=p.get('leads').children[1], messages=()=>p.get('chat-messages'), scroll=p.get('chat-scroll');
  Object.defineProperty(scroll,'scrollHeight',{value:500});scroll.clientHeight=100;
  for(const [opener,label,tool] of [[slot.children[2],'Room ','oms_panel_chat'],
    [slot.children[1].children.find(e=>e.type==='button'),'Lead ',managedRead]]){
    version=1;opener.dispatchEvent(new Event('click'));await setImmediate();
    assert.match(messages().textContent,new RegExp(label+'1'));
    scroll.scrollTop=10;
    const mark=p.calls.length;version=2;await p.event('chat-refresh','click');
    assert.deepEqual(p.calls.slice(mark).map(c=>c.name),[tool]);
    assert.match(messages().textContent,new RegExp(label+'2'));
    // Both readers retain the current position when the user is reading earlier content.
    assert.equal(messages().children.length,1);assert.equal(scroll.scrollTop,10);
    assert.equal(p.get('conversation').open,true);
    assert.equal(messages().children[0].children.some(e=>e.tagName==='time'),label==='Room ');
    // Backdrop clicks close; a press that began inside the content does not.
    p.get('conversation').dispatchEvent(new Event('click'));assert.equal(p.get('conversation').open,false);
    assert.ok(opener.focused);
  }
});
test('opt-in auto refresh persists, pauses and clears, and Alt+R refreshes only outside typing and dialogs',async()=>{
  let now=1000;
  const jobs=new Map(), memory=new Map();
  const timers={Date:{now:()=>now},setTimeout(fn,ms){const id=Symbol();jobs.set(id,{fn,ms});return id;},clearTimeout(id){jobs.delete(id);}};
  const storage={getItem:k=>memory.get(k),setItem:(k,v)=>memory.set(k,v)};
  const autos=()=>[...jobs].filter(([,job])=>job.ms===20000);
  const p=await panel(['one'],{},{locale:'en',timers,storage,visibility:true});
  const reads=()=>p.calls.filter(c=>c.name==='oms_panel_snapshot').length;
  assert.equal(autos().length,0);assert.equal(p.get('auto-refresh').getAttribute('aria-pressed'),'false');
  // With Auto off, neither return trigger rereads a stale snapshot.
  let mark=reads();now+=15000;p.window.dispatchEvent(new Event('focus'));await setImmediate();assert.equal(reads(),mark);
  p.document.show(false);p.document.show(true);await setImmediate();assert.equal(reads(),mark);assert.equal(autos().length,0);
  await p.event('auto-refresh','click');
  assert.equal(memory.get('oms.panel.auto-refresh.v1'),'on');assert.equal(p.get('auto-refresh').getAttribute('aria-pressed'),'true');
  assert.equal(autos().length,1);
  mark=reads();const [id,job]=autos()[0];jobs.delete(id);job.fn();await setImmediate();
  assert.equal(reads(),mark+1);assert.equal(autos().length,1);
  p.document.show(false);assert.equal(autos().length,0);
  // A return within 15 seconds only rearms; a stale return rereads once.
  mark=reads();p.document.show(true);await setImmediate();assert.equal(reads(),mark);assert.equal(autos().length,1);
  now+=15000;p.window.dispatchEvent(new Event('focus'));await setImmediate();assert.equal(reads(),mark+1);
  await p.event('new-goal','click');assert.equal(autos().length,0);
  assert.match(html,/<button id="refresh"[^>]*\saria-keyshortcuts="Alt\+R"/);
  const press=async(extra={altKey:true},target=p)=>{target.window.dispatchEvent(Object.assign(new Event('keydown',{cancelable:true}),{key:'r',...extra}));await setImmediate();};
  mark=reads();now+=15000;await press();p.window.dispatchEvent(new Event('focus'));await setImmediate();assert.equal(reads(),mark);
  await p.event('close-launch','click');assert.equal(autos().length,1);
  p.document.activeElement=p.get('brief');await press();assert.equal(reads(),mark);
  p.document.activeElement=undefined;
  for(const extra of [{},{ctrlKey:true},{metaKey:true},{altKey:true,ctrlKey:true},{altKey:true,metaKey:true},{altKey:true,shiftKey:true},{altKey:true,repeat:true}])await press(extra);
  assert.equal(reads(),mark);await press();assert.equal(reads(),mark+1);
  // macOS reports a composed character for Option+R, so the physical key also matches.
  await press({altKey:true,key:'\u00ae',code:'KeyR'});assert.equal(reads(),mark+2);
  await p.event('auto-refresh','click');assert.equal(autos().length,0);assert.equal(memory.get('oms.panel.auto-refresh.v1'),'off');
  await p.event('auto-refresh','click');
  const reopened=await panel(['one'],{},{timers:{...timers,setTimeout:()=>Symbol(),clearTimeout(){}},storage});
  assert.equal(reopened.get('auto-refresh').getAttribute('aria-pressed'),'true');
  p.notify({method:'ui/resource-teardown',id:'close-auto-refresh',params:{}});assert.equal(autos().length,0);
  mark=reads();await press();assert.equal(reads(),mark);assert.equal(p.get('refresh').getAttribute('aria-busy'),'false');
  // A pinned binding lists no aliases; its loaded view still refreshes.
  const pinned=await panel([],{},{timers,tools:{oms_list_projects:()=>({projects:[],binding:'pinned'})}});
  const pinnedReads=()=>pinned.calls.filter(c=>c.name==='oms_panel_snapshot').length;
  assert.equal(autos().length,0);await pinned.event('auto-refresh','click');
  assert.equal(pinned.get('auto-refresh').getAttribute('aria-pressed'),'true');assert.equal(autos().length,1);
  mark=pinnedReads();const [pinnedId,pinnedJob]=autos()[0];jobs.delete(pinnedId);pinnedJob.fn();await setImmediate();
  assert.equal(pinnedReads(),mark+1);assert.equal(autos().length,1);
  await press({},pinned);assert.equal(pinnedReads(),mark+1);
  await press({altKey:true},pinned);assert.equal(pinnedReads(),mark+2);
});
test('transient snapshot failures auto-retry a bounded number of times and recover',async()=>{
  const jobs=new Map(), memory=new Map([['oms.panel.auto-refresh.v1','on']]);
  const timers={setTimeout(fn,ms){const id=Symbol();jobs.set(id,{fn,ms});return id;},clearTimeout(id){jobs.delete(id);}};
  const storage={getItem:k=>memory.get(k),setItem:(k,v)=>memory.set(k,v)};
  const autos=()=>[...jobs].filter(([,job])=>job.ms===20000);
  const fire=async()=>{assert.equal(autos().length,1);const [id,job]=autos()[0];jobs.delete(id);job.fn();await setImmediate();};
  let failing=false, binding;
  const p=await panel(['one'],{},{locale:'en',timers,storage,tools:{
    oms_list_projects:()=>binding?{projects:[],binding}:{projects:['one']},
    oms_panel_snapshot:()=>{if(failing)throw new Error('offline');return {dashboard:{project:'one'}};}}});
  const reads=()=>p.calls.filter(c=>c.name==='oms_panel_snapshot').length;
  // Fires every scheduled retry, bounded so an unbounded loop fails instead of hanging.
  const drain=async()=>{let fired=0;while(autos().length && fired<20){await fire();fired++;}return fired;};
  // One failure hides the view but keeps Auto on, and the next tick recovers.
  failing=true;await fire();
  assert.equal(p.get('view').hidden,true);assert.equal(p.get('auto-refresh').getAttribute('aria-pressed'),'true');
  assert.equal(autos().length,1);
  failing=false;await fire();assert.equal(p.get('view').hidden,false);assert.equal(autos().length,1);
  // Recovery restores the full budget; persistent failure stops after five automatic retries.
  failing=true;let mark=reads();await fire();assert.equal(await drain(),5);
  assert.equal(reads(),mark+6);assert.equal(autos().length,0);
  // Manual refresh and re-enabling Auto each grant a new budget.
  mark=reads();await p.event('refresh','click');assert.equal(await drain(),5);assert.equal(reads(),mark+6);
  await p.event('auto-refresh','click');assert.equal(autos().length,0);
  await p.event('auto-refresh','click');assert.equal(await drain(),5);
  p.window.dispatchEvent(Object.assign(new Event('keydown',{cancelable:true}),{key:'r',altKey:true}));await setImmediate();
  assert.equal(await drain(),5);
  // Return triggers spend the same budget and cannot exceed it.
  await p.event('refresh','click');await fire();failing=false;await fire();assert.equal(p.get('view').hidden,false);
  // An unbound registry is not a transient failure.
  failing=true;await fire();binding='unbound';await fire();assert.equal(autos().length,0);
  await p.event('auto-refresh','click');await p.event('auto-refresh','click');assert.equal(autos().length,0);
});
test('stale return triggers share the transient retry cap',async()=>{
  let now=1000, failing=false;
  const jobs=new Map(), memory=new Map([['oms.panel.auto-refresh.v1','on']]);
  const timers={Date:{now:()=>now},setTimeout(fn,ms){const id=Symbol();jobs.set(id,{fn,ms});return id;},clearTimeout(id){jobs.delete(id);}};
  const storage={getItem:k=>memory.get(k),setItem:(k,v)=>memory.set(k,v)};
  const p=await panel(['one'],{},{timers,storage,tools:{oms_panel_snapshot:()=>{if(failing)throw new Error('offline');return {dashboard:{project:'one'}};}}});
  const reads=()=>p.calls.filter(c=>c.name==='oms_panel_snapshot').length;
  failing=true;now+=15000;
  const mark=reads();
  for(let i=0;i<9;i++){p.window.dispatchEvent(new Event('focus'));await setImmediate();}
  // The first read starts from a healthy view; five more are transient retries.
  assert.equal(reads(),mark+6);assert.equal([...jobs.values()].filter(job=>job.ms===20000).length,0);
});
test('the launcher counts and normalizes the brief, and the container width selects compact density',async()=>{
  const p=await panel(['one'],{},{locale:'ko',hostContext:{locale:'ko',displayMode:'inline',containerDimensions:{maxWidth:360}}});
  const root=p.document.documentElement, brief=p.get('brief'), count=p.get('brief-count');
  const change=params=>p.notify({method:'ui/notifications/host-context-changed',params});
  assert.equal(root.getAttribute('data-compact'),'true');assert.equal(root.getAttribute('data-display-mode'),'inline');
  change({theme:'light'});assert.equal(root.getAttribute('data-compact'),'true');
  for(const [dimensions,expected] of [[{width:421},null],[{width:420},'true'],[{},null],[{width:320},'true'],[null,null]]){
    change({containerDimensions:dimensions});assert.equal(root.getAttribute('data-compact'),expected);
  }
  assert.match(brief.getAttribute('placeholder'),/^예: Auth/);
  await choose(p,'en');assert.match(brief.getAttribute('placeholder'),/^e\.g\., Fix auth tests/);
  assert.equal(count.textContent,'0 / 2,000');
  brief.value='x'.repeat(2000);await p.event('brief','input');
  assert.equal(count.textContent,'2,000 / 2,000');assert.equal(count.getAttribute('data-tone'),'muted');
  p.get('lead-claude').checked=true;await p.event('lead-claude','change');
  assert.equal(count.getAttribute('data-tone'),'warn');
  assert.equal(count.textContent,'2,034 / 2,000');
  brief.value='';
  const paste=Object.assign(new Event('paste',{cancelable:true}),{clipboardData:{getData:()=>'Fix auth\r\n\r\n\tand tests\n'}});
  brief.dispatchEvent(paste);
  assert.equal(paste.defaultPrevented,true);assert.equal(brief.value,'Fix auth and tests');
  assert.equal(count.textContent,'52 / 2,000');
});

test('goal evidence separates repository criteria, task readiness and a scoped conversation draft',async()=>{
  const d={...dashboard,goal:'한'.repeat(2000),planProgress:{total:7,verified:4},
    acceptance:{available:true,scope:'repository',total:3,verified:2,complete:false,omitted:1,criteria:[
      {id:'saved',label:'Saved values persist',status:'verified',evidence:[{reference:'artifact:recorded',kind:'verification',status:'passed',stale:false}],evidenceOmitted:0},
      {id:'errors',label:'Error location is clear',status:'pending',task:'task-review',evidence:[],evidenceOmitted:0}]},
    planReadiness:{available:true,scope:'repository',tasks:[{id:'task-review',title:'Review error guidance',state:'review',reason:'parent-review-pending',selectionEligible:false}],omitted:0},
    nextStep:{id:'inspect',scope:'repository',summary:'Inspect the last criterion',authority:'read-only'}};
  const copied=[],key='b'.repeat(64),p=await panel(['one','two'],d,{locale:'en',context:true,navigator:{clipboard:{writeText:async text=>copied.push(text)}},
    tools:{oms_list_projects:()=>({projects:['one','two'],projectKeys:{one:key,two:'c'.repeat(64)}})}});
  await p.open('input',{arguments:{project:'one'}});
  assert.equal(p.get('progress').textContent,'Plan tasks 4 / 7');
  // One criterion is omitted, so the bar fills by ratio without segments and never adds the plan count.
  assert.equal(p.get('criteria-summary').textContent,'Criteria 2 / 3 verified · partly shown');
  assert.equal(p.get('progress-fill').style.width,'67%');assert.equal(p.get('progress-track').getAttribute('data-segments'),'false');assert.equal(p.get('outcome').hidden,true);
  assert.match(p.get('attention-summary').textContent,/Review error guidance.*Waiting for Main review/);
  assert.match(p.get('next-step').textContent,/Inspect the last criterion/);
  const mark=p.calls.length;await p.event('evidence-toggle','click');
  assert.match(p.get('criteria-list').textContent,/Saved values persist.*Verified.*artifact:recorded/);
  assert.match(p.get('evidence-note').textContent,/Only part/);assert.equal(p.calls.length,mark);
  await p.event('followup-open','click');assert.match(p.get('followup-text').value,/Review error guidance/);
  assert.match(html,/#followup-panel\{[^}]*border-radius:var\(--r-sm\)\}/);
  assert.match(html,/#followup-text\{[^}]*border-radius:var\(--r-xs\)[;}]/);
  const generated=p.get('followup-text').value;
  assert.ok(generated.includes('한'.repeat(400)+'…'));assert.ok(!generated.includes(d.goal));
  assert.ok(Buffer.byteLength(generated,'utf8')<=4000);assert.equal(Boolean(p.get('followup-copy').disabled),false);assert.equal(Boolean(p.get('followup-add').disabled),false);
  await p.event('followup-copy','click');assert.equal(copied.length,1);assert.ok(copied[0].includes('Task: task-review'));
  assert.ok(!copied[0].includes(key) && !copied[0].includes(main.room) && !copied[0].includes(main.participant));
  await p.event('followup-add','click');
  const context=p.host.contexts.at(-1).structuredContent;
  assert.equal(context.kind,'oms-followup-draft');assert.equal(context.project,'one');assert.equal(context.task,'task-review');
  assert.match(context.summary,/Review error guidance/);assert.ok(!JSON.stringify(p.host.contexts.at(-1)).includes(key));
  assert.equal(Object.hasOwn(context,'projectKey'),false);assert.equal(Object.hasOwn(context,'room'),false);assert.equal(Object.hasOwn(context,'owner'),false);
  assert.match(p.get('followup-status').textContent,/context · Main receipt unconfirmed/);
  assert.equal(p.host.messages.length,0);assert.equal(p.calls.length,mark);
  d.acceptance.criteria[0].label='Long criterion '.padEnd(2000,'x');d.nextStep.summary='Long next step '.padEnd(1200,'y');
  await p.event('refresh','click');if(p.get('goal-evidence').hidden)await p.event('evidence-toggle','click');
  assert.ok(p.get('criteria-list').textContent.includes(d.acceptance.criteria[0].label));assert.ok(p.get('next-step').textContent.includes(d.nextStep.summary));
  d.nextStep.id='resolve_blocker';d.nextStep.summary='An unresolved unknown_failure failure is recorded.';await p.event('refresh','click');
  assert.match(p.get('next-step').textContent,/Resolve the recorded failure/);assert.doesNotMatch(p.get('next-step').textContent,/unknown_failure/);
  d.acceptance.scope='room';d.acceptance.complete=true;d.acceptance.verified=3;await p.event('refresh','click');
  assert.equal(p.get('criteria-summary').textContent,'');assert.equal(p.get('outcome').hidden,true);
  await p.open('input',{arguments:{project:'two'}});
  assert.equal(p.get('followup-panel').hidden,true);assert.equal(p.get('followup-text').value,'');
});

test('individual Lead connection sends only the selected provider and never saves auto-open',async()=>{
  const memory=new Map(),storage={getItem:k=>memory.get(k),setItem:(k,v)=>memory.set(k,v)},states={codex:'not_started',claude:'not_started',agy:'not_started'};
  const status=a=>{const v=leadStatus(a);for(const row of v.leads)row.status=states[row.provider];return v;};
  const p=await panel(['one'],dashboard,{locale:'en',storage,tools:{
    [managedRead]:status,[managedOpen]:a=>{assert.deepEqual([...a.providers],['claude']);states.claude='ready';return status(a);}
  }});
  const connect=p.get('leads').children[1].children[1].children.find(e=>e.type==='button' && e.textContent==='Connect Claude');
  connect.dispatchEvent(new Event('click'));await setImmediate();
  const sent=p.calls.find(c=>c.name===managedOpen).args;
  assert.equal(sent.project,'one');assert.equal(sent.room,main.room);assert.equal(sent.projectKey,'b'.repeat(64));assert.deepEqual([...sent.providers],['claude']);
  assert.match(p.get('leads').children[1].textContent,/Ask Claude/);
  assert.ok(p.get('leads').children[0].textContent.includes('Connect Codex'));
  assert.equal(p.get('lead-auto').checked,false);assert.equal(memory.size,0);
});

test('worker result drafts keep the selected task and omit unknown attribution instead of using another attention item',async()=>{
  const key='b'.repeat(64),tasks=['task-a','task-b',null],d={...dashboard,
    planReadiness:{available:true,scope:'repository',tasks:[{id:'task-a',title:'Review task A',reason:'parent-review-pending'}]},
    conversationAttempts:[...dashboard.conversationAttempts,...tasks.map((task,i)=>({room:null,id:'worker-'+i,parentAttempt:'l',provider:'codex',role:'worker',state:'done',task}))]};
  const p=await panel(['one'],d,{locale:'en',context:true,tools:{
    oms_list_projects:()=>({projects:['one'],projectKeys:{one:key}}),
    oms_worker_output:a=>{
      const {project,...pair}=a,data={...pair,status:'complete'};
      return {structuredContent:data,_meta:{workerOutput:{...data,text:'Private selected output',source:'indexed-answer',truncated:false}}};
    }
  }});
  const group=p.get('leads').children[1].children[2],list=group.children.find(e=>e.tagName==='ul'),items=list.children.filter(e=>e.tagName==='li');
  assert.match(p.get('attention-summary').textContent,/Review task A/);
  for(const i of [1,0,2]){
    const open=items[i].children.find(e=>e.className?.includes('worker-output-link'));
    open.dispatchEvent(new Event('click'));await setImmediate();
    assert.equal(p.get('output-body').textContent,'Private selected output');
    const mark=p.calls.length;await p.event('output-followup','click');
    const draft=p.get('followup-text').value;
    assert.equal(p.get('worker-output').open,false);assert.equal(p.get('followup-panel').hidden,false);
    if(tasks[i])assert.match(draft,new RegExp('^Task: '+tasks[i]+'$','m'));else assert.doesNotMatch(draft,/^Task:/m);
    if(i!==0)assert.doesNotMatch(draft,/task-a|Review task A/);
    assert.doesNotMatch(draft,/Private selected output/);
    assert.match(draft,/Codex Worker · Worker output/);assert.ok(!draft.includes('worker-'+i) && !draft.includes(key));
    await p.event('followup-add','click');
    const context=p.host.contexts.at(-1).structuredContent;
    assert.equal(context.summary,'Codex Worker · Worker output');
    assert.ok(!JSON.stringify(p.host.contexts.at(-1)).includes('worker-'+i) && !JSON.stringify(p.host.contexts.at(-1)).includes(key));
    assert.equal(context.task,tasks[i]||undefined);assert.equal(Object.hasOwn(context,'task'),Boolean(tasks[i]));
    assert.equal(p.calls.length,mark);assert.equal(p.host.messages.length,0);
  }
});

test('only an independently confirmed physical binding retains a public stale snapshot and every write is fenced',async()=>{
  let failing=false,key='b'.repeat(64);
  const d={...dashboard,goal:'Last confirmed goal',updated:'2026-06-01T03:04:00Z'};
  const p=await panel(['one'],d,{locale:'en',tools:{
    oms_list_projects:()=>({projects:['one'],projectKeys:{one:key}}),
    oms_panel_snapshot:()=>{if(failing)throw Error('offline');return {dashboard:d};},
    [managedRead]:a=>{const v=leadStatus(a,'ready');if(a.provider)v._private={room:a.room,...v.leads[0],transcript:{turns:[{role:'claude',text:'Private answer'}]}};return v;}
  }});
  const open=p.get('leads').children[1].children[1].children.find(e=>e.type==='button');open.dispatchEvent(new Event('click'));await setImmediate();
  p.get('chat-text').value='Private draft';await p.event('chat-text','input');
  failing=true;await p.event('refresh','click');
  assert.equal(p.get('view').hidden,false);assert.equal(p.get('goal').textContent,'Last confirmed goal');assert.match(p.get('status').textContent,/Last confirmed state/);
  assert.match(p.get('observed-at').textContent,/Last confirmed.*refresh unconfirmed/);
  assert.equal(p.get('conversation').open,false);assert.equal(p.get('chat-text').value,'');assert.equal(p.get('chat-messages').textContent,'');
  assert.equal(p.get('new-goal').disabled,true);assert.equal(p.get('controls').disabled,true);
  const mark=p.calls.length;await p.event('new-goal','click');await p.event('open-leads','click');await p.event('followup-open','click');
  assert.ok(!p.get('launcher').open);assert.equal(p.calls.length,mark);
  await assert.rejects(p.run("tool('oms_start_main',{})"));assert.equal(p.calls.length,mark);
  failing=false;await p.event('refresh','click');assert.equal(p.get('new-goal').disabled,false);assert.equal(p.run('staleView'),false);
  failing=true;key='c'.repeat(64);await p.event('refresh','click');await setImmediate();
  assert.equal(p.get('view').hidden,true);assert.equal(p.run('dashboardData'),undefined);
});

const droppedFindingFields=['projectKey','room','owner','provider','participant','leadRequestId','operation','leadAttempt','workerAttempt','attempt','format','digest','pageCursor'];
function collectObjectKeys(value,keys=new Set()){
  if(!value || typeof value!=='object')return keys;
  if(Array.isArray(value)){for(const item of value)collectObjectKeys(item,keys);return keys;}
  for(const [k,v] of Object.entries(value)){keys.add(k);collectObjectKeys(v,keys);}
  return keys;
}
function assertPrivateFindingOmitted(delivered,internalValues=[],allowTurnId=false){
  const keys=collectObjectKeys(delivered);
  for(const field of droppedFindingFields)assert.equal(keys.has(field),false,field);
  if(!allowTurnId)assert.equal(keys.has('turnId'),false,'turnId');
  else if(delivered?.structuredContent?.source)assert.equal(Object.hasOwn(delivered.structuredContent.source,'turnId'),false,'source.turnId');
  const serialized=JSON.stringify(delivered);
  for(const internal of internalValues)assert.ok(!serialized.includes(internal),internal);
}

test('recorded Lead answer pages are lazy, digest-bound and prepare only an exact scoped draft',async()=>{
  const id=randomUUID(),copied=[],digest='d'.repeat(64);let changed=false,wrongHandoff=false;
  const claudeReq=leadStatus({provider:'claude'},'ready').leads[0].requestId,askParticipant='ask-'+claudeReq;
  const d={...dashboard,goal:'한'.repeat(2000),
    planReadiness:{available:true,scope:'repository',tasks:[{id:'task-a',title:'Review task A',reason:'parent-review-pending'}]},
    conversationAttempts:[...dashboard.conversationAttempts,{room:main.room,id:'other-operation',participant:askParticipant,provider:'claude',task:'task-a'}]
  };
  const p=await panel(['one','two'],d,{locale:'en',context:true,navigator:{clipboard:{writeText:async text=>copied.push(text)}},tools:{
    oms_list_projects:()=>({projects:['one','two'],projectKeys:{one:'b'.repeat(64),two:'c'.repeat(64)}}),
    [managedRead]:a=>{
      const v=leadStatus(a,'ready');
      if(a.provider){
        v._private={room:a.room,...v.leads[0],transcript:{turns:[{role:'user',text:'Question',turnId:id,status:'replied'},{role:'claude',text:'Excerpt',turnId:id,truncated:true}]}};
        if(a.answerPage!==undefined)v._private.answer={turnId:id,text:a.answerPage?'tail':'한글',offset:a.answerPage?6:0,bytes:a.answerPage?4:6,totalBytes:10,
          digest:changed?'e'.repeat(64):digest,nextCursor:a.answerPage?null:'next-part',hasMore:!a.answerPage,source:'recorded-answer',
          handoff:{room:a.room,participant:askParticipant,provider:a.provider,leadRequestId:a.leadRequestId,turnId:id,operation:wrongHandoff?'other-operation':'recorded-operation'}};
      }
      return v;
    }
  }});
  await p.open('input',{arguments:{project:'one'}});
  const open=p.get('leads').children[1].children[1].children.find(e=>e.type==='button');open.dispatchEvent(new Event('click'));await setImmediate();
  const more=p.get('chat-messages').children.at(-1).children.find(e=>e.tagName==='button');
  assert.ok(!p.calls.some(c=>c.args.answerPage!==undefined));
  more.dispatchEvent(new Event('click'));await setImmediate();
  assert.equal(p.calls.at(-1).args.answerPage,'');assert.equal(p.calls.at(-1).args.turnId,id);
  assert.equal(p.get('full-answer-body').textContent,'한글');assert.match(p.get('answer-status').textContent,/Showing bytes 1–6 \/ 10/);
  changed=true;await p.event('answer-more','click');
  assert.equal(p.get('full-answer-body').textContent,'');assert.match(p.get('answer-status').textContent,/changed/);assert.equal(p.get('answer-copy').hidden,true);
  changed=false;more.dispatchEvent(new Event('click'));await setImmediate();await p.event('answer-more','click');
  assert.equal(p.get('full-answer-body').textContent,'tail');assert.equal(p.get('answer-more').hidden,true);
  await p.event('answer-copy','click');assert.deepEqual(copied,['tail']);
  await p.event('answer-first','click');assert.equal(p.get('full-answer-body').textContent,'한글');assert.equal(p.get('answer-first').hidden,true);
  await p.event('answer-followup','click');assert.equal(p.get('conversation').open,false);assert.equal(p.get('followup-panel').hidden,false);
  assert.ok(p.get('followup-text').value.includes('한'.repeat(400)+'…'));assert.ok(Buffer.byteLength(p.get('followup-text').value,'utf8')<=4000);
  assert.equal(Boolean(p.get('followup-copy').disabled),false);assert.equal(Boolean(p.get('followup-add').disabled),false);
  await p.event('followup-copy','click');assert.ok(copied.at(-1).includes(id));assert.ok(copied.at(-1).includes('OMS Lead · Claude · Recorded output'));
  for(const internal of ['recorded-operation',askParticipant,claudeReq,'b'.repeat(64),digest,main.room])assert.ok(!copied.at(-1).includes(internal),internal);
  assert.equal(p.host.contexts.filter(c=>c.structuredContent?.kind==='oms-followup-draft').length,0);
  await p.event('followup-add','click');
  const leadDraft=p.host.contexts.at(-1).structuredContent;
  assert.equal(leadDraft.turnId,id);assert.equal(leadDraft.summary,'OMS Lead · Claude · Recorded output');assert.equal(p.host.messages.length,0);
  for(const internal of ['recorded-operation',askParticipant,claudeReq,'b'.repeat(64),digest,main.room])assert.ok(!JSON.stringify(p.host.contexts.at(-1)).includes(internal),internal);
  assert.equal(Object.hasOwn(leadDraft,'task'),false);assert.equal(Object.hasOwn(leadDraft,'source'),false);assert.equal(Object.hasOwn(leadDraft,'projectKey'),false);
  assert.doesNotMatch(p.get('followup-text').value,/^Task:|task-a|Review task A/m);
  d.conversationAttempts.push({room:main.room,id:'recorded-operation',participant:askParticipant,provider:'claude',task:'task-b'});
  await p.event('refresh','click');open.dispatchEvent(new Event('click'));await setImmediate();
  p.get('chat-messages').children.at(-1).children.find(e=>e.tagName==='button').dispatchEvent(new Event('click'));await setImmediate();
  await p.event('answer-followup','click');await p.event('followup-add','click');
  assert.equal(p.host.contexts.at(-1).structuredContent.task,'task-b');
  assert.match(p.get('followup-text').value,/^Task: task-b$/m);assert.doesNotMatch(p.get('followup-text').value,/task-a|Review task A/);
  open.dispatchEvent(new Event('click'));await setImmediate();
  p.get('chat-messages').children.at(-1).children.find(e=>e.tagName==='button').dispatchEvent(new Event('click'));await setImmediate();
  await p.event('answer-more','click');await p.event('answer-followup','click');
  // Ask about this result prepares references only; the excerpt waits for Include.
  assert.equal(p.get('finding-preview').hidden,true);assert.equal(p.get('finding-text').textContent,'');
  assert.equal(p.get('followup-include').hidden,false);await p.event('followup-include','click');
  assert.equal(p.get('followup-include').hidden,true);
  assert.equal(p.get('finding-text').textContent,'tail');assert.match(p.get('finding-range').textContent,/bytes 7–10 of 10.*partial/);
  assert.ok(!p.host.contexts.some(c=>c.structuredContent?.kind==='oms-selected-finding'));
  await p.event('finding-share','click');
  const deliveredLead=p.host.contexts.at(-1),shared=JSON.parse(JSON.stringify(deliveredLead.structuredContent));
  assert.deepEqual(shared,{kind:'oms-selected-finding',authority:'context-only',project:'one',task:'task-b',turnId:id,
    source:{kind:'lead-answer',summary:'OMS Lead · Claude · Recorded output'},
    selection:{text:'tail',offset:6,bytes:4,totalBytes:10,truncated:true,displayedBytes:4,
      excerptSha256:createHash('sha256').update('tail').digest('hex')},
    text:p.get('followup-text').value});
  assertPrivateFindingOmitted(deliveredLead,['recorded-operation',askParticipant,claudeReq,'b'.repeat(64),digest,main.room,main.participant,'next-part'],true);
  assert.doesNotMatch(JSON.stringify(shared),/Question|Excerpt/);assert.equal(p.host.messages.length,0);
  wrongHandoff=true;await p.event('finding-share','click');
  assert.equal(p.host.contexts.filter(c=>c.structuredContent?.kind==='oms-selected-finding').length,1);
  assert.equal(p.get('finding-text').textContent,'');assert.match(p.get('followup-status').textContent,/Nothing was shared/);wrongHandoff=false;
  open.dispatchEvent(new Event('click'));await setImmediate();const fresh=p.get('chat-messages').children.at(-1).children.find(e=>e.tagName==='button');
  p.host.delayTool=managedRead;fresh.dispatchEvent(new Event('click'));await setImmediate();
  await p.open('input',{arguments:{project:'two'}});p.host.release();await setImmediate();
  assert.equal(p.get('full-answer-body').textContent,'');assert.equal(p.get('full-answer').hidden,true);
});

test('Room result truncation links a bounded evidence read and keeps the reader position for new messages',async()=>{
  const d={...dashboard,planReadiness:{available:true,scope:'repository',tasks:[{id:'task-a',title:'Review task A',reason:'parent-review-pending'}]},
    conversationAttempts:dashboard.conversationAttempts.map(r=>r.id==='l'?{...r,task:'task-b'}:r)};
  let version=1,wrongAttempt=false,wrongKey=false;const digest='e'.repeat(64),key='b'.repeat(64),p=await panel(['one'],d,{locale:'en',context:true,tools:{
    oms_list_projects:()=>({projects:['one'],projectKeys:{one:key}}),
    oms_panel_chat:a=>a.section?{...a,projectKey:wrongKey?'c'.repeat(64):a.projectKey,source:'room',messages:[],evidence:{attempt:wrongAttempt?'another-attempt':a.attempt,section:a.section,text:a.evidenceCursor?'last':'first',offset:a.evidenceCursor?5:0,bytes:a.evidenceCursor?4:5,totalBytes:9,digest,nextCursor:a.evidenceCursor?null:'next',hasMore:!a.evidenceCursor}}:
      {...a,source:'room',messages:Array.from({length:version},(_,i)=>({seq:i+1,text:'Message '+(i+1)})),result:{attempt:'l',text:'Result excerpt',truncated:true}}
  }});
  const scroll=p.get('chat-scroll');Object.defineProperty(scroll,'scrollHeight',{value:500});scroll.clientHeight=100;
  p.get('leads').children[1].children[2].dispatchEvent(new Event('click'));await setImmediate();
  assert.equal(p.get('chat-result-limited').hidden,false);assert.equal(p.get('chat-result-full').hidden,false);
  scroll.scrollTop=10;version=2;await p.event('chat-refresh','click');
  assert.equal(scroll.scrollTop,10);assert.equal(p.get('chat-new').hidden,false);assert.equal(p.get('chat-messages').children.length,2);
  await p.event('chat-new','click');assert.equal(scroll.scrollTop,500);
  await p.event('chat-result-full','click');
  assert.deepEqual({...p.calls.at(-1).args},{project:'one',projectKey:key,room:main.room,participant:lead.participant,section:'transcript',attempt:'l',evidenceCursor:''});
  assert.equal(p.get('full-answer-body').textContent,'first');await p.event('answer-more','click');
  assert.equal(p.get('full-answer-body').textContent,'last');assert.equal(p.get('answer-more').hidden,true);
  await p.event('answer-followup','click');await p.event('followup-add','click');
  assert.equal(p.host.contexts.at(-1).structuredContent.task,'task-b');
  assert.equal(p.host.contexts.at(-1).structuredContent.summary,'OMS Lead · Claude · Recorded output');
  for(const internal of [key,digest,lead.participant,main.room])assert.ok(!JSON.stringify(p.host.contexts.at(-1)).includes(internal),internal);
  assert.match(p.get('followup-text').value,/^Task: task-b$/m);assert.doesNotMatch(p.get('followup-text').value,/task-a|Review task A/);
  p.get('leads').children[1].children[2].dispatchEvent(new Event('click'));await setImmediate();
  await p.event('chat-result-full','click');await p.event('answer-more','click');await p.event('answer-followup','click');await p.event('followup-include','click');
  assert.equal(p.get('finding-text').textContent,'last');await p.event('finding-share','click');
  const deliveredRoom=p.host.contexts.at(-1),shared=JSON.parse(JSON.stringify(deliveredRoom.structuredContent));
  assert.deepEqual(shared,{kind:'oms-selected-finding',authority:'context-only',project:'one',task:'task-b',
    source:{kind:'room-result',summary:'OMS Lead · Claude · Recorded output'},
    selection:{text:'last',offset:5,bytes:4,totalBytes:9,truncated:true,displayedBytes:4,
      excerptSha256:createHash('sha256').update('last').digest('hex')},
    text:p.get('followup-text').value});
  assertPrivateFindingOmitted(deliveredRoom,[key,digest,lead.participant,main.room,main.participant]);
  assert.doesNotMatch(JSON.stringify(shared),/Message [12]|Result excerpt/);assert.equal(p.host.messages.length,0);
  p.get('leads').children[1].children[2].dispatchEvent(new Event('click'));await setImmediate();
  wrongAttempt=true;await p.event('chat-result-full','click');
  assert.equal(p.get('full-answer-body').textContent,'');assert.match(p.get('answer-status').textContent,/changed/);assert.equal(p.get('answer-followup').hidden,true);assert.equal(p.get('answer-copy').hidden,true);
  wrongAttempt=false;wrongKey=true;await p.event('chat-result-full','click');
  assert.equal(p.get('full-answer-body').textContent,'');assert.equal(p.get('answer-followup').hidden,true);
});

test('worker full output pages keep the exact pair and reject a changed digest',async()=>{
  const child={room:null,id:'child',parentAttempt:'l',provider:'codex',role:'worker',state:'done'},d={...dashboard,conversationAttempts:[...dashboard.conversationAttempts,child]};
  let changed=false,emptyPrefix=false;const p=await panel(['one'],d,{locale:'en',tools:{oms_worker_output:a=>{
    const {project,answerPage,...pair}=a,publicData={...pair,status:'complete'};
    const part=answerPage==='tail'?2:answerPage?1:0;
    const out={...publicData,text:answerPage===undefined?(emptyPrefix?' \n\t ':'Excerpt'):['head',' \n\t ','tail'][part],source:'indexed-answer',truncated:part<2};
    if(answerPage!==undefined)out.page={offset:part*4,bytes:4,totalBytes:12,digest:(changed?'b':'a').repeat(64),nextCursor:part===0?'next':part===1?'tail':null,hasMore:part<2};
    return {structuredContent:publicData,_meta:{workerOutput:out}};
  }}});
  const group=p.get('leads').children[1].children[2],list=group.children.find(e=>e.tagName==='ul'),item=list.children.find(e=>e.tagName==='li'),open=item.children.find(e=>e.className?.includes('worker-output-link'));
  open.dispatchEvent(new Event('click'));await setImmediate();assert.equal(p.get('output-more').hidden,false);
  await p.event('output-more','click');assert.equal(p.calls.at(-1).args.answerPage,'');assert.equal(p.get('output-body').textContent,'head');
  changed=true;await p.event('output-more','click');assert.equal(p.get('output-body').textContent,'');assert.match(p.get('output-status').textContent,/changed/);
  assert.equal(p.get('output-copy').hidden,true);
  changed=false;await p.event('output-refresh','click');await p.event('output-more','click');await p.event('output-more','click');
  assert.equal(p.get('output-body').textContent,' \n\t ');assert.match(p.get('output-status').textContent,/Showing bytes 5–8 \/ 12/);assert.equal(p.get('output-more').hidden,false);
  await p.event('output-more','click');assert.equal(p.get('output-body').textContent,'tail');assert.equal(p.get('output-more').hidden,true);
  emptyPrefix=true;await p.event('output-refresh','click');assert.equal(p.get('output-body').textContent,'');assert.equal(p.get('output-copy').hidden,true);
  assert.ok(p.calls.filter(c=>c.name==='oms_worker_output').every(c=>c.args.leadAttempt==='l' && c.args.workerAttempt==='child'));
});

async function findingFixture(){
  const key='b'.repeat(64),digest='d'.repeat(64),displayed='한'.repeat(2000),tail='PRIVATE OFF-PAGE CONTENT';
  const workers=['task-a','task-b'].map((task,i)=>({id:'worker-'+i,parentAttempt:'l',provider:'codex',role:'worker',state:'done',task}));
  const d={...dashboard,goal:'Review selected findings',conversationAttempts:[...dashboard.conversationAttempts,...workers],
    planReadiness:{available:true,scope:'repository',tasks:[{id:'task-a',title:'Review task A',reason:'parent-review-pending'}]}};
  const state={key,readOnly:false,snapshot:d,fault:null};
  const p=await panel(['one','two'],d,{locale:'en',context:true,tools:{
    oms_list_projects:()=>({projects:['one','two'],projectKeys:{one:state.key,two:'c'.repeat(64)},readOnly:state.readOnly}),
    oms_panel_snapshot:()=>{if(state.fault==='offline')throw Error('offline');return {dashboard:state.snapshot};},
    oms_worker_output:a=>{
      const {project,answerPage,...pair}=a,publicData={...pair,status:'complete'};
      if(state.fault==='pair')publicData.workerAttempt='worker-0';
      const text=state.fault==='body'?'x'+displayed.slice(1):answerPage==='next'?tail:displayed;
      const out={...publicData,text,source:state.fault==='format'?'native-report':'indexed-answer',truncated:answerPage!=='next'};
      if(answerPage!==undefined)out.page={offset:answerPage==='next'?Buffer.byteLength(displayed):0,bytes:Buffer.byteLength(text),
        totalBytes:Buffer.byteLength(displayed+tail),digest:(state.fault==='digest'?'e'.repeat(64):digest),nextCursor:answerPage==='next'?null:'next',hasMore:answerPage!=='next'};
      return {structuredContent:publicData,_meta:{workerOutput:out}};
    }
  }});
  await p.open('input',{arguments:{project:'one'}});
  const list=p.get('leads').children[1].children[2].children.find(e=>e.tagName==='ul');
  const open=list.children.filter(e=>e.tagName==='li')[1].children.find(e=>e.className?.includes('worker-output-link'));
  open.dispatchEvent(new Event('click'));await setImmediate();await p.event('output-followup','click');
  assert.equal(p.get('finding-preview').hidden,true);assert.ok(!p.calls.some(c=>c.name==='oms_worker_output' && c.args.answerPage!==undefined));
  await p.event('followup-include','click');
  assert.equal(p.get('finding-preview').hidden,false);assert.equal(p.get('worker-output').open,false);
  return {p,state,d,key,digest,displayed,tail};
}

test('selected worker findings stay private through preview and references-only drafts until an exact bounded Share',async()=>{
  const {p,key,digest,displayed,tail}=await findingFixture(),selection='한'.repeat(1333);
  assert.match(p.get('attention-summary').textContent,/Review task A/);
  assert.equal(p.get('finding-text').textContent,selection);assert.equal(Buffer.byteLength(selection),3999);
  assert.match(p.get('finding-range').textContent,/Sharing bytes 1–3999 of 6024.*partial result/);
  assert.match(p.get('finding-range').textContent,/first 3999 bytes/);
  assert.doesNotMatch(JSON.stringify(p.host.contexts),/한|PRIVATE OFF-PAGE/);
  assert.match(p.get('followup-text').value,/^Task: task-b$/m);assert.doesNotMatch(p.get('followup-text').value,/task-a|Review task A/);
  await p.event('followup-add','click');
  assert.equal(p.host.contexts.at(-1).structuredContent.kind,'oms-followup-draft');
  assert.doesNotMatch(JSON.stringify(p.host.contexts),/한|PRIVATE OFF-PAGE/);
  const mark=p.calls.length;await p.event('finding-share','click');
  const deliveredWorker=p.host.contexts.at(-1),shared=JSON.parse(JSON.stringify(deliveredWorker.structuredContent));
  assert.deepEqual(shared,{kind:'oms-selected-finding',authority:'context-only',project:'one',task:'task-b',
    source:{kind:'worker-output',summary:'Codex Worker · Worker output'},
    selection:{text:selection,offset:0,bytes:3999,totalBytes:Buffer.byteLength(displayed+tail),truncated:true,
      displayedBytes:6000,excerptSha256:createHash('sha256').update(selection).digest('hex')},
    text:p.get('followup-text').value});
  assertPrivateFindingOmitted(deliveredWorker,[key,digest,main.room,main.participant,'worker-1','indexed-answer']);
  assert.deepEqual(p.calls.slice(mark).map(c=>c.name),['oms_list_projects','oms_panel_snapshot','oms_worker_output','oms_list_projects','oms_panel_snapshot']);
  assert.equal(p.calls.slice(mark)[2].args.answerPage,'');assert.equal(p.host.messages.length,0);
  assert.doesNotMatch(JSON.stringify(shared),/PRIVATE OFF-PAGE/);assert.match(p.get('followup-status').textContent,/no execution authority/);
});

test('finding Share rejects changed source, binding, Room, task, read-only state and missing context capability',async()=>{
  for(const fault of ['digest','body','pair','format','binding','room','owner','task','readOnly','capability','offline']){
    const {p,state,d}=await findingFixture();
    if(fault==='binding')state.key='c'.repeat(64);
    else if(fault==='readOnly')state.readOnly=true;
    else if(fault==='capability')p.run('contextTypes={};paintFollowup()');
    else if(['room','owner'].includes(fault))state.snapshot={...d,conversationMain:{...d.conversationMain,[fault==='room'?'room':'participant']:'changed'}};
    else if(fault==='task')state.snapshot={...d,conversationAttempts:d.conversationAttempts.map(a=>a.id==='worker-1'?{...a,task:'task-a'}:a)};
    else state.fault=fault;
    await p.event('finding-share','click');
    assert.ok(!p.host.contexts.some(c=>c.structuredContent?.kind==='oms-selected-finding'),fault);
    assert.doesNotMatch(JSON.stringify(p.host.contexts),/한|PRIVATE OFF-PAGE/,fault);assert.equal(p.host.messages.length,0,fault);
    if(fault==='capability')assert.equal(p.get('finding-share').hidden,true);
    else {assert.equal(p.get('finding-text').textContent,'',fault);assert.match(p.get('followup-status').textContent,/Nothing was shared/,fault);}
  }
});

test('pending finding Share is fenced by project changes, teardown, source-scope changes and draft edits; text-only hosts remain supported',async()=>{
  for(const change of ['project','teardown','room','draft','stale']){
    const {p,state,d}=await findingFixture();p.host.delayTool='oms_worker_output';await p.event('finding-share','click');
    const before=p.calls.length;
    if(change==='project')await p.open('input',{arguments:{project:'two'}});
    else if(change==='teardown')p.notify({method:'ui/resource-teardown',id:'close-finding',params:{}});
    else if(change==='room')state.snapshot={...d,conversationMain:{...d.conversationMain,room:'room-b'}};
    else if(change==='stale'){state.fault='offline';await p.event('refresh','click');}
    else {p.get('followup-text').value='Edited after clicking Share';await p.event('followup-text','input');}
    p.host.release();await setImmediate();
    assert.ok(!p.host.contexts.some(c=>c.structuredContent?.kind==='oms-selected-finding'),change);
    assert.equal(p.get('finding-text').textContent,'',change);
    if(change==='teardown')assert.equal(p.calls.length,before);
  }
  const {p,key,digest}=await findingFixture();p.run('contextTypes={text:{}}');await p.event('finding-share','click');
  const deliveredText=p.host.contexts.at(-1),text=JSON.parse(deliveredText.content[0].text);
  assert.equal(text.kind,'oms-selected-finding');assert.equal(text.selection.bytes,3999);assert.equal(text.authority,'context-only');
  assertPrivateFindingOmitted(deliveredText,[key,digest,main.room,main.participant,'worker-1','indexed-answer']);
  assertPrivateFindingOmitted(text,[key,digest,main.room,main.participant,'worker-1','indexed-answer']);
});

test('worker task attribution resolves Share rechecks only from the fresh snapshot and refuses unattributable snapshots',async()=>{
  const key='b'.repeat(64),digest='d'.repeat(64);
  const memberWorkers=[0,1].map(i=>({room:main.room,participant:'member-worker-'+i,parent:lead.participant,role:'worker',provider:'codex',joined:true}));
  const workerAttempts=[{room:main.room,id:'mw-0',participant:'member-worker-0',role:'worker',provider:'codex',state:'done',task:'task-member'},
    {room:main.room,id:'mw-1',participant:'member-worker-1',role:'worker',provider:'codex',state:'done',task:null}];
  const d={...dashboard,conversationMain:{...main,participants:[lead,...memberWorkers]},
    planReadiness:{available:true,scope:'repository',tasks:[{id:'task-a',title:'Review task A',reason:'parent-review-pending'}]},
    conversationAttempts:[...dashboard.conversationAttempts,...workerAttempts]};
  let snapshot=d;
  const p=await panel([],d,{locale:'en',context:true,tools:{
    oms_list_projects:()=>({projects:[],binding:'pinned',projectKey:key}),
    oms_panel_snapshot:()=>({dashboard:snapshot}),
    oms_worker_output:a=>{
      const {project,answerPage,...pair}=a,publicData={...pair,status:'complete'},body='Member worker finding';
      const out={...publicData,text:body,source:'indexed-answer',truncated:false};
      if(answerPage!==undefined)out.page={offset:0,bytes:Buffer.byteLength(body),totalBytes:Buffer.byteLength(body),digest,nextCursor:null,hasMore:false};
      return {structuredContent:publicData,_meta:{workerOutput:out}};
    }
  }});
  const open=i=>p.get('leads').children[1].children[2].children.find(e=>e.tagName==='ul').children.filter(e=>e.tagName==='li')[i]
    .children.find(e=>e.className?.includes('worker-output-link'));
  open(0).dispatchEvent(new Event('click'));await setImmediate();await p.event('output-followup','click');
  assert.match(p.get('followup-text').value,/^Task: task-member$/m);
  assert.doesNotMatch(p.get('followup-text').value,/task-a|Review task A/);
  await p.event('followup-add','click');
  assert.equal(p.host.contexts.at(-1).structuredContent.task,'task-member');
  assert.equal(Object.hasOwn(p.host.contexts.at(-1).structuredContent,'project'),false);
  // Fresh snapshot with member-parent workers resolves task-member from its own rows[].workers.
  open(0).dispatchEvent(new Event('click'));await setImmediate();await p.event('output-followup','click');await p.event('followup-include','click');
  await p.event('finding-share','click');
  const sharedMember=p.host.contexts.at(-1);
  assert.equal(sharedMember.structuredContent.kind,'oms-selected-finding');
  assert.equal(sharedMember.structuredContent.task,'task-member');
  assert.equal(Object.hasOwn(sharedMember.structuredContent,'project'),false);
  assertPrivateFindingOmitted(sharedMember,[key,digest,main.room,main.participant,'mw-0','member-worker-0','indexed-answer']);
  // Fresh snapshot confirms unattributed member-parent worker when present with task: null.
  open(1).dispatchEvent(new Event('click'));await setImmediate();await p.event('output-followup','click');await p.event('followup-include','click');
  assert.doesNotMatch(p.get('followup-text').value,/^Task:|task-a|Review task A/m);
  await p.event('followup-add','click');
  assert.equal(Object.hasOwn(p.host.contexts.at(-1).structuredContent,'task'),false);
  await p.event('finding-share','click');
  const sharedUnattributed=p.host.contexts.at(-1);
  assert.equal(sharedUnattributed.structuredContent.kind,'oms-selected-finding');
  assert.equal(Object.hasOwn(sharedUnattributed.structuredContent,'task'),false);
  assertPrivateFindingOmitted(sharedUnattributed,[key,digest,main.room,main.participant,'mw-1','member-worker-1','indexed-answer']);
  // Fresh snapshot without conversationAttempts (or missing the worker, or changed parent/task) refuses Share as changed instead of reading rendered rows.
  for(const [index,fresh] of [
    [0,{conversationBinding:'matched',conversationMain:d.conversationMain}],
    [1,{conversationBinding:'matched',conversationMain:d.conversationMain}],
    [1,{...d,conversationAttempts:d.conversationAttempts.filter(a=>a.id!=='mw-1')}],
    [0,{...d,conversationMain:{...d.conversationMain,participants:[lead,{...memberWorkers[0],parent:'other-lead'},memberWorkers[1]]}}],
    [0,{...d,conversationAttempts:d.conversationAttempts.map(a=>a.id==='mw-0'?{...a,task:'task-a'}:a)}],
    [1,{...d,conversationAttempts:d.conversationAttempts.map(a=>a.id==='mw-1'?{...a,task:'task-a'}:a)}]
  ]){
    snapshot=d;await p.event('refresh','click');
    open(index).dispatchEvent(new Event('click'));await setImmediate();await p.event('output-followup','click');await p.event('followup-include','click');
    snapshot=fresh;
    const sharedBefore=p.host.contexts.filter(c=>c.structuredContent?.kind==='oms-selected-finding').length;
    await p.event('finding-share','click');
    assert.equal(p.host.contexts.filter(c=>c.structuredContent?.kind==='oms-selected-finding').length,sharedBefore);
    assert.equal(p.get('finding-text').textContent,'');
    assert.match(p.get('followup-status').textContent,/Nothing was shared/);
  }
});

const inspectReads=['oms_panel_usage','oms_panel_tui_view','oms_panel_project_graph'];
function inspectionTools(state){
  return {
    oms_list_projects:()=>({projects:['one','two'],projectKeys:{one:state.key,two:'c'.repeat(64)},readOnly:state.readOnly}),
    oms_panel_usage:a=>{
      assert.deepEqual(Object.keys(a),['project']);
      return {scope:'repository-recent-attempts',providers:[{provider:'codex',contextUsed:0,cacheReadShare:null,windowsOmitted:1,
        windows:[{name:'5 hour',used:0,source:'cached observation',stale:true,resets_at:null,age_seconds:12}],
        recent7d:{calls:1,input:0,output:17,cacheRead:null}}],attempts:[
        {id:'reported-zero',provider:'codex',tokens:0,source:'receipt',reportedCalls:1,calls:1,breakdown:[],costMicrousd:1500000,costSource:'events'},
        {id:'unreported',provider:'claude',tokens:null,source:null,reportedCalls:null,calls:null,breakdown:[],costMicrousd:null,costSource:null}],
        attemptsOmitted:0,providerTotals:[
          {provider:'claude',calls:10,tokens:2048,tokenReports:9,reportedCostMicrousd:null,reportedCostCalls:0,estimatedCostMicrousd:1545000,estimatedCostCalls:9},
          {provider:'antigravity',calls:4,tokens:512,tokenReports:3,reportedCostMicrousd:null,reportedCostCalls:0,estimatedCostMicrousd:null,estimatedCostCalls:0}
        ],providerTotalsOmitted:0,diagnostic:'PRIVATE DEBUG JSON'};
    },
    oms_panel_tui_view:a=>{
      assert.deepEqual(Object.keys(a).sort(),['page','project','view']);assert.ok(['routes','modelcatalog'].includes(a.view));
      return {view:a.view,page:a.page,status:'ready',nextPage:a.view==='routes' && a.page===1?2:null,incomplete:false,
        columns:[{key:a.view==='routes'?'role':'model',label:'항목'}],rows:a.view==='routes'?
        [{role:'reviewer '+a.page,access:'read-only',tier:'review',status:'ready',provider:'claude',model:'fixture-model',effort:'high',
          requestedModel:null,requestedEffort:null,requestedAccount:null,selectedAccount:null,versionEvidence:'cached',probeAge:12,inFlight:0,recentAttempts:4,issue:null}]:
        [{provider:'codex',model:'cached-model',efforts:null,providerEfforts:'low, high',routable:null,source:'oms models · cache',probe:'unavailable',age:null}]};
    },
    oms_panel_project_graph:a=>{
      assert.ok(Object.keys(a).every(k=>['project','node','query','depth','limit'].includes(k)));
      assert.equal(a.depth,2);assert.equal(a.limit,20);assert.ok(!(a.node && a.query));
      return {schema:1,kind:'oms-panel-project-graph',status:'ready',completeness:'complete',
        provenance:{source:'indexed-graph',mode:'indexed',indexState:'present',indexFreshness:'fresh',verifiedRelationships:true,readOnly:true,wroteIndex:false},
        scope:{repository:'fixture',focusNode:a.node||null,query:a.query||null,includeTests:false,depth:a.depth,limit:a.limit},
        limits:{maxNodes:20,maxEdges:180,truncated:false},counts:{shown_nodes:2,total_nodes:2,shown_edges:2,total_edges:2,confidence:{EXTRACTED:1,INFERRED:1}},
        nodes:[{id:'file:src/a.rs',label:'a.rs',kind:'file',path:'src/a.rs'},{id:'file:src/b.rs',label:'b.rs',kind:'file',path:'src/b.rs'}],
        edges:[{source:'file:src/a.rs',target:'file:src/b.rs',relation:'imports',confidence:'EXTRACTED'},
          {source:'file:src/b.rs',target:'file:src/a.rs',relation:'related',confidence:'INFERRED'}],selection:null};
    }
  };
}

test('Inspect reads only the selected view and renders usage, paged routes, catalog evidence and graph neighborhoods',async()=>{
  const state={key:'b'.repeat(64)},p=await panel(['one','two'],dashboard,{locale:'en',tools:inspectionTools(state)});
  await p.open('input',{arguments:{project:'one'}});
  assert.ok(!p.calls.some(c=>inspectReads.includes(c.name)));assert.equal(p.get('inspect-open').disabled,false);
  await p.event('inspect-open','click');
  assert.deepEqual(p.calls.filter(c=>inspectReads.includes(c.name)).map(c=>c.name),['oms_panel_usage']);
  const content=p.get('inspect-content');
  assert.match(content.textContent,/Observed context used0%Cache read shareStatus unknown/);
  assert.match(content.textContent,/tokens in the last 7 days/);
  assert.match(content.textContent,/Left100%Used0%.*Observed agejust now/);
  assert.match(content.textContent,/Claude · repository totals.*Reported calls10Reported tokens2,048Token-reported calls9Estimated cost \(USD\)1\.545/);
  assert.match(content.textContent,/AGY · repository totals.*Reported calls4Reported tokens512Token-reported calls3/);
  assert.match(content.textContent,/Reported tokens0SourceUsage receipt/);
  assert.match(content.textContent,/Reported cost \(USD\)1.5/);assert.match(content.textContent,/Reported tokensStatus unknown/);
  assert.doesNotMatch(content.textContent,/PRIVATE DEBUG JSON|"providers"|"scope"/);
  assert.match(p.get('inspect-status').textContent,/Only part/);assert.match(p.get('inspect-note').textContent,/not this Room alone/);
  p.get('inspect-kind').value='routes';await p.event('inspect-kind','change');
  assert.match(content.textContent,/reviewer 1/);assert.equal(p.get('inspect-prev').hidden,true);assert.equal(p.get('inspect-next').hidden,false);
  assert.match(p.get('inspect-note').textContent,/proposals/);await p.event('inspect-next','click');
  assert.match(content.textContent,/reviewer 2/);assert.equal(p.get('inspect-prev').hidden,false);assert.equal(p.get('inspect-next').hidden,true);
  await p.event('inspect-prev','click');assert.match(content.textContent,/reviewer 1/);
  p.get('inspect-kind').value='modelcatalog';await p.event('inspect-kind','change');
  assert.match(content.textContent,/cached-model/);assert.match(content.textContent,/Model-specific effortsStatus unknownProvider-wide effortslow, high/);
  assert.match(p.get('inspect-note').textContent,/does not guarantee account access/);assert.equal(p.get('inspect-next').hidden,true);
  p.get('inspect-kind').value='graph';await p.event('inspect-kind','change');
  assert.equal(p.get('inspect-graph-form').hidden,false);assert.match(content.textContent,/Cached project relationships/);
  assert.match(content.textContent,/imports · Extracted/);assert.match(content.textContent,/related · Inferred/);
  const nodes=content.children[0].children.find(e=>e.className==='inspect-nodes');nodes.children.find(e=>e.tagName==='button').dispatchEvent(new Event('click'));await setImmediate();
  assert.deepEqual({...p.calls.filter(c=>c.name==='oms_panel_project_graph').at(-1).args},{project:'one',node:'file:src/a.rs',limit:20,depth:2});
  p.get('inspect-query').value='src';await p.event('inspect-graph-form','submit');
  assert.deepEqual({...p.calls.filter(c=>c.name==='oms_panel_project_graph').at(-1).args},{project:'one',query:'src',limit:20,depth:2});
  await p.event('inspect-root','click');assert.equal(Object.hasOwn(p.calls.filter(c=>c.name==='oms_panel_project_graph').at(-1).args,'query'),false);
  await choose(p,'ko');assert.match(content.textContent,/추출됨/);assert.match(p.get('inspect-note').textContent,/관계/);
  await p.event('inspect-close','click');assert.equal(content.textContent,'');assert.equal(p.get('inspection').open,false);
  assert.equal(p.host.messages.length,0);assert.ok(!p.calls.some(c=>/^oms_(start|send|update|open_lead|assign)/.test(c.name)));
  const readOnly=await panel(['one'],dashboard,{locale:'en',tools:inspectionTools({key:'b'.repeat(64),readOnly:true})});
  await readOnly.open('input',{arguments:{project:'one'}});await readOnly.event('inspect-open','click');
  assert.match(readOnly.get('inspect-content').textContent,/provider observation/);assert.equal(readOnly.get('followup-open').hidden,true);
});

test('Inspect drops late views and private scope changes, and clears unverifiable or malformed results',async()=>{
  const state={key:'b'.repeat(64)},p=await panel(['one','two'],dashboard,{locale:'en',tools:inspectionTools(state)});
  await p.open('input',{arguments:{project:'one'}});await p.event('inspect-open','click');
  p.host.delayTool='oms_panel_project_graph';p.get('inspect-kind').value='graph';await p.event('inspect-kind','change');
  p.get('inspect-kind').value='routes';await p.event('inspect-kind','change');p.host.release();await setImmediate();
  assert.match(p.get('inspect-content').textContent,/reviewer 1/);assert.doesNotMatch(p.get('inspect-content').textContent,/a.rs|Cached project/);
  p.host.delayTool='oms_panel_usage';p.get('inspect-kind').value='usage';await p.event('inspect-kind','change');
  state.key='d'.repeat(64);p.host.release();await setImmediate();
  assert.equal(p.get('inspect-content').textContent,'');assert.match(p.get('inspect-status').textContent,/Read result unconfirmed/);
  state.key='b'.repeat(64);await p.event('inspect-refresh','click');assert.match(p.get('inspect-content').textContent,/provider observation/);
  p.host.delayTool='oms_panel_usage';await p.event('inspect-refresh','click');const before=p.calls.length;
  await p.event('inspect-close','click');p.host.release();await setImmediate();
  assert.equal(p.calls.length,before);assert.equal(p.get('inspect-content').textContent,'');
  p.host.delayTool='oms_panel_usage';await p.event('inspect-open','click');
  await p.open('input',{arguments:{project:'two'}});p.host.release();await setImmediate();
  assert.equal(p.get('inspection').open,false);assert.equal(p.get('inspect-content').textContent,'');
  await p.open('input',{arguments:{project:'one'}});await p.event('inspect-open','click');
  const good=p.host.tools.oms_panel_tui_view;
  for(const fault of ['view','page','nextPage','rows']){
    p.host.tools.oms_panel_tui_view=a=>{const v=good(a);if(fault==='view')v.view='plan';if(fault==='page')v.page++;
      if(fault==='nextPage')v.nextPage=999;if(fault==='rows')v.rows=Array(21).fill(v.rows[0]);return v;};
    p.get('inspect-kind').value='routes';await p.event('inspect-kind','change');
    assert.equal(p.get('inspect-content').textContent,'',fault);assert.match(p.get('inspect-status').textContent,/Read result unconfirmed/,fault);
    assert.equal(p.get('inspect-next').hidden,true,fault);
  }
  const graph=p.host.tools.oms_panel_project_graph;p.host.tools.oms_panel_project_graph=a=>({...graph(a),scope:{focusNode:'different',query:null}});
  p.get('inspect-kind').value='graph';await p.event('inspect-kind','change');
  assert.equal(p.get('inspect-content').textContent,'');assert.match(p.get('inspect-status').textContent,/Read result unconfirmed/);
  p.host.tools.oms_panel_usage=()=>({scope:'repository-recent-attempts',providers:[],attempts:[],providerTotals:Array.from({length:33},(_,i)=>({provider:'p'+i,calls:1}))});
  p.get('inspect-kind').value='usage';await p.event('inspect-kind','change');
  assert.equal(p.get('inspect-content').textContent,'');assert.match(p.get('inspect-status').textContent,/Read result unconfirmed/);
  p.host.tools.oms_panel_usage=()=>({scope:'repository-recent-attempts',providers:[],attempts:[]});
  await p.event('inspect-refresh','click');assert.match(p.get('inspect-content').textContent,/No records/);
});

test('Inspect graph distinguishes unavailable provenance from zero cached relationships',async()=>{
  const state={key:'b'.repeat(64)},tools=inspectionTools(state),good=tools.oms_panel_project_graph;
  tools.oms_panel_project_graph=a=>({...good(a),status:'unknown',completeness:'unknown',
    provenance:{source:'git-ls-files',mode:'unavailable',indexState:'unindexed',indexFreshness:'absent',verifiedRelationships:false,readOnly:true,wroteIndex:false},
    counts:{shown_nodes:0,total_nodes:0,shown_edges:0,total_edges:0,confidence:{}},nodes:[],edges:[]});
  const p=await panel(['one'],dashboard,{locale:'en',tools});
  await p.open('input',{arguments:{project:'one'}});await p.event('inspect-open','click');
  p.get('inspect-kind').value='graph';await p.event('inspect-kind','change');
  assert.equal(p.get('inspect-content').textContent,'');
  assert.doesNotMatch(p.get('inspect-content').textContent,/Cached project relationships|0 \/ 0|No records/);
  assert.match(p.get('inspect-status').textContent,/Status unknown/);
  await choose(p,'ko');assert.match(p.get('inspect-status').textContent,/상태 미확인/);
});

test('Include stays hidden on hosts without updateModelContext',async()=>{
  const key='b'.repeat(64),id=randomUUID(),digest='d'.repeat(64);
  const claudeReq=leadStatus({provider:'claude'},'ready').leads[0].requestId,askParticipant='ask-'+claudeReq;
  const worker={id:'worker-0',parentAttempt:'l',provider:'codex',role:'worker',state:'done'};
  const d={...dashboard,conversationAttempts:[...dashboard.conversationAttempts,worker]};
  const p=await panel(['one'],d,{locale:'en',tools:{
    oms_list_projects:()=>({projects:['one'],projectKeys:{one:key}}),
    oms_worker_output:a=>{
      const {project,...pair}=a,publicData={...pair,status:'complete'};
      return {structuredContent:publicData,_meta:{workerOutput:{...publicData,text:'Worker answer',source:'indexed-answer',truncated:false}}};
    },
    [managedRead]:a=>{
      const v=leadStatus(a,'ready');
      if(a.provider){
        v._private={room:a.room,...v.leads[0],transcript:{turns:[{role:'user',text:'Q',turnId:id,status:'replied'},{role:'claude',text:'A',turnId:id,truncated:true}]}};
        if(a.answerPage!==undefined)v._private.answer={turnId:id,text:'Full Lead answer',offset:0,bytes:16,totalBytes:16,digest,nextCursor:null,hasMore:false,
          source:'recorded-answer',handoff:{room:a.room,participant:askParticipant,provider:a.provider,leadRequestId:a.leadRequestId,turnId:id,operation:'recorded-operation'}};
      }
      return v;
    },
    oms_panel_chat:a=>a.section?{...a,projectKey:a.projectKey,source:'room',messages:[],evidence:{attempt:a.attempt,section:a.section,text:'Room full',offset:0,bytes:9,totalBytes:9,digest,nextCursor:null,hasMore:false}}:
      {...a,source:'room',messages:[],result:{attempt:'l',text:'Room excerpt',truncated:true}}
  }});
  const group=p.get('leads').children[1].children[2],list=group.children.find(e=>e.tagName==='ul'),openWorker=list.children.find(e=>e.tagName==='li').children.find(e=>e.className?.includes('worker-output-link'));
  openWorker.dispatchEvent(new Event('click'));await setImmediate();
  assert.equal(p.get('output-copy').hidden,false);assert.equal(p.get('output-followup').hidden,false);
  await p.event('output-followup','click');assert.equal(p.get('followup-include').hidden,true);
  const openLead=p.get('leads').children[1].children[1].children.find(e=>e.type==='button');openLead.dispatchEvent(new Event('click'));await setImmediate();
  p.get('chat-messages').children.at(-1).children.find(e=>e.tagName==='button').dispatchEvent(new Event('click'));await setImmediate();
  assert.equal(p.get('answer-copy').hidden,false);assert.equal(p.get('answer-followup').hidden,false);
  await p.event('answer-followup','click');assert.equal(p.get('followup-include').hidden,true);
  group.children.find(e=>e.className==='person').dispatchEvent(new Event('click'));await setImmediate();await p.event('chat-result-full','click');
  assert.equal(p.get('answer-copy').hidden,false);assert.equal(p.get('answer-followup').hidden,false);
  await p.event('answer-followup','click');assert.equal(p.get('followup-include').hidden,true);
});

test('a failed Include keeps the excerpt private, and a superseded Include never fills a newer draft',async()=>{
  const key='b'.repeat(64),digest='d'.repeat(64);let failPage=true;
  const workers=[0,1].map(i=>({id:'worker-'+i,parentAttempt:'l',provider:'codex',role:'worker',state:'done'}));
  const d={...dashboard,conversationAttempts:[...dashboard.conversationAttempts,...workers]};
  const p=await panel(['one'],d,{locale:'en',context:true,tools:{
    oms_list_projects:()=>({projects:['one'],projectKeys:{one:key}}),
    oms_worker_output:a=>{
      const {project,answerPage,...pair}=a,publicData={...pair,status:'complete'};
      const text=answerPage!==undefined?(failPage?'changed body':'Previewable output'):'Previewable output';
      const out={...publicData,text,source:'indexed-answer',truncated:true};
      if(answerPage!==undefined)out.page={offset:0,bytes:Buffer.byteLength(text),totalBytes:Buffer.byteLength(text)+4,digest,nextCursor:'next',hasMore:true};
      return {structuredContent:publicData,_meta:{workerOutput:out}};
    }
  }});
  const items=p.get('leads').children[1].children[2].children.find(e=>e.tagName==='ul').children.filter(e=>e.tagName==='li');
  const open=async i=>{items[i].children.find(e=>e.className?.includes('worker-output-link')).dispatchEvent(new Event('click'));await setImmediate();await p.event('output-followup','click');};
  await open(0);await p.event('followup-include','click');
  assert.equal(p.get('finding-preview').hidden,true);assert.equal(p.get('followup-include').hidden,true);
  assert.match(p.get('followup-status').textContent,/Nothing was shared/);
  assert.ok(!p.host.contexts.some(c=>c.structuredContent?.kind==='oms-selected-finding'));
  failPage=false;await open(0);p.host.delayTool='oms_worker_output';await p.event('followup-include','click');
  const firstRelease=p.host.release;assert.equal(p.get('followup-include').disabled,true);
  await open(1);p.host.delayTool='oms_worker_output';await p.event('followup-include','click');const secondRelease=p.host.release;
  firstRelease();await setImmediate();assert.equal(p.get('finding-preview').hidden,true);
  secondRelease();await setImmediate();
  assert.equal(p.get('finding-preview').hidden,false);assert.equal(p.get('finding-text').textContent,'Previewable output');
});

// v34 usage card and More: read-only reads on demand, cached per binding scope.
function moreFixture(state){
  const jobs=new Map(),timers={setTimeout(fn,ms){const id=Symbol();jobs.set(id,{fn,ms});return id;},clearTimeout(id){jobs.delete(id);}};
  const tools={...inspectionTools(state),
    oms_panel_usage:()=>({scope:'repository-recent-attempts',providers:[
      {provider:'codex',windows:[{name:'5 hour',used:40,stale:false,age_seconds:30},{name:'weekly',used:86,stale:true,age_seconds:2700,source:'session log'}]},
      {provider:'claude',windows:[{name:'weekly',used:38,stale:false,age_seconds:120}]}],attempts:[]}),
    oms_panel_git_status:()=>({git:{available:true,branch:'feat',dirty:false,upstream:'origin/feat'},github:{status:'available',repository:'o/r',observedAt:null,
      pullRequests:[{number:1,title:'Pass',checks:{passed:2},url:'https://github.com/o/r/pull/1'},
        {number:2,title:'Broken',baseBranch:'dev',checks:{failed:1,passed:1,items:[{name:'lint',status:'failed'}]},url:'https://evil.example/o/r/pull/2'},
        {number:3,title:'Running',checks:{pending:1}}]}}),
    oms_panel_work_items:()=>({schema:1,availability:'partial',items:[{id:'t1',title:'Running one',state:'running',owner:null},{id:'bad id',title:'Spaced',state:'running'},
      {id:'t2',title:'Stuck',state:'blocked'},{id:'t3',state:'weird'},{id:'t4',state:'cancelled'},{id:'t5',title:'Old',state:'done'}]}),
    oms_panel_tui_view:a=>({view:'results',page:a.page,status:'ready',nextPage:null,incomplete:false,rows:[
      {taskId:'t1',implement:'passed',verify:'failed',review:'not_run',delivery:'not_run'},
      {taskId:'zz',implement:'passed',verify:'passed',review:'not_requested',delivery:'not_run'}]})};
  const fire=async ms=>{for(const [id,j] of jobs)if(j.ms===ms){jobs.delete(id);j.fn();}for(let i=0;i<4;i++)await setImmediate();};
  return {jobs,timers,tools,fire};
}
const count=(p,name)=>p.calls.filter(c=>c.name===name).length,kids=e=>e.children.filter(c=>typeof c!=='string');
const settle=async()=>{for(let i=0;i<4;i++)await setImmediate();};

test('v35 usage strip reads once after the first screen, shows only recorded windows and opens Inspect usage',async()=>{
  const state={key:'b'.repeat(64)},f=moreFixture(state),p=await panel(['one','two'],dashboard,{locale:'en',timers:f.timers,tools:f.tools});
  await p.open('input',{arguments:{project:'one'}});await settle();
  assert.ok(!p.calls.some(c=>/^oms_panel_(usage|git_status|work_items|tui_view)$/.test(c.name)));
  assert.equal(p.get('usage-strip').hidden,false);
  await f.fire(1500);assert.equal(count(p,'oms_panel_usage'),1);
  // Codex and Claude have windows; AGY and Pi have none and are left out rather than drawn as 0%.
  const uses=kids(p.get('usage-rows'));
  assert.deepEqual(uses.map(u=>[u.getAttribute('data-tone'),u.getAttribute('data-stale')]),[['warn','true'],['plain','false']]);
  assert.equal(uses[0].textContent,'14%Stale');assert.equal(uses[1].textContent,'62%');assert.doesNotMatch(p.get('usage-rows').textContent,/0%$|No record/);
  assert.match(p.get('usage-strip').getAttribute('aria-label'),/^Usage left · Codex Weekly 14% left, Stale · 45m ago · Claude Weekly 62% left, 2m ago · open for detail$/);
  await p.event('refresh','click');await settle();assert.equal(count(p,'oms_panel_usage'),1);
  await p.event('usage-strip','click');await settle();assert.equal(count(p,'oms_panel_usage'),2);
  assert.equal(p.get('inspection').open,true);assert.equal(p.get('inspect-kind').value,'usage');
  p.get('inspection').close();await settle();assert.ok(p.get('usage-strip').focused);
  f.tools.oms_panel_usage=()=>({scope:'repository-recent-attempts',providers:[],attempts:[]});
  await p.open('input',{arguments:{project:'two'}});await settle();
  assert.equal(kids(p.get('usage-rows')).length,0);await f.fire(1500);assert.equal(count(p,'oms_panel_usage'),3);
  assert.equal(p.get('usage-rows').textContent,'No record');assert.match(p.get('usage-strip').getAttribute('aria-label'),/No record/);
});

test('v34 More sections read on first expand, sort PRs by failure and join result titles from work',async()=>{
  const state={key:'b'.repeat(64)},f=moreFixture(state),p=await panel(['one','two'],dashboard,{locale:'en',timers:f.timers,tools:f.tools,openLinks:true});
  await p.open('input',{arguments:{project:'one'}});await settle();
  await p.event('more-pr','click');await settle();
  const pr=p.get('more-pr-body');assert.equal(p.get('more-pr').getAttribute('aria-expanded'),'true');assert.equal(count(p,'oms_panel_git_status'),1);
  const list=pr.children.find(e=>e.tagName==='ul'),items=kids(list).map(li=>li.row.textContent);
  assert.deepEqual(items.map(s=>s.slice(0,s.indexOf(' '))),['×Broken','●Running','✓Pass']);
  assert.match(items[0],/1 failed · 1 passed · lint · → dev/);assert.match(p.get('more-pr-chip').textContent,/1 failed/);
  const link=li=>li.head.children.at(-1);
  assert.equal(link(kids(list)[0]).tagName,'span');assert.equal(link(kids(list)[2]).tagName,'button');
  await p.event('more-pr','click');await p.event('more-pr','click');await settle();assert.equal(count(p,'oms_panel_git_status'),1);
  kids(kids(pr)[0])[1].dispatchEvent(new Event('click'));await settle();assert.equal(count(p,'oms_panel_git_status'),2);
  await p.event('more-work','click');await settle();
  const work=p.get('more-work-body').textContent;assert.match(work,/ · partial/);assert.match(work,/1 cancelled/);
  assert.match(work,/Active●Running oneUnassigned/);assert.match(work,/Other○t3/);assert.doesNotMatch(work,/Old/);
  assert.match(p.get('more-work-chip').textContent,/Blocked 1/);
  // A draft from a work row names the task only when its id is a valid task id.
  const rowOf=title=>{const walk=e=>e.row && e.head?.textContent===title?e:(e.children?kids(e):[]).map(walk).find(Boolean);return walk(p.get('more-work-body'));};
  for(const [title,task] of [['Spaced',null],['Running one','t1']]){
    // A real empty element has no children; the fake DOM keeps an empty text child that would block the first open.
    const li=rowOf(title);kids(li)[1].children=[];li.row.dispatchEvent(new Event('click'));await settle();
    kids(kids(kids(li)[1])[1])[0].dispatchEvent(new Event('click'));await settle();
    const text=p.get('followup-text').value;assert.match(text,new RegExp(title));
    if(task)assert.match(text,/^Task: t1$/m);else assert.doesNotMatch(text,/^Task:|bad id/m);}
  await p.event('more-results','click');await settle();assert.equal(count(p,'oms_panel_work_items'),1);
  const res=p.get('more-results-body');assert.match(res.textContent,/Failed · timed out 1.*Running one.*t1/);assert.match(res.textContent,/zz/);
  const rowButtons=res.children.filter(e=>e.className==='group').flatMap(g=>kids(g.children.at(-1)).map(li=>li.row));
  assert.equal(rowButtons[0].getAttribute('aria-label'),'Running one: Implement passed, Verify failed, Review not run, Delivery not run');
  assert.match(rowButtons[1].getAttribute('aria-label'),/^zz: /);
  await p.event('refresh','click');await settle();
  assert.equal(p.get('more-results').getAttribute('aria-expanded'),'true');assert.equal(count(p,'oms_panel_tui_view'),1);assert.match(res.textContent,/Running one/);
  await p.open('input',{arguments:{project:'two'}});await settle();
  for(const k of ['pr','work','results']){assert.equal(p.get('more-'+k).getAttribute('aria-expanded'),'false',k);assert.equal(p.get('more-'+k+'-body').hidden,true,k);
    assert.equal(kids(p.get('more-'+k+'-body')).length,0,k);}
});

test('pending-turn polling pauses visibly and slot badges change only after a confirmed metadata read',async()=>{
  let now=0,state='running',agyTurnState={status:'unavailable',reason:'outbound_withheld'};const id=randomUUID(),jobs=new Map(),timers={Date:{now:()=>now},
    setTimeout(fn,ms){const key=Symbol();jobs.set(key,{fn,ms});return key;},clearTimeout:key=>jobs.delete(key)};
  assert.ok(!html.includes('"followup.scope"'));
  const p=await panel(['one'],dashboard,{locale:'en',timers,tools:{[managedRead]:a=>{
    const v=leadStatus(a,'ready');
    for(const row of v.leads){
      if(row.provider==='claude')row.latestTurn={turnId:id,status:state,submittedAt:null,lastObservedAt:null};
      if(row.provider==='agy' && agyTurnState){row.latestTurn=null;row.turnState=agyTurnState;}
    }
    if(a.provider)v._private={room:a.room,...v.leads[0],transcript:{turns:[{role:'user',text:'Recorded question',turnId:id,status:state},...(state==='replied'?[{role:'claude',text:'Recorded reply',turnId:id}]:[])]}};
    return v;
  }}});
  assert.equal([...jobs.values()].filter(v=>v.ms>=4000 && v.ms<=15000).length,0);
  assert.match(p.get('leads').children[0].textContent,/Ask Codex/);
  assert.match(p.get('leads').children[1].textContent,/Reply pending/);
  assert.match(p.get('leads').children[2].textContent,/Ask AGY.*Answer withheld by the safety scan/);
  await choose(p,'ko');assert.match(p.get('leads').children[2].textContent,/안전 검사로 답변이 보류되었습니다/);
  await choose(p,'en');
  for(const [turnState,enRe,koRe] of [
    [{status:'unavailable',reason:'invalid_answer_range'},/Answer size or range unavailable/,/답변 크기 또는 범위를 읽을 수 없습니다/],
    [{status:'unavailable',reason:'invalid_turn_record'},/Turn record unreadable/,/전달 기록을 읽을 수 없습니다/],
    [{status:'unconfirmed',reason:'receipt_changed'},/Receipt changed · delivery unconfirmed/,/접수 상태가 변경되어 전달을 확인할 수 없습니다/]
  ]){
    agyTurnState=turnState;await p.event('lead-refresh','click');
    assert.match(p.get('leads').children[2].textContent,enRe);
    await choose(p,'ko');assert.match(p.get('leads').children[2].textContent,koRe);
    await choose(p,'en');
  }
  const open=p.get('leads').children[1].children[1].children.find(e=>e.type==='button');open.dispatchEvent(new Event('click'));await setImmediate();
  const poll=[...jobs].find(([,v])=>v.ms===4000);jobs.delete(poll[0]);now=130000;poll[1].fn();await setImmediate();
  assert.match(p.get('chat-delivery').textContent,/Automatic checks paused/);assert.equal([...jobs.values()].filter(v=>v.ms===4000).length,0);
  const mark=p.calls.length;await p.event('chat-check','click');assert.deepEqual(p.calls.slice(mark).map(c=>c.name),[managedRead,'oms_read_lead_stream']);
  await p.event('chat-back','click');assert.equal([...jobs.values()].filter(v=>v.ms>=4000 && v.ms<=15000).length,0);
  state='replied';agyTurnState=null;await p.event('lead-refresh','click');assert.match(p.get('leads').children[1].textContent,/New reply/);
  assert.doesNotMatch(p.get('leads').children[2].textContent,/withheld|unreadable|unconfirmed/);
  open.dispatchEvent(new Event('click'));await setImmediate();await p.event('chat-back','click');assert.match(p.get('leads').children[1].textContent,/Reply read/);
  assert.ok(!p.calls.some(c=>c.name==='oms_send_lead_message'));
});

test('auto-refresh pauses while the follow-up preview or finding Share is active and resumes on close without weakening rechecks',async()=>{
  let now=1000;
  const jobs=new Map(),memory=new Map([['oms.panel.auto-refresh.v1','on']]),key='b'.repeat(64),digest='d'.repeat(64),body='Scoped worker finding';
  const timers={Date:{now:()=>now},setTimeout(fn,ms){const id=Symbol();jobs.set(id,{fn,ms});return id;},clearTimeout(id){jobs.delete(id);}};
  const storage={getItem:k=>memory.get(k),setItem:(k,v)=>memory.set(k,v)};
  const autos=()=>[...jobs].filter(([,job])=>job.ms===20000);
  const d={...dashboard,conversationAttempts:[...dashboard.conversationAttempts,{id:'worker-0',parentAttempt:'l',provider:'codex',role:'worker',state:'done',task:'task-b'}]};
  let snapshot=d;
  const p=await panel(['one'],d,{locale:'en',context:true,timers,storage,visibility:true,tools:{
    oms_list_projects:()=>({projects:['one'],projectKeys:{one:key}}),
    oms_panel_snapshot:()=>({dashboard:snapshot}),
    oms_worker_output:a=>{
      const {project,answerPage,...pair}=a,publicData={...pair,status:'complete'};
      const out={...publicData,text:body,source:'indexed-answer',truncated:false};
      if(answerPage!==undefined)out.page={offset:0,bytes:Buffer.byteLength(body),totalBytes:Buffer.byteLength(body),digest,nextCursor:null,hasMore:false};
      return {structuredContent:publicData,_meta:{workerOutput:out}};
    }
  }});
  await p.open('input',{arguments:{project:'one'}});
  assert.equal(autos().length,1);
  const staleCallback=autos()[0][1].fn;
  const open=p.get('leads').children[1].children[2].children.find(e=>e.tagName==='ul').children.find(e=>e.tagName==='li')
    .children.find(e=>e.className?.includes('worker-output-link'));
  open.dispatchEvent(new Event('click'));await setImmediate();
  await p.event('output-followup','click');await p.event('followup-include','click');
  assert.equal(p.get('followup-panel').hidden,false);
  assert.equal(p.get('finding-preview').hidden,false);
  assert.equal(autos().length,0);
  // Stale focus/visibility and timer callbacks skip while the preview and pending Share are active.
  now+=25000;
  p.host.delayTool='oms_worker_output';
  await p.event('finding-share','click');
  assert.equal(p.get('finding-share').disabled,true);
  const callsDuringShare=p.calls.length;
  staleCallback();
  p.window.dispatchEvent(new Event('focus'));
  p.document.show(false);p.document.show(true);
  p.run('autoReload()');
  await setImmediate();
  assert.equal(p.calls.length,callsDuringShare);
  assert.equal(p.run('busy'),false);
  assert.equal(p.get('finding-text').textContent,body);
  p.host.release();await setImmediate();
  assert.equal(p.host.contexts.at(-1)?.structuredContent?.kind,'oms-selected-finding');
  assert.equal(p.host.contexts.at(-1)?.structuredContent?.task,'task-b');
  assert.equal(p.get('finding-preview').hidden,false);
  assert.equal(autos().length,0);
  // Post-load verifiedFindingScope still enforces exact recheck.
  p.host.delayTool='oms_worker_output';
  await p.event('finding-share','click');
  snapshot={...d,conversationAttempts:d.conversationAttempts.map(a=>a.id==='worker-0'?{...a,task:'task-changed'}:a)};
  p.host.release();await setImmediate();
  assert.equal(p.host.contexts.filter(c=>c.structuredContent?.kind==='oms-selected-finding').length,1);
  assert.equal(p.get('finding-text').textContent,'');
  assert.match(p.get('followup-status').textContent,/Nothing was shared/);
  await p.event('followup-close','click');
  assert.equal(autos().length,1);
});

test('hostToolsUnavailable sets only for host transport refusal before any tools/call succeeds, offers Retry, and clears on Retry success',async()=>{
  const ipcUnavailable={code:-32603,message:'IPC not available for MCP tool call'};
  const serverBusy={code:-32000,message:'OMS panel is busy. The request was not executed.'};
  const invalidParams={code:-32602,message:'unknown or invalid desktop tool arguments'};
  const methodNotFound={code:-32601,message:'method not found'};
  const key='b'.repeat(64);

  // Server's own JSON-RPC errors (-32000 busy, -32602, -32601) and tool isError results must never set hostToolsUnavailable,
  // even when no panel tools/call has succeeded yet.
  for(const rpcError of [serverBusy,invalidParams,methodNotFound]){
    const s=await panel(['one'],{...dashboard,goal:'Live after transient server error'},{
      locale:'en',
      tools:{oms_list_projects:()=>({projects:['one'],projectKeys:{one:key}})},
      toolError:rpcError
    });
    assert.equal(s.run('hostToolsUnavailable'),false);
    assert.equal(s.get('host-retry').hidden,true);
    assert.equal(s.get('refresh').disabled,false);
    assert.equal(s.get('auto-refresh').disabled,false);
    s.host.toolError=null;
    await s.event('refresh','click');
    assert.equal(s.run('hostToolsUnavailable'),false);
    assert.equal(s.get('goal').textContent,'Live after transient server error');
  }

  let initialToolError=true;
  const toolIsErrorPanel=await panel(['one'],{...dashboard,goal:'Live after tool isError'},{
    locale:'en',
    tools:{oms_list_projects:()=>{if(initialToolError)throw new Error('server tool error');return {projects:['one'],projectKeys:{one:key}};}},
  });
  assert.equal(toolIsErrorPanel.run('hostToolsUnavailable'),false);
  assert.equal(toolIsErrorPanel.get('host-retry').hidden,true);
  assert.equal(toolIsErrorPanel.get('refresh').disabled,false);
  initialToolError=false;
  await toolIsErrorPanel.event('refresh','click');
  assert.equal(toolIsErrorPanel.get('goal').textContent,'Live after tool isError');

  // A host that refuses every tools/call renders the initial open dashboard, disables regular controls, and offers Retry.
  const timers=new Map(),memory=new Map([['oms.panel.auto-refresh.v1','on']]),attempts=[];
  let activeError=ipcUnavailable;
  const p=await panel(['one','two'],{...dashboard,goal:'Live snapshot from tools/call'},{
    locale:'en',
    context:true,
    storage:{getItem:k=>memory.get(k),setItem:(k,v)=>memory.set(k,v)},
    timers:{setTimeout(fn,ms){const id=Symbol();timers.set(id,{fn,ms});return id;},clearTimeout(id){timers.delete(id);}},
    tools:{oms_list_projects:()=>({projects:['one','two'],projectKeys:{one:key,two:'c'.repeat(64)}})},
    toolError:params=>{attempts.push(params.name);return activeError;}
  });
  assert.equal(p.run('hostToolsUnavailable'),true);
  assert.equal(p.get('host-retry').hidden,false);
  assert.equal(p.get('host-retry').disabled,false);
  assert.equal(p.get('host-retry').textContent,'Retry');
  assert.equal(p.get('refresh').disabled,true);
  assert.equal(p.get('auto-refresh').disabled,true);

  await p.open('input',{arguments:{project:'one'}});
  await p.open('result',{structuredContent:{
    panel:'OMS',project:'one',readOnly:false,
    dashboard:{...dashboard,project:'one',goal:'Ignored public dashboard'}
  }});
  assert.equal(p.get('view').hidden,true);
  await p.open('result',{
    structuredContent:{panel:'OMS',project:'one',readOnly:false,snapshot:'attached'},
    _meta:{dashboard:{...dashboard,project:'one',goal:'Rendered from initial open'}}
  });
  assert.equal(p.get('view').hidden,false);
  assert.equal(p.get('project').textContent,'one');
  assert.equal(p.get('goal').textContent,'Rendered from initial open');
  assert.equal(p.get('main-name').textContent,'OMS Main · Codex');
  assert.equal(p.run('hostToolsUnavailable'),true);
  assert.equal(p.get('host-retry').hidden,false);
  assert.equal(p.get('host-retry').disabled,false);
  assert.equal(p.get('refresh').disabled,true);
  assert.equal(p.get('auto-refresh').disabled,true);
  assert.equal(p.get('new-goal').disabled,true);
  assert.equal(p.get('header-main').disabled,true);
  assert.equal(p.get('inspect-open').disabled,true);
  assert.equal(p.get('open-leads').disabled,true);
  // The usage strip and More never read through a host that refuses tools/call.
  assert.equal(p.get('more-pr').disabled,true);assert.equal(p.get('usage-strip').hidden,true);
  assert.equal([...timers.values()].filter(t=>t.ms===1500).length,0);
  const memberButton=p.get('leads').children[1].children[2];
  assert.equal(memberButton.disabled,true);
  assert.equal([...timers.values()].filter(t=>t.ms===20000).length,0);
  assert.match(p.get('status').textContent,/This host cannot refresh or act here · ask in the conversation/);
  await choose(p,'ko');
  assert.equal(p.get('host-retry').textContent,'다시 시도');
  assert.match(p.get('status').textContent,/새로고침·작업을 쓸 수 없습니다 · 대화에서 요청하세요/);
  await choose(p,'en');

  const mark=attempts.length;
  await p.event('refresh','click');
  await p.event('auto-refresh','click');
  await p.event('new-goal','click');
  await p.event('header-main','click');
  await p.event('inspect-open','click');
  memberButton.dispatchEvent(new Event('click'));await setImmediate();
  p.window.dispatchEvent(Object.assign(new Event('keydown',{cancelable:true}),{key:'r',altKey:true}));await setImmediate();
  assert.equal(attempts.length,mark);
  assert.equal(p.get('goal').textContent,'Rendered from initial open');
  assert.equal(p.get('launcher').open ?? false,false);
  assert.equal(p.get('conversation').open ?? false,false);
  assert.equal(p.get('inspection').open ?? false,false);

  // Clicking Retry while the host still refuses attempts a single refresh, keeps the rendered dashboard, and keeps Retry available.
  p.document.activeElement=p.get('host-retry');
  await p.event('host-retry','click');
  assert.deepEqual(attempts.slice(mark),['oms_list_projects']);
  assert.equal(p.run('hostToolsUnavailable'),true);
  assert.equal(p.get('view').hidden,false);
  assert.equal(p.get('goal').textContent,'Rendered from initial open');
  assert.equal(p.get('host-retry').hidden,false);
  assert.equal(p.get('host-retry').disabled,false);
  assert.equal(p.get('refresh').focused ?? 0,0);
  assert.equal([...timers.values()].filter(t=>t.ms===20000).length,0);

  // Existing hostRejected classification for -32601/-32602 keeps its meaning.
  activeError={code:-32601,message:'Method not found'};
  const rejected=await p.run(`request('tools/call',{name:'oms_list_projects',arguments:{}}).then(()=>null,e=>({hostRejected:e.hostRejected,message:e.message}))`);
  assert.equal(rejected.hostRejected,true);
  activeError=ipcUnavailable;
  const generic=await p.run(`request('tools/call',{name:'oms_list_projects',arguments:{}}).then(()=>null,e=>({hostRejected:e.hostRejected,message:e.message}))`);
  assert.equal(generic.hostRejected,false);

  // Retry success clears hostToolsUnavailable, hides Retry, renders live state, re-enables controls, moves focus to refresh, and arms auto-refresh.
  activeError=null;
  p.document.activeElement=p.get('host-retry');
  await p.event('host-retry','click');
  assert.equal(p.run('hostToolsUnavailable'),false);
  assert.equal(p.get('host-retry').hidden,true);
  assert.equal(p.get('host-retry').disabled,true);
  assert.equal(p.get('refresh').focused,1);
  assert.equal(p.get('project').textContent,'one');
  assert.equal(p.get('goal').textContent,'Live snapshot from tools/call');
  assert.equal(p.get('refresh').disabled,false);
  assert.equal(p.get('auto-refresh').disabled,false);
  assert.equal(p.get('new-goal').disabled,false);
  assert.equal(p.get('inspect-open').disabled,false);
  assert.equal(p.get('status').textContent,'');
  assert.equal([...timers.values()].filter(t=>t.ms===20000).length,1);

  // Once a newer snapshot from a successful tools/call has rendered for the same project and generation,
  // a late open result dashboard does not overwrite it.
  await p.open('input',{arguments:{project:'two'}});
  assert.equal(p.get('project').textContent,'two');
  assert.equal(p.get('goal').textContent,'Live snapshot from tools/call');
  await p.open('result',{structuredContent:{panel:'OMS',project:'two',snapshot:'attached'},_meta:{dashboard:{project:'two',goal:'Stale open dashboard'}}});
  assert.equal(p.get('goal').textContent,'Live snapshot from tools/call');

  // Once any panel tools/call has succeeded, neither server errors (-32000, -32602, -32601, isError) nor later JSON-RPC errors ever set hostToolsUnavailable.
  const approval={id:'r-1',version:1,digest:'a'.repeat(64),summary:'Approve?',action:'Main scope decision',object:main.room};
  const q=await panel(['one'],{...dashboard,goal:'Initial live'},{locale:'en',context:true,tools:{
    oms_list_projects:()=>({projects:['one'],projectKeys:{one:key}}),
    oms_list_approvals:()=>({approvals:[approval]})
  }});
  const approveButton=()=>q.get('approvals').children[0].children.at(-1).children.at(-1).children.find(b=>b.type==='button');
  assert.equal(approveButton().disabled,false);
  await q.event('followup-open','click');
  assert.equal(q.get('followup-main').disabled,false);
  q.host.tools.oms_panel_snapshot=()=>{throw new Error('server tool error');};
  await q.event('refresh','click');
  assert.equal(q.run('hostToolsUnavailable'),false);
  q.host.tools.oms_panel_snapshot=args=>({dashboard:{...dashboard,project:args.project,goal:'Recovered live'}});
  await q.event('refresh','click');
  assert.equal(q.get('goal').textContent,'Recovered live');
  for(const laterError of [serverBusy,invalidParams,methodNotFound,ipcUnavailable]){
    q.host.toolError=laterError;
    await q.event('refresh','click');
    assert.equal(q.run('hostToolsUnavailable'),false);
    assert.equal(q.get('host-retry').hidden,true);
    assert.equal(q.get('refresh').disabled,false);
    assert.equal(q.get('auto-refresh').disabled,false);
  }
  q.host.toolError=null;
  await q.event('refresh','click');
  assert.equal(q.get('goal').textContent,'Recovered live');
  assert.equal(approveButton().disabled,false);
  assert.equal(q.get('status').textContent,'');
});

test('initial open renders _meta.dashboard without a second snapshot read until the next refresh tick, while snapshotOmitted still fetches',async()=>{
  const key='b'.repeat(64);
  let now=1000;
  const jobs=new Map(),memory=new Map([['oms.panel.auto-refresh.v1','on']]);
  const timers={Date:{now:()=>now},setTimeout(fn,ms){const id=Symbol();jobs.set(id,{fn,ms});return id;},clearTimeout(id){jobs.delete(id);}};
  const storage={getItem:k=>memory.get(k),setItem:(k,v)=>memory.set(k,v)};
  const autos=()=>[...jobs].filter(([,job])=>job.ms===20000);
  const p=await panel(['one','two'],{...dashboard,goal:'Refreshed snapshot'},{
    locale:'en',
    timers,
    storage,
    tools:{oms_list_projects:()=>({projects:['one','two'],projectKeys:{one:key,two:'c'.repeat(64)}})},
    initialOpen:[
      ['input',{arguments:{project:'one'}}],
      ['result',{
        structuredContent:{panel:'OMS',project:'one',snapshot:'attached'},
        _meta:{dashboard:{...dashboard,project:'one',goal:'Delivered in _meta.dashboard'}}
      }]
    ]
  });
  const snapshots=()=>p.calls.filter(c=>c.name==='oms_panel_snapshot');
  assert.equal(p.get('view').hidden,false);
  assert.equal(p.get('goal').textContent,'Delivered in _meta.dashboard');
  assert.equal(p.get('main-name').textContent,'OMS Main · Codex');
  assert.equal(snapshots().length,0);
  assert.deepEqual(p.calls.map(c=>c.name),['oms_list_projects','oms_list_approvals',managedRead]);
  assert.equal(autos().length,1);

  const [tickId,tickJob]=autos()[0];
  jobs.delete(tickId);
  now+=20000;
  tickJob.fn();
  await setImmediate();
  assert.equal(snapshots().length,1);
  assert.equal(p.get('goal').textContent,'Refreshed snapshot');
  assert.equal(autos().length,1);

  for(const reason of ['busy','size']){
    const omittedJobs=new Map();
    const omitted=await panel(['one'],{...dashboard,goal:'Fetched after '+reason},{
      locale:'en',
      storage,
      timers:{Date:{now:()=>now},setTimeout(fn,ms){const id=Symbol();omittedJobs.set(id,{fn,ms});return id;},clearTimeout(id){omittedJobs.delete(id);}},
      tools:{oms_list_projects:()=>({projects:['one'],projectKeys:{one:key}})},
      initialOpen:[
        ['input',{arguments:{project:'one'}}],
        ['result',{structuredContent:{panel:'OMS',project:'one',snapshotOmitted:reason}}]
      ]
    });
    assert.equal(omitted.calls.filter(c=>c.name==='oms_panel_snapshot').length,1);
    assert.equal(omitted.get('goal').textContent,'Fetched after '+reason);
    assert.equal([...omittedJobs.values()].filter(j=>j.ms===20000).length,1);
  }
});

test('mcp-app hosts open direct Lead chat without a Room or kickoff round and keep chat open across refresh',async()=>{
  const key='b'.repeat(64);
  let model=null,effort=null,revision='1'.repeat(64),snapCount=0;
  const turns=[];
  const leadRow=provider=>({provider,status:'ready',requestId:'12345678-1234-8234-8234-123456789ab'+['codex','claude','agy'].indexOf(provider)});
  const leadResp=a=>{
    const all=['codex','claude','agy'].map(leadRow);
    const leads=a.provider?all.filter(r=>r.provider===a.provider):all;
    const out={projectKey:key,room:null,leads};
    if(a.turnId)out.turn={turnId:a.turnId,status:turns.some(r=>r.turnId===a.turnId)?'replied':'not_found'};
    if(a.provider)out._private={room:null,provider:a.provider,requestId:leads[0].requestId,status:'ready',transcript:{turns,cursor:null,hasMore:false}};
    return out;
  };
  const p=await panel(['one'],{project:'one',goal:'Direct lead chat'},{
    host:'Antigravity',
    locale:'en',
    readOnly:true,
    tools:{
      oms_list_projects:()=>({projects:['one'],projectKeys:{one:key},binding:'catalog',readOnly:true,leadChat:true}),
      oms_panel_snapshot:()=>{snapCount++;return {dashboard:{project:'one',goal:'Direct lead chat #' + snapCount}};},
      [managedRead]:a=>{assert.equal(Object.hasOwn(a,'room'),false);return leadResp(a);},
      oms_send_lead_message:a=>{
        assert.equal(Object.hasOwn(a,'room'),false);
        assert.equal(a.project,'one');assert.equal(a.projectKey,key);assert.equal(a.provider,'codex');
        turns.push({role:'user',text:a.text,turnId:a.turnId,status:'replied'},{role:'codex',text:'Codex direct reply',turnId:a.turnId});
        return {...leadResp(a),turn:{turnId:a.turnId,status:'replied'}};
      },
      oms_lead_settings:a=>{
        assert.equal(Object.hasOwn(a,'room'),false);
        const s={projectKey:key,provider:a.provider,leadRequestId:a.leadRequestId,revision,
          selection:{model,effort},options:[{model:'gpt-5.4',source:'catalog',efforts:['low','high'],fixed:false,requiresEffort:false}],
          catalogAgeSeconds:5,scope:{target:'lead-conversation',appliesTo:'next-message',inheritsToWorkers:false,mode:'read-only-consultation',continuity:'oms-thread'}};
        return {structuredContent:s,_meta:{leadSettings:s}};
      },
      oms_update_lead_settings:a=>{
        assert.equal(Object.hasOwn(a,'room'),false);
        model=a.model;effort=a.effort;revision='2'.repeat(64);
        const s={projectKey:key,provider:a.provider,leadRequestId:a.leadRequestId,revision,
          selection:{model,effort},options:[{model:'gpt-5.4',source:'catalog',efforts:['low','high'],fixed:false,requiresEffort:false}],
          catalogAgeSeconds:5};
        return {structuredContent:s,_meta:{leadSettings:s}};
      }
    }
  });
  assert.equal(p.get('new-goal').hidden,true);
  assert.equal(p.get('approval-card').hidden,true);
  assert.equal(p.get('lead-settings').hidden,true);
  assert.equal(p.get('open-leads').hidden,true);
  const codexSlot=p.get('leads').children[0];
  const opener=codexSlot.children[1].children.find(e=>e.type==='button' && e.textContent==='Ask Codex');
  assert.ok(opener && !opener.hidden);
  opener.dispatchEvent(new Event('click'));await setImmediate();
  assert.equal(p.get('conversation').open,true);
  assert.equal(p.get('chat-title').textContent,'OMS Lead · Codex');
  p.get('chat-text').value='Hello Codex directly';await p.event('chat-text','input');
  await p.event('chat-compose','submit');await setImmediate();
  assert.match(p.get('chat-messages').textContent,/Codex direct reply/);

  // Settings work without room in project-scoped mode, and continuity copy names Codex, Claude and AGY.
  await p.event('settings-toggle','click');await setImmediate();
  assert.match(p.get('settings-scope').textContent,/Codex, Claude and AGY/);
  p.get('settings-model').value='gpt-5.4';await p.event('settings-model','change');
  p.get('settings-effort').value='high';await p.event('settings-effort','change');
  await p.event('settings-save','click');await setImmediate();
  assert.match(p.get('settings-saved').textContent,/gpt-5\.4 · high/);

  // Explicit newSession flag sends newSession:true, survives not_found retry, and clears on accept.
  let freshAttempts=0;
  p.host.tools.oms_send_lead_message=a=>{
    assert.equal(Object.hasOwn(a,'room'),false);
    freshAttempts++;
    if(freshAttempts===1)return {...leadResp(a),turn:{turnId:a.turnId,status:'not_found'}};
    turns.push({role:'user',text:a.text,turnId:a.turnId},{role:'codex',text:'Fresh Codex reply',turnId:a.turnId});
    return {...leadResp(a),turn:{turnId:a.turnId,status:'replied'}};
  };
  p.get('chat-text').value='Start fresh please';await p.event('chat-text','input');
  p.get('chat-fresh').checked=true;await p.event('chat-fresh','change');
  await p.event('chat-compose','submit');await setImmediate();
  let sendCalls=p.calls.filter(c=>c.name==='oms_send_lead_message');
  assert.equal(sendCalls.at(-1).args.newSession,true);
  assert.equal(p.get('chat-fresh').checked,true);
  await p.event('chat-check','click');await setImmediate();
  sendCalls=p.calls.filter(c=>c.name==='oms_send_lead_message');
  assert.equal(sendCalls.length,3);
  assert.equal(sendCalls.at(-1).args.newSession,true);
  assert.equal(p.get('chat-fresh').checked,false);
  p.get('chat-text').value='Resume new thread';await p.event('chat-text','input');
  await p.event('chat-compose','submit');await setImmediate();
  sendCalls=p.calls.filter(c=>c.name==='oms_send_lead_message');
  assert.equal(Object.hasOwn(sendCalls.at(-1).args,'newSession'),false);

  // Refresh keeps the open Lead chat alive.
  await p.event('refresh','click');await setImmediate();
  assert.equal(p.get('conversation').open,true);
  assert.match(p.get('chat-messages').textContent,/Codex direct reply/);
});

test('chatgpt-panel enables project-scoped Lead chat without a Codex Room and fences on stale snapshot',async()=>{
  const key='b'.repeat(64);
  let failSnap=false;
  const leadRow=provider=>({provider,status:'ready',requestId:'12345678-1234-8234-8234-123456789ab'+['codex','claude','agy'].indexOf(provider)});
  const leadResp=a=>{
    const all=['codex','claude','agy'].map(leadRow);
    const leads=a.provider?all.filter(r=>r.provider===a.provider):all;
    const out={projectKey:key,room:null,leads};
    if(a.provider)out._private={room:null,provider:a.provider,requestId:leads[0].requestId,status:'ready',transcript:{turns:[],cursor:null,hasMore:false}};
    return out;
  };
  const claudeMain={...dashboard,conversationBinding:'matched',conversationMain:{source:'room-session',state:'joined',provider:'claude',harness:'claude',room:'claude-room',participant:'claude-main',incomplete:false,participants:[]}};
  const p=await panel(['one'],claudeMain,{
    host:'Codex',
    locale:'en',
    tools:{
      oms_list_projects:()=>({projects:['one'],projectKeys:{one:key},binding:'catalog',readOnly:false,leadChat:true}),
      oms_panel_snapshot:()=>{if(failSnap)throw Error('offline');return {dashboard:{project:'one',...claudeMain}};},
      [managedRead]:a=>{assert.equal(Object.hasOwn(a,'room'),false);return leadResp(a);},
      oms_send_lead_message:a=>{
        assert.equal(Object.hasOwn(a,'room'),false);
        return {...leadResp(a),turn:{turnId:a.turnId,status:'replied'}};
      }
    }
  });
  assert.equal(p.get('lead-settings').hidden,true);
  const agySlot=p.get('leads').children[2];
  const opener=agySlot.children[1].children.find(e=>e.type==='button' && e.textContent==='Ask AGY');
  assert.ok(opener && !opener.hidden);
  opener.dispatchEvent(new Event('click'));await setImmediate();
  assert.equal(p.get('conversation').open,true);
  failSnap=true;await p.event('refresh','click');await setImmediate();
  assert.equal(p.get('conversation').open,false);
  assert.ok(opener.hidden);
  await assert.rejects(p.run("tool('oms_send_lead_message',{})"));
});
