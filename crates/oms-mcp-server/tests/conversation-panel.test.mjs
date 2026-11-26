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
  const host = {modes:[],contexts:[],status:null, failStart:true, replies:[], links:[], messages:[], notices:[], tools:options.tools};
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
    if (id == null) { host.notices.push(method); return; }
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
      try {
        const value = tool(params.name, params.arguments);
        if(params.name==='oms_worker_output')result=value;
        else result = params.name==='oms_lead_conversation_status' && value._private ? {_meta:{leadConversation:value._private},structuredContent:{projectKey:value.projectKey,room:value.room,leads:value.leads,turn:value.turn}} : params.name === 'oms_panel_chat' ? {_meta:{chat:value}, structuredContent:{count:0}} : {structuredContent:value};
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
  const context = createContext({window, document, Option, TextEncoder, CSS:{supports:(_k,v)=>/^#[0-9a-f]{6}$/i.test(v)}, setTimeout, clearTimeout, crypto:{randomUUID},
    ...options.timers, ...(options.navigator && {navigator:options.navigator}), ...(options.storage && {localStorage:options.storage})});
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
  assert.match(p.get('status').textContent,/No project is bound/);
  await p.event('refresh','click');
  assert.ok(p.calls.every(c=>c.name==='oms_list_projects'));
  await choose(p,'ko'); assert.match(p.get('status').textContent,/프로젝트가 없습니다/);
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
    registry=invalid;await p.event('refresh','click');assert.match(p.get('status').textContent,/프로젝트를 확인할 수 없습니다/);
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
  return {projectKey,room:args.room,leads:leads.filter(r=>!args.provider || r.provider===args.provider)};
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
  change({theme:null,styles:null});assert.equal(root.getAttribute('data-theme'),null);assert.equal(color(ink),'');
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
  assert.match(cards[1].children[2].textContent,/OMS Lead · Claude.*Check indexing.*Blocked.*Open conversation/);
  assert.doesNotMatch(p.get('leads').textContent,/opaque|Foreign task|lead-one/);
  assert.equal(p.get('approval-card').hidden,true);
  assert.equal(p.get('outcome').hidden,true);
  await choose(p,'ko');assert.match(cards[1].textContent,/차단됨.*대화 열기/);assert.match(cards[2].textContent,/배정 대기/);
  assert.ok(!html.includes('role="tablist"'));
  for(const tool of ['oms_panel_project_graph','oms_panel_work_items','oms_panel_usage','oms_panel_capabilities','oms_panel_tui_view'])
    assert.ok(!script.includes(tool),tool);
  const generic=await panel([],dashboard,{locale:'en',readOnly:true});
  assert.deepEqual(generic.calls.map(c=>c.name),startup.slice(0,2));
  assert.equal(generic.get('new-goal').hidden,true);assert.equal(generic.get('header-main').hidden,true);
  assert.equal(generic.get('leads').children.length,3);
  assert.ok(generic.get('leads').children.every(slot=>slot.textContent.endsWith('Assignment unknown') && slot.children.length===2));
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
  assert.equal(opener.textContent,'Open Claude chat');assert.ok(!opener.hidden);
  assert.equal(claude.children[2].tagName,'button');
  opener.dispatchEvent(new Event('click'));await setImmediate();
  assert.deepEqual({...p.calls.at(-1).args},{project:'one',room:main.room,provider:'claude'});
  assert.equal(p.get('chat-title').textContent,'OMS Lead · Claude');
  assert.match(p.get('chat-messages').textContent,/Recorded review/);
  await p.event('chat-back','click');assert.ok(opener.focused);
  spoof=true;opener.dispatchEvent(new Event('click'));await setImmediate();
  assert.equal(p.get('chat-messages').textContent,'');assert.match(p.get('chat-status').textContent,/Could not load/);
  await choose(p,'ko');assert.match(opener.textContent,/Claude 대화 열기/);
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
  const polls=()=>[...timers.values()].filter(v=>v.ms===8000);
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

test('only an exact explicit opener pair opts into this physical project and refresh never replays an uncertain write',async()=>{
  const memory=new Map(),storage={getItem:k=>memory.get(k),setItem:(k,v)=>memory.set(k,v)};
  const tools={[managedOpen]:()=>{throw Error('acknowledgement lost');}};
  const p=await panel(['one'],dashboard,{storage,tools});
  const input={arguments:{project:'one',leadConversations:true}}, result={structuredContent:{panel:'OMS',project:'one',leadConversations:true}};
  await p.open('input',input);assert.ok(!p.calls.some(c=>c.name===managedOpen));
  await p.open('result',result);await p.open('result',result);await p.event('refresh','click');
  assert.equal(p.calls.filter(c=>c.name===managedOpen).length,1);
  const q=await panel(['one'],dashboard,{storage,tools:{[managedOpen]:a=>leadStatus(a,'starting')}});
  assert.equal(q.calls.filter(c=>c.name===managedOpen).length,1);
  assert.match(q.get('leads').textContent,/시작 중/);await q.event('lead-auto-off','click');q.notify({method:'ui/resource-teardown',id:'close-auto',params:{}});
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
  assert.match(p.get('leads').textContent,/Open Claude chat/);
  assert.equal(p.calls.filter(c=>c.name===managedOpen).length,1);
  status='starting';await p.event('refresh','click');const start=now;
  while(polls().length){await tick();assert.ok(now-start<=120000);}
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
test('a degraded, partial or trimmed snapshot keeps a visible status until a clean read',async()=>{
  const d={};
  const p=await panel(['one'],d,{locale:'en'});
  assert.equal(p.get('status').textContent,'');
  for(const [fields,expected] of [[{degraded:true},/could not be collected/],[{observation:{completeness:'unknown'}},/could not be collected/],
    [{observation:{completeness:'partial'}},/partial status/],[{conversationAttemptsOmitted:2},/partial status/],
    [{activeAttemptsTrimmed:1},/partial status/],[{degraded:false,observation:{completeness:'complete'}},/^$/]]) {
    for(const key of Object.keys(d))delete d[key];
    Object.assign(d,fields);await p.event('refresh','click');
    assert.match(p.get('status').textContent,expected);
  }
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
    assert.match(p.get('issue-evidence').textContent,/Receipt saved only.*not confirmed/);
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
  assert.ok(p.get('leads').children.every(slot=>slot.textContent.endsWith('Assignment unknown')));assert.match(p.get('lead-note').textContent,/part of/);
  assert.match(p.get('main-status').textContent,/Joined this room/);
  d.conversationMain.participantsOmitted=0;await p.event('refresh','click');
  assert.ok(p.get('leads').children.every(slot=>slot.textContent.endsWith('Waiting for assignment')));
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
  assert.ok(unconfirmed.get('leads').children.every(slot=>slot.textContent.endsWith('배정 미확인') && slot.children.length===2));
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
  open.dispatchEvent(new Event('click'));await setImmediate();assert.ok(p.get('worker-output').open);
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
  await p.event('output-close','click');assert.ok(open.focused);
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
    let rows=[row];const d={...dashboard,acceptance:{available:true,complete:false,total:3,verified:2}};
    const p=await panel(['one'],d,{locale:'en',tools:{oms_list_approvals:()=>({approvals:rows}),oms_decide_approval:a=>{rows=[];return a;}}});
    assert.equal(p.get('approval-card').hidden,false);assert.match(p.get('approvals').textContent,/Publish the documentation/);
    assert.doesNotMatch(p.get('approvals').textContent,/receipt-2|ffff|room-a|internal-name/);
    assert.equal(p.get('outcome').hidden,true);
    d.acceptance={available:true,complete:true,total:3,verified:3};
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
  assert.equal(p.get('progress').textContent,'2 of 3 planned tasks verified');
  assert.equal(p.get('outcome').hidden,true);
});
test('language persists on reopen without fetching or replacing the focused draft',async()=>{
  const memory=new Map(),storage={getItem:key=>memory.get(key),setItem:(key,value)=>memory.set(key,value)};
  const p=await panel(['one'],dashboard,{locale:'ko',storage});
  await p.event('new-goal','click');const brief=p.get('brief');brief.value='Keyboard audit';p.document.activeElement=brief;
  const mark=p.calls.length;await choose(p,'en');
  assert.equal(p.calls.length,mark);assert.equal(p.document.activeElement,brief);assert.equal(brief.value,'Keyboard audit');
  assert.equal(p.get('new-goal').textContent,'New goal');
  const q=await panel(['one'],dashboard,{locale:'ko',storage});assert.equal(q.get('new-goal').textContent,'New goal');
});
test('partial approval retrieval stays visible without a decision button or a false empty state',async()=>{
  const p=await panel(['one'],{},{locale:'en',tools:{oms_list_approvals:()=>({approvals:[],incomplete:true})}});
  assert.equal(p.get('approval-card').hidden,false);assert.match(p.get('approvals').textContent,/Only part of the approval list/);
  assert.ok(p.get('approvals').children.every(child=>child.tagName!=='details'));
  p.host.tools.oms_list_approvals=()=>{throw Error('failed read');};await p.event('refresh','click');
  assert.equal(p.get('approval-card').hidden,false);assert.equal(p.get('approvals').children[0].tagName,'p');
  assert.ok(!p.calls.some(c=>c.name==='oms_decide_approval'));
});
