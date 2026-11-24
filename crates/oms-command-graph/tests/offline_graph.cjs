// Exercise the actual shared renderer with no CDN or browser dependencies.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const template = fs.readFileSync(path.join(__dirname, '../src/graph.html'), 'utf8');
const mcp = fs.readFileSync(path.join(__dirname, '../../oms-mcp-server/src/project-graph.html'), 'utf8');
const script = html => html.match(/\(\(\) => \{[\s\S]*?\n\}\)\(\);/)[0];
const style = html => html.match(/<style>([\s\S]*?)<\/style>/)[1];
const legendBlock = html => html.match(/<div class="oms-graph-legend[^>]*>([\s\S]*?)<\/div>/)[1];
assert.equal(script(mcp).replaceAll('oms-graph-view-19ff9c65bfcf', '@@ROOT@@'), script(template));
assert.equal(style(mcp).replaceAll('oms-graph-view-19ff9c65bfcf', '@@ROOT@@'), style(template));
assert.equal(legendBlock(mcp).replaceAll('oms-graph-view-19ff9c65bfcf', '@@ROOT@@'), legendBlock(template));

// No new external dependencies, CDNs, network calls, or frameworks.
const networkPattern = /\b(?:fetch|XMLHttpRequest)\b|@import\b/i;
assert.match('@import url("a.css");', networkPattern, 'network regex must detect CSS @import');
assert.match('  @import "a.css";', networkPattern, 'network regex must detect indented CSS @import');
assert.match('fetch("/api")', networkPattern, 'network regex must detect fetch');
assert.match('new XMLHttpRequest()', networkPattern, 'network regex must detect XMLHttpRequest');
for (const [name, html] of [['graph.html', template], ['project-graph.html', mcp]]) {
  const externalScripts = [...html.matchAll(/<script\b[^>]*\bsrc="([^"]+)"/g)].map(m => m[1]);
  assert.deepEqual(externalScripts, [], `${name} must load no external scripts`);
  assert.doesNotMatch(html, /d3js|cdn\.jsdelivr|window\.d3/i, `${name} must not reference d3 or a CDN`);
  assert.doesNotMatch(html, networkPattern, `${name} must not make network calls`);
  const svgTag = html.match(/<svg\b[^>]*>/);
  assert.ok(svgTag, `${name} must include an <svg> element`);
  assert.doesNotMatch(svgTag[0], /\brole="img"/, `${name} svg must not use role="img" (hides interactive button children)`);
  assert.match(svgTag[0], /\brole="group"/, `${name} svg must use a container role that exposes interactive children`);
  assert.match(svgTag[0], /\baria-label="[^"]+"/, `${name} svg must keep an accessible aria-label`);
}

// Theme variables, focus visibility, and responsive layout CSS contract.
const css = style(template);
assert.match(css, /color-scheme:\s*light dark/);
assert.match(css, /@media\s*\(prefers-color-scheme:\s*dark\)/);
for (const token of ['--background', '--foreground', '--muted-foreground', '--border', '--primary', '--destructive', '--viz-series-1', '--viz-series-2', '--viz-series-3', '--viz-series-4', '--viz-series-5']) {
  assert.ok(css.includes(`var(${token},`), `CSS must use host variable ${token} with fallback`);
}
assert.match(css, /:focus-visible/);
assert.match(css, /\[data-layout="narrow"\]/);
assert.match(css, /\[data-layout="medium"\]/);

function render(data, options = {}) {
  const ids = new Map();
  let document;
  let currentWidth = options.width || 736;
  let resizeCallback = null;
  class Element {
    constructor(tag = 'div') {
      this.tagName = tag.toUpperCase(); this.children = []; this.attributes = {}; this.events = {};
      this.value = ''; this._text = ''; this.hidden = false; this.disabled = false;
      const classes = new Set();
      this.classList = {
        toggle(name, enabled) { if (enabled) classes.add(name); else classes.delete(name); },
        contains: name => classes.has(name)
      };
    }
    get textContent() { return this.children.length ? this.children.map(c => c.textContent).join('') : this._text; }
    set textContent(v) { this._text = String(v ?? ''); this.children = []; }
    append(...children) { children.forEach(child => { child.parent = this; child.parentElement = this; this.children.push(child); }); }
    replaceChildren(...children) { this.children = []; this.append(...children); }
    setAttribute(name, value) {
      this.attributes[name] = String(value);
      if (name === 'class') {
        for (const c of String(value).split(/\s+/).filter(Boolean)) this.classList.toggle(c, true);
      }
    }
    getAttribute(name) { return Object.prototype.hasOwnProperty.call(this.attributes, name) ? this.attributes[name] : null; }
    removeAttribute(name) { delete this.attributes[name]; }
    contains(target) {
      let cur = target;
      while (cur) {
        if (cur === this) return true;
        cur = cur.parent;
      }
      return false;
    }
    matches(selector) {
      const attr = selector.match(/^\[([\w-]+)\]$/);
      if (attr) return Object.prototype.hasOwnProperty.call(this.attributes, attr[1]);
      if (selector.startsWith('.')) return this.classList.contains(selector.slice(1));
      return this.tagName.toLowerCase() === selector.toLowerCase();
    }
    querySelector(selector) {
      if (selector.startsWith('#')) {
        if (!ids.has(selector)) {
          const tag = selector.endsWith('-svg') ? 'svg'
            : selector.endsWith('-search') ? 'input'
            : selector.endsWith('-draft') ? 'textarea'
            : /-(relation|confidence|state|scope|change|node)$/.test(selector) ? 'select'
            : /-(zoom-in|zoom-out|reset|clear|copy|ask)$/.test(selector) ? 'button'
            : 'div';
          const el = new Element(tag);
          el.parent = root;
          ids.set(selector, el);
        }
        return ids.get(selector);
      }
      if (selector === '.oms-graph-stage') return stage;
      for (const child of this.children) {
        if (child.matches && child.matches(selector)) return child;
        const found = child.querySelector ? child.querySelector(selector) : null;
        if (found) return found;
      }
      return null;
    }
    querySelectorAll(selector) {
      const out = [];
      const walk = node => {
        for (const child of node.children || []) {
          if (child.matches && child.matches(selector)) out.push(child);
          walk(child);
        }
      };
      walk(this);
      return out;
    }
    addEventListener(name, handler) { (this.events[name] ||= []).push(handler); }
    fire(name, event = {}) {
      let prevented = false;
      const payload = {target: this, preventDefault() { prevented = true; }, get defaultPrevented() { return prevented; }, ...event};
      for (const handler of this.events[name] || []) handler(payload);
      return payload;
    }
    get options() { return this.children; }
    focus() {
      const prev = document.activeElement;
      document.activeElement = this;
      this.fire('focus', {relatedTarget: prev});
    }
    blur() { document.activeElement = null; }
    setSelectionRange() {}
    getBoundingClientRect() { return {width: currentWidth, height: currentWidth <= 480 ? 400 : 520}; }
    closest(selector) { return this.matches(selector) ? this : this.parent?.closest(selector); }
  }
  const root = new Element('section');
  const stage = new Element('div');
  stage.parent = root;
  const svg = root.querySelector('#@@ROOT@@-svg');
  svg.append(new Element('title'));
  const legend = root.querySelector('#@@ROOT@@-legend');
  for (const match of legendBlock(template).matchAll(/<span\b([^>]*)>([\s\S]*?)<\/span>/g)) {
    const [, attrs, inner] = match;
    const item = new Element('span');
    const cls = attrs.match(/\bclass="([^"]*)"/);
    if (cls) item.setAttribute('class', cls[1]);
    const kindKey = attrs.match(/\bdata-kind-key="([^"]*)"/);
    if (kindKey) item.setAttribute('data-kind-key', kindKey[1]);
    const legendKey = attrs.match(/\bdata-legend="([^"]*)"/);
    if (legendKey) item.setAttribute('data-legend', legendKey[1]);
    if (/\bhidden\b/.test(attrs)) item.hidden = true;
    const glyphMatch = inner.match(/<i\b([^>]*)>([^<]*)<\/i>([\s\S]*)/);
    if (glyphMatch) {
      const icon = new Element('i');
      icon.textContent = glyphMatch[2];
      item.append(icon, {textContent: glyphMatch[3]});
    } else {
      item.textContent = inner.trim();
    }
    legend.append(item);
  }
  root.append(legend, stage);
  root.querySelector('#@@ROOT@@-data').textContent = JSON.stringify(data);
  document = new Element('document');
  document.activeElement = null;
  document.getElementById = () => root;
  document.createElementNS = (_namespace, tag) => new Element(tag);
  document.createElement = tag => new Element(tag);
  document.createTextNode = text => ({textContent: text});
  const frames = [];
  const timers = new Map(); let timerId = 0;
  let rafCalls = 0;
  const context = {document, window: {innerWidth: options.innerWidth, innerHeight: options.innerHeight, matchMedia: () => ({matches: !options.motion}), openai: options.openai}, navigator: {languages: options.languages || ['en-US']},
    localStorage: options.stored ? {getItem: key => key === 'oms.panel.language' ? options.stored : null} : undefined,
    Option: function(text, value) { const option = new Element('option'); option.textContent = text; option.value = value; return option; },
    ResizeObserver: class {
      constructor(callback) { this.callback = callback; resizeCallback = callback; }
      observe() { if (!options.noObserverFire) frames.push(() => this.callback([{contentRect: {width: currentWidth}}])); }
    },
    requestAnimationFrame: callback => { rafCalls += 1; return options.noRaf ? rafCalls : frames.push(callback); },
    setTimeout: callback => { timers.set(++timerId, callback); return timerId; },
    clearTimeout: id => { timers.delete(id); }};
  let clock = 0;
  const drain = () => { let guard = 0; while (frames.length && guard++ < 5000) { clock += 16.7; frames.shift()(clock); } };
  if (options.initWidth) stage.getBoundingClientRect = () => ({width: options.initWidth, height: 0});
  context.console = console; vm.runInNewContext(script(template), context);
  const rafBeforeFirstFrames = rafCalls;
  drain();
  const viewport = svg.children.find(child => child.tagName === 'G');
  const nodes = viewport?.children.filter(child => child.tagName === 'G').flatMap(group => group.children).filter(child => child.tagName === 'G') || [];
  const resize = nextWidth => {
    currentWidth = nextWidth;
    if (resizeCallback) resizeCallback([{contentRect: {width: nextWidth}}]);
    drain();
  };
  return {root, svg, stage, get: suffix => root.querySelector(`#@@ROOT@@-${suffix}`), nodes, viewport, document, resize, drain,
    flushTimers: () => { for (let n = 0; n < 50 && timers.size; n += 1) { const [id, fn] = timers.entries().next().value; timers.delete(id); fn(); } },
    rafCalls: () => rafCalls, pendingFrames: () => frames.length, rafBeforeFirstFrames};
}

function camera(view) {
  const m = view.viewport.attributes.transform.match(/^translate\((-?[\d.]+),(-?[\d.]+)\) scale\(([\d.]+)\)$/);
  assert.ok(m, `camera transform ${view.viewport.attributes.transform}`);
  return {x: Number(m[1]), y: Number(m[2]), k: Number(m[3])};
}
function positions(view) {
  return view.nodes.map(n => { const [, x, y] = n.attributes.transform.match(/^translate\(([\d.]+),([\d.]+)\)$/); return {x: Number(x), y: Number(y)}; });
}

function model(kind, count) {
  return {kind, title: `OMS ${kind === 'project' ? 'Project' : 'Execution'} Graph`, counts: {},
    nodes: Array.from({length: count}, (_, i) => ({id: `n${i}`,label: `Node ${i}`,path: `src/${i}.sh`,kind: 'file',show_label: i === 0,degree: 1,assurance: 'supported',status: 'done'})),
    edges: count > 1 ? [{source: 'n0',target: 'n1',relation: 'calls',confidence: 'REPEAT'}] : []};
}

// 1. Existing deterministic layouts, zoom/reset, filter/search, inspection.
for (const kind of ['project', 'execution']) for (const count of [0, 1, 30]) {
  const data = model(kind, count); const view = render(data);
  assert.equal(view.nodes.length, count);
  assert.equal(view.get('error').textContent, '');
  if (!count) {
    assert.equal(view.get('notice').hidden, false);
    assert.match(view.get('notice').textContent, /no nodes|no data/i);
    assert.match(view.get('visible').textContent, /0\/0.*no data/i);
    assert.match(view.get('detail').textContent, /no nodes/i);
    assert.equal(view.get('copy').disabled, true);
    continue;
  }
  const positions = view.nodes.map(node => node.attributes.transform);
  assert.ok(positions.every(position => /^translate\([\d.]+,[\d.]+\)$/.test(position)));
  assert.deepEqual(render(data).nodes.map(node => node.attributes.transform), positions);
  const fitK = camera(view).k;
  view.get('zoom-in').fire('click'); assert.ok(Math.abs(camera(view).k - fitK * 1.35) < 1e-2);
  view.get('zoom-out').fire('click'); assert.ok(Math.abs(camera(view).k - fitK) < 1e-2);
  view.get('search').value = 'missing'; view.get('search').fire('input');
  assert.ok(view.nodes.slice(1).every(node => node.classList.contains('is-muted')));
  view.get('search').focus(); view.document.fire('keydown', {key: 'Escape', target: view.get('search')});
  assert.equal(view.get('search').value, '');
  assert.ok(view.document.activeElement !== null, 'Escape from search must return focus sensibly');
  if (count > 1) {
    view.get('confidence').value = 'EXTRACTED'; view.get('confidence').fire('change');
    assert.ok(view.viewport.children[0].children[0].classList.contains('is-muted'));
    view.nodes[1].fire('click'); assert.match(view.get('detail').textContent, /n1/);
    assert.ok(view.get('draft').value.includes('n1'));
  }
  view.get('reset').fire('click'); assert.ok(Math.abs(camera(view).k - fitK) < 1e-2, 'reset returns to fit-all');
  assert.equal(view.get('search').value, '');
}

// 2. Narrow / medium / wide responsive widths (320 / 768 / 1024 CSS px), including 200-node non-overlap.
const responsiveData = model('project', 12);
for (const [w, expectedLayout, expectedHeight] of [[320, 'narrow', 400], [768, 'medium', 520], [1024, 'wide', 520]]) {
  const view = render(responsiveData, {width: w});
  assert.equal(view.root.attributes['data-layout'], expectedLayout);
  assert.equal(view.svg.attributes.viewBox, `0 0 ${w} ${expectedHeight}`);
  for (const n of view.nodes) {
    const [, x, y] = n.attributes.transform.match(/^translate\(([\d.]+),([\d.]+)\)$/);
    assert.ok(Number(x) >= 18 && Number(x) <= w - 18, `x=${x} within width ${w}`);
    assert.ok(Number(y) >= 18 && Number(y) <= expectedHeight - 18, `y=${y} within height ${expectedHeight}`);
  }
}

const denseData = {
  kind: 'project',
  title: 'Dense Project Graph',
  counts: {},
  nodes: Array.from({length: 200}, (_, i) => ({
    id: `d${i}`, label: `Node ${i}`, path: `src/${i}.rs`, kind: i % 2 === 0 ? 'file' : 'function',
    show_label: i < 4, degree: (i % 6) * 7, assurance: 'supported', status: 'done'
  })),
  edges: [{source: 'd0', target: 'd1', relation: 'calls', confidence: 'EXTRACTED'}]
};
for (const [w, expectedLayout, minHeight] of [[320, 'narrow', 400], [768, 'medium', 520], [1024, 'wide', 520]]) {
  const view = render(denseData, {width: w});
  assert.equal(view.root.attributes['data-layout'], expectedLayout);
  const vb = view.svg.attributes.viewBox.match(/^0 0 (\d+) (\d+)$/);
  assert.ok(vb, `200-node viewBox must be numeric for width ${w}`);
  const vbW = Number(vb[1]);
  const vbH = Number(vb[2]);
  assert.equal(vbW, w);
  assert.ok(vbH >= minHeight, `200-node stage height ${vbH} >= ${minHeight} at width ${w}`);
  if (expectedLayout === 'narrow') assert.ok(vbH <= 400, `narrow stage height ${vbH} stays viewport-capped`);
  const world = view.svg.attributes['data-world'].match(/^(\d+)x(\d+)$/);
  assert.ok(world && Number(world[1]) >= w && Number(world[2]) >= vbH, 'world extent is at least the stage');
  const worldW = Number(world[1]);
  const worldH = Number(world[2]);
  if (expectedLayout === 'narrow') {
    assert.ok(worldH > vbH, `narrow world ${worldH} grows past the stage ${vbH}`);
    assert.ok(Math.abs(worldW / worldH - w / vbH) < .02, `narrow world ${worldW}x${worldH} keeps the stage aspect ${w}x${vbH}`);
  } else assert.equal(worldW, w);
  const pts = view.nodes.map((n, idx) => {
    const [, x, y] = n.attributes.transform.match(/^translate\(([\d.]+),([\d.]+)\)$/);
    const r = Math.max(9, Math.min(14, 8 + Math.sqrt(denseData.nodes[idx].degree || 0)));
    assert.ok(Number(x) >= 18 && Number(x) <= worldW - 18, `200-node x=${x} within world ${worldW}`);
    assert.ok(Number(y) >= 18 && Number(y) <= worldH - 18, `200-node y=${y} within world ${worldH}`);
    return {x: Number(x), y: Number(y), r};
  });
  assert.ok(camera(view).k < 1 && camera(view).k >= .1, `200-node fit zooms out to show the world at width ${w}`);
  for (let i = 0; i < pts.length; i += 1) {
    for (let j = i + 1; j < pts.length; j += 1) {
      const dist = Math.hypot(pts[i].x - pts[j].x, pts[i].y - pts[j].y);
      const minDist = pts[i].r + pts[j].r;
      assert.ok(dist >= minDist, `200-node overlap at width ${w}: nodes ${i} and ${j} dist=${dist.toFixed(2)} < ${minDist.toFixed(2)}`);
    }
  }
  const tabStops = view.nodes.filter(n => n.attributes.tabindex === '0');
  assert.equal(tabStops.length, 1, `200-node graph must expose only 1 tab stop at width ${w}`);
  assert.equal(tabStops[0], view.nodes[0]);
}

// A same-class resize relaxes the rescaled positions: no overlap after 1200 -> 780 px with 200 nodes.
{
  const view = render(denseData, {width: 1200});
  view.resize(780);
  const pts = view.nodes.map((n, idx) => {
    const [, x, y] = n.attributes.transform.match(/^translate\(([\d.]+),([\d.]+)\)$/);
    return {x: Number(x), y: Number(y), r: Math.max(6, Math.min(16, 4.5 + 1.5 * Math.sqrt(denseData.nodes[idx].degree || 0))) + 3};
  });
  for (let i = 0; i < pts.length; i += 1) {
    for (let j = i + 1; j < pts.length; j += 1) {
      assert.ok(Math.hypot(pts[i].x - pts[j].x, pts[i].y - pts[j].y) >= pts[i].r + pts[j].r - .6, `resize overlap ${i}/${j}`);
    }
  }
}

// 3. Non-color status and node kind encoding (shapes, glyphs, patterns, legend, badges).
const richProject = {
  kind: 'project',
  title: 'OMS Project Graph',
  revision: 'abc123def456',
  counts: {shown_nodes: 5, total_nodes: 5, shown_edges: 2, total_edges: 2, hidden_test_nodes: 0},
  display: {production_only: true, task_focus: true, default_scope: 'neighbors'},
  nodes: [
    {id: 'f1', label: 'main.rs', path: 'src/main.rs', kind: 'file', degree: 3, focus: true, distance: 0, show_label: true, assurance: 'supported', assurance_label: 'Test-linked', change: 'changed'},
    {id: 'fn1', label: 'parse', path: 'src/parse.rs', kind: 'function', degree: 2, focus: false, distance: 1, show_label: true, assurance: 'needs-evidence', assurance_label: 'Needs evidence', change: 'impacted'},
    {id: 't1', label: 'parse_test', path: 'tests/parse.rs', kind: 'test', degree: 1, focus: false, distance: 1, show_label: true, assurance: 'attention', assurance_label: 'Attention'},
    {id: 'a1', label: 'builder', path: '', kind: 'agent', degree: 1, focus: false, distance: 2, show_label: false, assurance: 'test-evidence', assurance_label: 'Test evidence'},
    {id: 'term1', label: 'done', path: '', kind: 'terminal', degree: 1, focus: false, distance: 3, show_label: false, assurance: 'supported', assurance_label: 'Test-linked'}
  ],
  edges: [
    {source: 'f1', target: 'fn1', relation: 'calls', confidence: 'EXTRACTED'},
    {source: 'fn1', target: 't1', relation: 'tested_by', confidence: 'INFERRED'}
  ]
};
const richView = render(richProject, {width: 768});
const shapes = richView.nodes.map(n => n.querySelector('.oms-graph-node-body').attributes.d);
assert.equal(new Set(shapes).size, 5, 'file, function, test, agent, terminal must have distinct shapes');
const glyphs = richView.nodes.map(n => n.querySelector('.oms-graph-glyph').textContent);
assert.deepEqual(glyphs.slice(0, 4), ['✓', '?', '!', 'T']);
const patterns = richView.nodes.map(n => n.attributes['data-pattern']);
assert.deepEqual(patterns.slice(0, 4), ['solid', 'dashed', 'dash-dot', 'dotted']);
assert.equal(richView.get('legend').hidden, false);
assert.equal(richView.get('badges').hidden, false);
assert.match(richView.get('badges').textContent, /file/i);
assert.match(richView.get('badges').textContent, /Test-linked/);
assert.match(richView.get('badges').textContent, /changed/i);

const richExec = {
  kind: 'execution',
  title: 'OMS Execution Graph',
  counts: {shown_nodes: 7, total_nodes: 7, shown_edges: 2, total_edges: 2},
  nodes: [
    {id: 'e1', label: 'Plan', kind: 'agent', degree: 1, focus: true, show_label: true, status: 'active'},
    {id: 'e2', label: 'Gate', kind: 'gate', degree: 2, focus: false, show_label: true, status: 'failed'},
    {id: 'e3', label: 'Verify', kind: 'node', degree: 1, focus: false, show_label: true, status: 'pending'},
    {id: 'e4', label: 'Finish', kind: 'terminal', degree: 1, focus: false, show_label: true, status: 'finished'},
    {id: 'e5', label: 'Wait', kind: 'node', degree: 1, focus: false, show_label: true, status: 'blocked'},
    {id: 'e6', label: 'Hold', kind: 'node', degree: 1, focus: false, show_label: true, status: 'waiting'},
    {id: 'e7', label: 'Queue', kind: 'node', degree: 1, focus: false, show_label: true, status: 'ready'}
  ],
  edges: [
    {source: 'e1', target: 'e2', relation: 'next', confidence: 'NORMAL'},
    {source: 'e2', target: 'e4', relation: 'pass', confidence: 'NORMAL'}
  ]
};
const execView = render(richExec, {width: 768});
assert.equal(execView.get('legend').hidden, false, 'execution graph must also show status/kind legend');
const execGlyphs = execView.nodes.map(n => n.querySelector('.oms-graph-glyph').textContent);
const execPatterns = execView.nodes.map(n => n.attributes['data-pattern']);
assert.deepEqual(execGlyphs.slice(0, 6), ['▶', '×', '·', '■', '⊘', '!']);
assert.notEqual(execGlyphs[6], execGlyphs[2], 'ready must have a distinct glyph from pending');
{
  const meta = script(template).match(/const STATE_META = \{([\s\S]*?)\n  \};/)[1];
  const pairs = [...meta.matchAll(/(\w+): \{glyph: '([^']+)', pattern: '([^']+)'\}/g)].map(m => `${m[2]}|${m[3]}`);
  assert.ok(pairs.length >= 13);
  assert.equal(new Set(pairs).size, pairs.length, 'every execution state differs from every other in glyph or pattern');
  const unk = render({kind: 'execution', title: 'x', counts: {}, nodes: [{id: 'u1', label: 'U', kind: 'node', degree: 0, status: 'cancelled'}, {id: 'u2', label: 'V', kind: 'node', degree: 0}], edges: []}, {width: 768});
  assert.deepEqual(unk.nodes.map(n => n.attributes['data-state']), ['unknown', 'unknown']);
  assert.deepEqual([...unk.get('state').children].map(o => o.value).filter(Boolean), ['unknown']);
}
assert.notEqual(execPatterns[6], execPatterns[2], 'ready must have a distinct pattern from pending');
const visibleLegendGlyphs = (view, cls) => new Set(
  view.get('legend').children
    .filter(item => !item.hidden && item.classList.contains(cls))
    .map(item => item.querySelector('i')?.textContent)
    .filter(Boolean)
);
const execLegendGlyphs = visibleLegendGlyphs(execView, 'oms-exec-key');
for (const glyph of execGlyphs) {
  assert.ok(execLegendGlyphs.has(glyph), `execution legend must explain glyph ${JSON.stringify(glyph)}`);
}
const execLegendText = execView.get('legend').children.filter(item => !item.hidden && item.classList.contains('oms-exec-key')).map(item => item.textContent).join(' | ');
assert.match(execLegendText, /blocked|waiting/i, 'execution legend must explain blocked/waiting');
assert.match(execLegendText, /ready/i, 'execution legend must explain ready');

const unclassifiedProject = render({
  kind: 'project',
  title: 'Unclassified Project Graph',
  counts: {shown_nodes: 5, total_nodes: 5, shown_edges: 0, total_edges: 0},
  nodes: [
    {id: 'u1', label: 'a.rs', path: 'src/a.rs', kind: 'file', degree: 1, show_label: true, assurance: 'supported'},
    {id: 'u2', label: 'b.rs', path: 'src/b.rs', kind: 'file', degree: 1, show_label: true, assurance: 'needs-evidence'},
    {id: 'u3', label: 'c.rs', path: 'src/c.rs', kind: 'file', degree: 1, show_label: true, assurance: 'attention'},
    {id: 'u4', label: 'd.rs', path: 'src/d.rs', kind: 'test', degree: 1, show_label: true, assurance: 'test-evidence'},
    {id: 'u5', label: 'e.rs', path: 'src/e.rs', kind: 'file', degree: 1, show_label: true, assurance: ''}
  ],
  edges: []
}, {width: 768});
const projectLegendGlyphs = visibleLegendGlyphs(unclassifiedProject, 'oms-project-key');
for (const nodeEl of unclassifiedProject.nodes) {
  const glyph = nodeEl.querySelector('.oms-graph-glyph').textContent;
  assert.ok(projectLegendGlyphs.has(glyph), `project legend must explain glyph ${JSON.stringify(glyph)}`);
}

// 4. Keyboard navigation, roving tabindex, selection persistence across search/filter/zoom/resize, Escape & clear focus restoration.
assert.equal(richView.nodes[0].attributes.role, 'button');
assert.equal(richView.nodes[0].attributes.tabindex, '0');
assert.equal(richView.nodes[0].attributes['aria-pressed'], 'true');
assert.match(richView.nodes[0].attributes['aria-label'], /main\.rs.*file.*Test-linked/);
// Roving tabindex: only the selected (or first visible) node has tabindex=0; all others have -1.
assert.deepEqual(richView.nodes.map(n => n.attributes.tabindex), ['0', '-1', '-1', '-1', '-1']);

// Select fn1 via keyboard Enter without stealing focus to draft textarea.
richView.nodes[1].focus();
richView.nodes[1].fire('keydown', {key: 'Enter'});
assert.equal(richView.get('node').value, 'fn1');
assert.equal(richView.nodes[1].attributes['aria-pressed'], 'true');
assert.equal(richView.document.activeElement, richView.nodes[1], 'keyboard node selection must not steal focus to draft');
assert.deepEqual(richView.nodes.map(n => n.attributes.tabindex), ['-1', '0', '-1', '-1', '-1']);

// ArrowRight/ArrowDown and ArrowLeft/ArrowUp move focus and the roving tab stop across visible nodes.
richView.nodes[1].fire('keydown', {key: 'ArrowRight'});
assert.equal(richView.document.activeElement, richView.nodes[2]);
assert.deepEqual(richView.nodes.map(n => n.attributes.tabindex), ['-1', '-1', '0', '-1', '-1']);
richView.nodes[2].fire('keydown', {key: 'ArrowDown'});
assert.equal(richView.document.activeElement, richView.nodes[0]);
assert.deepEqual(richView.nodes.map(n => n.attributes.tabindex), ['0', '-1', '-1', '-1', '-1']);
richView.nodes[0].fire('keydown', {key: 'ArrowUp'});
assert.equal(richView.document.activeElement, richView.nodes[2]);
assert.deepEqual(richView.nodes.map(n => n.attributes.tabindex), ['-1', '-1', '0', '-1', '-1']);
richView.nodes[2].fire('keydown', {key: 'ArrowLeft'});
assert.equal(richView.document.activeElement, richView.nodes[1]);
assert.deepEqual(richView.nodes.map(n => n.attributes.tabindex), ['-1', '0', '-1', '-1', '-1']);
// Move tab stop to t1, then filter it out: tab stop falls back to selected visible node fn1.
richView.nodes[1].fire('keydown', {key: 'ArrowRight'});
assert.equal(richView.nodes[2].attributes.tabindex, '0');

// Edit unsent draft on fn1, then search/filter/zoom/resize -> selection and unsent draft stay intact.
richView.get('draft').value = 'Custom user note for fn1';
richView.get('draft').fire('input');
richView.get('search').value = 'nonexistent-query';
richView.get('search').fire('input');
richView.get('state').value = 'supported';
richView.get('state').fire('change');
richView.get('zoom-in').fire('click');
richView.resize(320);
assert.equal(richView.root.attributes['data-layout'], 'narrow');
assert.equal(richView.get('node').value, 'fn1');
assert.equal(richView.nodes[1].classList.contains('is-selected'), true);
assert.equal(richView.nodes[1].classList.contains('is-muted'), false);
assert.deepEqual(richView.nodes.map(n => n.attributes.tabindex), ['-1', '0', '-1', '-1', '-1']);
assert.equal(richView.get('draft').value, 'Custom user note for fn1');
const fn1Option = richView.get('node').options.find(opt => opt.value === 'fn1');
assert.equal(fn1Option.hidden, false, 'selected option must remain visible during search');
assert.match(richView.get('notice').textContent, /No nodes match/i);
assert.match(richView.get('visible').textContent, /hidden/);

// Switch to t1 and back to fn1 -> custom unsent draft on fn1 is preserved.
richView.nodes[2].fire('click');
assert.equal(richView.get('node').value, 't1');
assert.ok(richView.get('draft').value.includes('t1'));
richView.nodes[1].fire('click');
assert.equal(richView.get('draft').value, 'Custom user note for fn1');

// '/' remembers origin focus and Escape restores focus to that element.
richView.get('zoom-in').focus();
richView.document.fire('keydown', {key: '/', target: richView.get('zoom-in')});
assert.equal(richView.document.activeElement, richView.get('search'));
richView.document.fire('keydown', {key: 'Escape', target: richView.get('search')});
assert.equal(richView.get('search').value, '');
assert.equal(richView.document.activeElement, richView.get('zoom-in'));

// Escape from draft returns focus to selected SVG node.
richView.get('draft').focus();
richView.document.fire('keydown', {key: 'Escape', target: richView.get('draft')});
assert.equal(richView.document.activeElement, richView.nodes[1]);

// Clear filters resets filters, preserves selection, and focuses search sensibly.
richView.get('state').value = 'attention';
richView.get('state').fire('change');
richView.get('clear').fire('click');
assert.equal(richView.get('state').value, '');
assert.equal(richView.get('search').value, '');
assert.equal(richView.get('node').value, 'fn1');
assert.equal(richView.document.activeElement, richView.get('search'));

// 5. Explicit empty / partial / truncated / no-data states (never fabricated).
const invalid = render({nodes: null, edges: []});
assert.equal(invalid.get('error').hidden, false);
assert.match(invalid.get('error').textContent, /No graph data/);
assert.match(invalid.get('summary').textContent, /No data/i);
assert.match(invalid.get('visible').textContent, /No data/i);
assert.equal(invalid.get('copy').disabled, true);
assert.equal(invalid.stage.hidden, true, 'no blank canvas box without data');

const templatePlaceholder = render({nodes: [], edges: [], display: {resource_template: true}});
assert.equal(templatePlaceholder.get('error').hidden, true, 'an unpopulated template is a calm status, not an alert');
assert.equal(templatePlaceholder.get('notice').hidden, false);
assert.match(templatePlaceholder.get('notice').textContent, /No graph data/);
assert.match(template.match(/<div id="@@ROOT@@-notice"[^>]*>/)[0], /role="status"/);
assert.match(template.match(/<div id="@@ROOT@@-error"[^>]*>/)[0], /role="alert"/);

const partialTruncated = render({
  kind: 'project',
  title: 'Partial Project Graph',
  revision: 'deadbeef1234',
  counts: {shown_nodes: 2, total_nodes: 10, shown_edges: 1, total_edges: 6, hidden_test_nodes: 4},
  coverage: {parsed: 8, unparsed: 3},
  display: {production_only: true},
  nodes: [
    {id: 'a', label: 'a.rs', path: 'src/a.rs', kind: 'file', degree: 1, show_label: true, assurance: 'supported', assurance_label: 'Test-linked'},
    {id: 'b', label: 'b.rs', path: 'src/b.rs', kind: 'file', degree: 1, show_label: true, assurance: 'needs-evidence', assurance_label: 'Needs evidence'}
  ],
  edges: [{source: 'a', target: 'b', relation: 'calls', confidence: 'EXTRACTED'}]
});
assert.match(partialTruncated.get('summary').textContent, /2\/10 nodes · 1\/6 edges/);
assert.equal(partialTruncated.get('notice').hidden, false);
assert.match(partialTruncated.get('notice').textContent, /8 nodes hidden/);
assert.match(partialTruncated.get('notice').textContent, /5 edges hidden/);
assert.match(partialTruncated.get('notice').textContent, /4 test nodes hidden/);
assert.match(partialTruncated.get('notice').textContent, /3 unparsed/);

const partialFlag = render({
  kind: 'project',
  title: 'Partial Flag Graph',
  completeness: 'partial',
  counts: {shown_nodes: 1, total_nodes: 1, shown_edges: 0, total_edges: 0},
  nodes: [{id: 'p1', label: 'p1.rs', path: 'src/p1.rs', kind: 'file', degree: 0, show_label: true, assurance: 'supported'}],
  edges: []
});
assert.equal(partialFlag.get('notice').hidden, false);
assert.match(partialFlag.get('notice').textContent, /Partial graph/i);
assert.match(partialFlag.get('summary').textContent, /partial graph/i);

// 6. Native force layout: deterministic, not a lattice, fit-to-view, focus-on-select, reduced motion.
function chain(kind, count) {
  const data = model(kind, count);
  data.edges = Array.from({length: count - 1}, (_, i) => ({source: `n${i}`, target: `n${(i * 7 + 1) % count === i ? i + 1 : (i * 7 + 1) % count}`, relation: i % 2 ? 'calls' : 'tests', confidence: 'EXTRACTED'}));
  data.nodes.forEach((row, i) => { row.degree = 2 + (i % 3); row.show_label = i < 3; });
  return data;
}
for (const kind of ['project', 'execution']) {
  const data = chain(kind, 40);
  const first = render(data); const second = render(data);
  assert.deepEqual(positions(first), positions(second), `${kind} layout must be deterministic`);
  const pts = positions(first);
  assert.ok(new Set(pts.map(p => p.x.toFixed(1))).size > 20 && new Set(pts.map(p => p.y.toFixed(1))).size > 20, `${kind} layout must not sit on a fixed lattice`);
  const cam = camera(first);
  assert.ok(cam.k >= .3 && cam.k <= 5);
  const stageH = Number(first.svg.attributes.viewBox.split(' ')[3]);
  for (const p of pts) {
    const sx = cam.x + p.x * cam.k; const sy = cam.y + p.y * cam.k;
    assert.ok(sx >= 0 && sx <= 736 && sy >= 0 && sy <= stageH, `${kind} fit-to-view keeps node inside stage (${sx},${sy})`);
  }
  // Selecting a node moves/zooms the view; Reset and Escape return to fit-all.
  const before = camera(first);
  first.nodes[5].fire('click');
  assert.deepEqual(camera(first), before, 'click keeps the fitted view when the node is already visible');
  first.nodes[5].fire('keydown', {key: 'Escape'});
  assert.deepEqual(camera(first), before, 'Esc returns to fit-all');
  assert.equal(first.document.activeElement, first.get('search'), 'Escape from a node moves focus to the visible search input');
  first.nodes[5].fire('keydown', {key: '+'}); assert.ok(camera(first).k > before.k);
  first.nodes[5].fire('keydown', {key: '0'}); assert.deepEqual(camera(first), before, '0 fits all');
  // Hover highlights neighbours and dims the rest; leaving clears it. Drag pin / unpin by double-click.
  const edgeEls = first.viewport.children[0].children;
  first.nodes[0].fire('pointerenter');
  assert.ok(first.nodes.some(n => n.classList.contains('is-dim')) && first.nodes[0].classList.contains('is-dim') === false);
  assert.ok(edgeEls.some(e => e.classList.contains('is-hot')));
  first.nodes[0].fire('pointerleave');
  assert.ok(first.nodes.every(n => !n.classList.contains('is-dim')));
  assert.ok(edgeEls.some(e => e.classList.contains('is-flow')) === true, 'selected node edges flow');
  const svgPts = first.svg;
  svgPts.fire('pointerdown', {pointerId: 1, button: 0, clientX: 100, clientY: 100, target: first.nodes[0]});
  svgPts.fire('pointermove', {pointerId: 1, clientX: 140, clientY: 160});
  svgPts.fire('pointerup', {pointerId: 1});
  assert.equal(first.nodes[0].attributes['data-pinned'], 'true');
  assert.match(first.nodes[0].attributes['aria-label'], /pinned/);
  first.nodes[0].fire('click');
  svgPts.fire('dblclick', {target: first.nodes[0]});
  assert.equal(first.nodes[0].attributes['data-pinned'], 'false', 'click then double-click unpins');
  first.nodes[1].fire('keydown', {key: 'p'});
  assert.equal(first.nodes[1].attributes['data-pinned'], 'true', 'p pins a focused node');
  first.nodes[2].fire('keydown', {key: 'p'}); first.nodes[1].fire('keydown', {key: 'p'});
  assert.equal(first.nodes[1].attributes['data-pinned'], 'false', 'p unpins');
  first.nodes[1].fire('keydown', {key: 'p'}); first.get('reset').fire('click');
  assert.ok(first.nodes.every(n => n.attributes['data-pinned'] !== 'true'), 'Reset view releases every pin');
}
function dragNode(view, index, id, x, y) {
  const c = camera(view); const p = positions(view)[index];
  const from = {clientX: c.x + p.x * c.k, clientY: c.y + p.y * c.k};
  view.svg.fire('pointerdown', {pointerId: id, button: 0, ...from, target: view.nodes[index]});
  view.svg.fire('pointermove', {pointerId: id, clientX: x, clientY: y});
  view.svg.fire('pointerup', {pointerId: id});
}
// Round 9: one camera padding, pin-aware recentring, scope refit, one-shot click suppression, honest badges.
for (const w of [320, 736]) {
  const view = render(chain('project', 40), {width: w});
  const fitted = camera(view);
  view.nodes.forEach(n => n.fire('click'));
  assert.deepEqual(camera(view), fitted, `${w}px: clicking any node already inside the stage never moves the camera`);
  const g = view.nodes[0]; const svg = view.svg;
  dragNode(view, 0, 1, 1, 1);
  assert.equal(g.attributes['data-pinned'], 'true');
  const afterDrag = camera(view);
  g.fire('click'); assert.deepEqual(camera(view), afterDrag, `${w}px: click on a node pinned at the border keeps the camera`);
  svg.fire('dblclick', {target: g});
  assert.equal(g.attributes['data-pinned'], 'false', `${w}px: click then double-click unpins a border-pinned node`);
  // suppressClick swallows only the click that ends the drag.
  dragNode(view, 1, 2, 150, 150);
  view.flushTimers();
  view.nodes[2].fire('click');
  assert.match(view.get('detail').textContent, /n2/, `${w}px: a later programmatic click is not swallowed`);
}
{ // Free nodes recentre on their own centroid, not on one pulled by a far pin.
  const view = render(chain('project', 6));
  const centroid = pts => ({x: pts.reduce((a, p) => a + p.x, 0) / pts.length, y: pts.reduce((a, p) => a + p.y, 0) / pts.length});
  const centre = centroid(positions(view));
  dragNode(view, 0, 1, 1, 1); view.flushTimers();
  const free = centroid(positions(view).slice(1));
  assert.ok(Math.hypot(free.x - centre.x, free.y - centre.y) < 4, `unpinned nodes keep their own centroid at the world centre (${free.x.toFixed(1)},${free.y.toFixed(1)} vs ${centre.x.toFixed(1)},${centre.y.toFixed(1)})`);
}
{ // Task scope and Clear filters always refit to the new scope; Reset view fits the current scope.
  const data = chain('project', 40);
  data.display = {production_only: true, task_focus: true, default_scope: 'neighbors'};
  data.nodes.forEach((row, i) => { row.focus = i === 0; row.distance = i === 0 ? 0 : (i < 3 ? 1 : 3); });
  const view = render(data); const sc = view.get('scope');
  const narrow = camera(view);
  sc.value = 'all'; sc.fire('change');
  const wide = camera(view);
  assert.ok(wide.k < narrow.k, 'widening the scope refits to the larger node set');
  view.get('zoom-in').fire('click');
  sc.value = 'neighbors'; sc.fire('change');
  assert.ok(Math.abs(camera(view).k - narrow.k) < 1e-2, 'scope change refits even after the camera was moved');
  sc.value = 'all'; sc.fire('change'); view.get('zoom-in').fire('click');
  view.get('reset').fire('click');
  assert.equal(sc.value, 'all', 'Reset view does not change the scope');
  assert.ok(Math.abs(camera(view).k - wide.k) < 1e-2, 'Reset view fits the nodes in the current scope');
  view.get('clear').fire('click');
  assert.equal(sc.value, 'neighbors', 'Clear filters resets the scope');
}
{ // Status badges use the node's state tokens; unclassified and exited are never the verified colour.
  const view = render({kind: 'execution', title: 'x', counts: {}, edges: [], nodes: [{id: 'a', label: 'A', kind: 'node', degree: 0, status: 'exited', show_label: true}]});
  view.nodes[0].fire('click');
  const badge = view.get('badges').children.find(c => c.attributes['data-pattern']);
  assert.equal(badge.attributes['data-state'], 'exited');
  const project = render({kind: 'project', title: 'x', counts: {}, edges: [], nodes: [{id: 'a', label: 'A', kind: 'file', degree: 0, show_label: true}]});
  project.nodes[0].fire('click');
  assert.equal(project.get('badges').children.find(c => c.attributes['data-pattern']).attributes['data-assurance'], 'unclassified');
  for (const html of [template, mcp]) {
    assert.doesNotMatch(style(html), /data-focus="true"\] \.oms-graph-node-body \{[^}]*stroke-dasharray/, 'focus halo never replaces the stroke dash');
    assert.match(style(html), /\.oms-badge\[data-state\][^{]*\{ border-color: var\(--oms-st\)/);
  }
}
// Reduced motion computes the settled layout before first paint and never requests animation frames.
{
  const data = chain('project', 40);
  const view = render(data);
  assert.equal(view.rafCalls(), 0, 'reduced motion must not request animation frames');
  view.nodes[3].fire('click'); view.get('zoom-in').fire('click'); view.get('reset').fire('click'); view.resize(500);
  assert.equal(view.rafCalls(), 0, 'reduced motion interactions stay frame-free');
}
// Full motion animates the settle then eases to fit-all, and the animation ends.
{
  const data = chain('project', 40);
  const view = render(data, {motion: true});
  assert.ok(view.rafCalls() > 0, 'full motion animates the layout');
  assert.equal(view.pendingFrames(), 0, 'animation settles and stops');
  const settled = camera(view);
  assert.ok(settled.k > 0 && !Number.isNaN(settled.x));
  const startPositions = positions(view);
  view.nodes[2].fire('keydown', {key: 'Enter'}); view.drain();
  assert.notDeepEqual(camera(view), settled);
  assert.deepEqual(positions(view), startPositions, 'camera moves do not move nodes');
}

// 7. Design round 2: labels, edge groups, vocabulary, language, filters, empty state, rings, hubs.
const radiusOf = degree => Math.max(6, Math.min(16, 4.5 + 1.5 * Math.sqrt(degree || 0)));
const labelOf = el => el.querySelector('.oms-graph-label');
{ // AUD-02: labels centred below the node, collisions hidden, full text in <title> and the selection card.
  const long = 'a_very_long_module_name_that_gets_truncated.rs';
  const data = chain('project', 40);
  data.nodes.forEach((row, i) => { row.show_label = true; row.label = i === 3 ? long : `module_${i}.rs`; });
  const view = render(data, {width: 768});
  const cam = camera(view); const pts = positions(view);
  view.nodes.forEach(n => {
    const label = labelOf(n);
    assert.equal(label.attributes['text-anchor'], 'middle');
    assert.match(label.attributes.transform, /^translate\(0,[\d.]+\) scale\([\d.]+\)$/);
  });
  const boxes = view.nodes.map((n, i) => ({i, hidden: labelOf(n).classList.contains('is-hidden'), text: labelOf(n).textContent}))
    .filter(b => !b.hidden).map(b => {
      const half = (b.text.length * 6.4 + 6) / 2; const cx = cam.x + pts[b.i].x * cam.k;
      const top = cam.y + (pts[b.i].y + radiusOf(data.nodes[b.i].degree)) * cam.k + 4;
      return [cx - half, top, cx + half, top + 14];
    });
  assert.ok(boxes.length > 0 && boxes.length < view.nodes.length, 'some labels are hidden because they would collide');
  for (let a = 0; a < boxes.length; a += 1) for (let b = a + 1; b < boxes.length; b += 1) {
    const [p, q] = [boxes[a], boxes[b]];
    assert.ok(p[2] <= q[0] || q[2] <= p[0] || p[3] <= q[1] || q[3] <= p[1], `visible labels ${a} and ${b} must not overlap`);
  }
  assert.equal(labelOf(view.nodes[3]).textContent.endsWith('…'), true, 'long labels are truncated');
  assert.equal(view.nodes[3].querySelector('title').textContent, long, 'the node <title> carries the full label');
  assert.equal(view.nodes[0].querySelector('title').textContent, '', 'untruncated labels need no <title>');
  const hiddenIndex = view.nodes.findIndex(n => labelOf(n).classList.contains('is-hidden'));
  view.nodes[hiddenIndex].fire('pointerenter');
  assert.equal(labelOf(view.nodes[hiddenIndex]).classList.contains('is-hidden'), false, 'hover reveals a colliding label');
  view.nodes[hiddenIndex].fire('pointerleave');
  assert.equal(labelOf(view.nodes[hiddenIndex]).classList.contains('is-hidden'), true);
  view.nodes[hiddenIndex].fire('click');
  assert.equal(labelOf(view.nodes[hiddenIndex]).classList.contains('is-hidden'), false, 'selected label always shows');
  view.nodes[3].fire('click');
  assert.ok(view.get('detail').textContent.includes(long), 'the selection card shows the full label');
  const shown = () => view.nodes.filter(n => !labelOf(n).classList.contains('is-hidden')).length;
  const atFit = shown();
  for (let i = 0; i < 4; i += 1) view.get('zoom-in').fire('click');
  assert.ok(shown() > atFit, `zooming in reveals labels (${atFit} -> ${shown()})`);
}
{ // AUD-03: ownership vs dependency edges.
  const data = model('project', 4);
  data.edges = [
    {source: 'n0', target: 'n1', relation: 'contains', confidence: 'EXTRACTED'},
    {source: 'n0', target: 'n2', relation: 'imports', confidence: 'EXTRACTED'},
    {source: 'n2', target: 'n3', relation: 'tested_by', confidence: 'INFERRED'}
  ];
  const view = render(data);
  const edges = view.viewport.children[0].children;
  assert.deepEqual(edges.map(e => e.attributes['data-relation-group']), ['ownership', 'dependency', 'dependency']);
  assert.equal(edges[0].attributes['marker-end'], undefined, 'ownership has no arrowhead');
  assert.match(edges[1].attributes['marker-end'], /^url\(#/);
  assert.doesNotMatch(edges[0].attributes.d, /Q/, 'ownership is a straight line');
  assert.match(edges[1].attributes.d, /Q/, 'dependency is curved');
  view.nodes[0].fire('click');
  assert.equal(edges[0].attributes['marker-end'], undefined, 'selection highlight keeps ownership arrowless');
  const legendItems = view.get('legend').children.filter(item => !item.hidden).map(item => item.getAttribute('data-legend'));
  assert.ok(legendItems.includes('ownership') && legendItems.includes('dependency'), 'legend lists both edge groups');
  assert.match(css, /data-relation-group="ownership"\][^}]*stroke-width:\s*1\.25;\s*opacity:\s*\.35;\s*stroke-dasharray:\s*none/);
  assert.match(css, /data-relation-group="dependency"\][^}]*stroke-width:\s*1\.25;\s*opacity:\s*\.6/);
  assert.match(css, /\[data-relation-group\]\.is-hot[^}]*stroke-width:\s*1\.75/);
  assert.match(css, /oms-graph-node-body \{ fill-opacity: \.9;/, 'nodes are not washed out');
  const noOwnership = render(model('project', 2));
  assert.ok(!noOwnership.get('legend').children.some(item => !item.hidden && item.getAttribute('data-legend') === 'ownership'));
}
{ // AUD-04: one state vocabulary; legend and node colours share one token; symbol kind is not a status glyph.
  assert.doesNotMatch(css, /data-status=/, 'styling keys off the shared state, not raw status strings');
  assert.match(css, /--oms-st:/);
  const exec = {kind: 'execution', title: 'x', counts: {}, edges: [], nodes: [
    ['running', '▶', 'Running'], ['active', '▶', 'Running'], ['verifying', '⟳', 'Verifying'], ['in_review', '⚑', 'In review'],
    ['waiting_approval', '!', 'Needs you'], ['blocked', '⊘', 'Blocked'], ['ready', '▷', 'Ready'], ['queued', '·', 'Pending'],
    ['done', '■', 'Exited'], ['completed', '✓', 'Verified'], ['error', '×', 'Failed'], ['timed_out', '◷', 'Timed out'],
    ['partial', '~', 'Stale'], ['unknown', '?', 'Unknown']
  ].map(([status], i) => ({id: `s${i}`, label: `S${i}`, kind: i === 0 ? 'function' : 'node', degree: 0, status, show_label: true}))};
  const vocabulary = [['running', '▶', 'Running'], ['active', '▶', 'Running'], ['verifying', '⟳', 'Verifying'], ['in_review', '⚑', 'In review'],
    ['waiting_approval', '!', 'Needs you'], ['blocked', '⊘', 'Blocked'], ['ready', '▷', 'Ready'], ['queued', '·', 'Pending'],
    ['done', '■', 'Exited'], ['completed', '✓', 'Verified'], ['error', '×', 'Failed'], ['timed_out', '◷', 'Timed out'],
    ['partial', '~', 'Stale'], ['unknown', '?', 'Unknown']];
  const view = render(exec);
  view.nodes.forEach((n, i) => {
    assert.equal(n.querySelector('.oms-graph-glyph').textContent, vocabulary[i][1], vocabulary[i][0]);
    assert.ok(n.attributes['aria-label'].includes(vocabulary[i][2]), `${vocabulary[i][0]} -> ${vocabulary[i][2]}`);
  });
  const legendText = view.get('legend').children.filter(item => !item.hidden).map(item => item.textContent).join('|');
  for (const word of ['Running', 'Verifying', 'In review', 'Needs you', 'Blocked', 'Ready', 'Pending', 'Exited', 'Verified', 'Failed', 'Timed out', 'Stale', 'Unknown']) {
    assert.ok(legendText.includes(word), `legend explains ${word}`);
  }
  assert.match(view.nodes[0].querySelector('.oms-graph-glyph').textContent, /▶/);
  assert.match(view.get('badges').textContent + view.get('node').children.map(o => o.textContent).join(''), /ƒ function/, 'symbol kind glyph is ƒ');
  assert.ok(!script(template).includes('○'), 'the pending glyph is no longer overloaded as a kind glyph');
  assert.match(legendBlock(template), /data-kind="symbol"[^>]*>ƒ</);
}
{ // AUD-05: ko/en negotiation (stored > host locale > navigator > ko) and a host-neutral ask button.
  const data = model('project', 3);
  const ko = render(data, {languages: ['ko-KR']});
  assert.equal(ko.get('search-label').textContent, '노드 검색');
  assert.equal(ko.get('ask').textContent, '대화에 보내기');
  assert.equal(ko.get('zoom-out').textContent, '축소');
  assert.equal(ko.get('relation').children[0].textContent, '모든 관계');
  assert.match(ko.get('summary').textContent, /^노드 3\/3/);
  assert.match(ko.get('filters-summary').textContent, /^필터 · 0개 적용$/);
  assert.equal(ko.root.attributes.lang, 'ko');
  assert.equal(render(data, {languages: ['en-GB', 'ko']}).get('ask').textContent, 'Send to chat');
  assert.equal(render(data, {languages: []}).get('search-label').textContent, '노드 검색', 'no signal falls back to Korean');
  assert.equal(render(data, {languages: ['en-US'], openai: {locale: 'ko_KR'}}).get('search-label').textContent, '노드 검색', 'host locale beats navigator');
  assert.equal(render(data, {languages: ['ko-KR'], openai: {locale: 'en-US'}, stored: 'en'}).get('search-label').textContent, 'Search nodes', 'stored choice wins');
  assert.equal(render(data, {languages: ['ko-KR'], stored: 'fr'}).get('search-label').textContent, '노드 검색', 'unknown stored language is ignored');
  for (const [name, html] of [['graph.html', template], ['project-graph.html', mcp]]) assert.doesNotMatch(html, /Send to Codex/, `${name} ask button is host-neutral`);
  const koExec = render({kind: 'execution', title: 'x', counts: {}, edges: [], nodes: [{id: 'a', label: 'A', kind: 'node', degree: 0, status: 'failed', show_label: true}]}, {languages: ['ko']});
  assert.match(koExec.nodes[0].attributes['aria-label'], /실패/);
  assert.equal(render({nodes: null, edges: []}, {languages: ['ko']}).get('error').textContent, '이 화면에 전달된 그래프 데이터가 없습니다.');
}
{ // AUD-07: only search (and task scope) above the canvas; filters in a collapsed disclosure; legend below, only present encodings.
  for (const html of [template, mcp]) {
    const primary = html.match(/id="[^"]*-primary"[\s\S]*?<\/div>/)[0];
    assert.match(primary, /-search"/); assert.match(primary, /-scope"/);
    assert.doesNotMatch(primary, /-(relation|confidence|state|change|node)"/);
    const details = html.match(/<details class="oms-graph-filters"[^>]*>/)[0];
    assert.doesNotMatch(details, /\bopen\b/, 'the filter disclosure is collapsed by default');
    const drawer = html.match(/<details class="oms-graph-filters"[\s\S]*?<\/details>/)[0];
    for (const id of ['relation', 'confidence', 'state', 'change', 'node']) assert.match(drawer, new RegExp(`-${id}"`));
    assert.ok(html.indexOf('class="oms-graph-stage"') < html.indexOf('class="oms-graph-legend'), 'the legend sits below the canvas');
  }
  const view = render(richProject, {width: 768});
  assert.equal(view.get('filters-summary').textContent, 'Filters · 0 active');
  view.get('state').value = 'supported'; view.get('state').fire('change');
  view.get('relation').value = 'calls'; view.get('relation').fire('change');
  assert.equal(view.get('filters-summary').textContent, 'Filters · 2 active');
  assert.equal(view.get('scope-wrap').hidden, false, 'task scope stays above the canvas when active');
  const onlyRunning = render({kind: 'execution', title: 'x', counts: {}, edges: [], nodes: [{id: 'a', label: 'A', kind: 'node', degree: 0, status: 'running', show_label: true}]});
  const shownKeys = onlyRunning.get('legend').children.filter(item => !item.hidden).map(item => item.getAttribute('data-legend'));
  assert.deepEqual(shownKeys, ['running', 'symbol'], 'only encodings present in the nodes are listed');
}
{ // AUD-09: calm empty state.
  for (const data of [model('project', 0), {nodes: [], edges: [], display: {resource_template: true}}, {nodes: null, edges: []}]) {
    const view = render(data);
    assert.equal(view.stage.hidden, true, 'no blank canvas box');
    for (const id of ['search', 'relation', 'confidence', 'state', 'zoom-out', 'reset', 'zoom-in', 'clear', 'copy']) assert.equal(view.get(id).disabled, true, `${id} is disabled when empty`);
    assert.equal(view.get('draft-wrap').hidden, true, 'the empty draft is hidden');
    assert.equal(view.get('selection').hidden, true);
  }
  const empty = render(model('project', 0));
  assert.equal(empty.get('error').textContent, '', 'an empty graph is a status, never an alert');
  assert.equal(empty.get('notice').classList.contains('is-empty'), true);
  assert.match(css, /oms-graph-notice\.is-empty[^}]*dashed/);
  const populated = render(model('project', 3));
  assert.equal(populated.stage.hidden, false); assert.equal(populated.get('search').disabled, false); assert.equal(populated.get('draft-wrap').hidden, false);
}
{ // AUD-15: focus ring outside a halo, selection ring inside it, change as a corner badge rather than a ring.
  const view = render(richProject, {width: 768});
  const f1 = view.nodes[0];
  const r = radiusOf(3);
  assert.equal(Number(f1.querySelector('.oms-graph-focus-ring').attributes.r), r + 10);
  assert.equal(Number(f1.querySelector('.oms-graph-focus-halo').attributes.r), r + 10);
  assert.equal(Number(f1.querySelector('.oms-graph-select-ring').attributes.r), r + 5);
  assert.equal(f1.querySelector('.oms-graph-change-ring'), null, 'no competing change ring');
  assert.equal(f1.querySelector('.oms-graph-change-badge').querySelector('text').textContent, 'Δ');
  assert.equal(view.nodes[1].querySelector('.oms-graph-change-badge').querySelector('text').textContent, '~');
  assert.match(css, /focus-visible \.oms-graph-focus-halo \{ stroke: var\(--oms-bg\)/);
  assert.match(css, /data-change="changed"\] \.oms-graph-change-badge/);
}
{ // Hubs: 90 leaves around one hub never overlap, stay inside the fitted view, and the layout is deterministic.
  const star = {kind: 'project', title: 'Star', counts: {}, nodes: [{id: 'hub', label: 'hub.rs', kind: 'file', degree: 90, show_label: true, assurance: 'supported'},
    ...Array.from({length: 90}, (_, i) => ({id: `leaf${i}`, label: `leaf${i}.rs`, kind: 'file', degree: 1, show_label: false, assurance: 'supported'}))],
    edges: Array.from({length: 90}, (_, i) => ({source: 'hub', target: `leaf${i}`, relation: 'imports', confidence: 'EXTRACTED'}))};
  for (const width of [390, 768, 1280]) {
    const view = render(star, {width});
    const pts = positions(view); const radii = star.nodes.map(n => radiusOf(n.degree));
    for (let i = 0; i < pts.length; i += 1) for (let j = i + 1; j < pts.length; j += 1) {
      assert.ok(Math.hypot(pts[i].x - pts[j].x, pts[i].y - pts[j].y) >= radii[i] + radii[j], `star nodes ${i},${j} overlap at width ${width}`);
    }
    const cam = camera(view); const h = Number(view.svg.attributes.viewBox.split(' ')[3]);
    for (const p of pts) assert.ok(cam.x + p.x * cam.k >= 0 && cam.x + p.x * cam.k <= width && cam.y + p.y * cam.k >= 0 && cam.y + p.y * cam.k <= h, 'fit view contains the settled hub graph');
    assert.deepEqual(positions(render(star, {width})), pts, 'hub layout is deterministic');
  }
}

{ // Round 3: nodes read above edges, fit centres on narrow stages, large ownership fans sit on rings.
  const fan = {kind: 'project', title: 'Fan', counts: {}, nodes: [{id: 'hub', label: 'hub.rs', kind: 'file', degree: 90, show_label: true, assurance: 'supported'},
    ...Array.from({length: 90}, (_, i) => ({id: `leaf${i}`, label: `leaf${i}.rs`, kind: 'file', degree: 1, show_label: false, assurance: 'supported'}))],
    edges: Array.from({length: 90}, (_, i) => ({source: 'hub', target: `leaf${i}`, relation: 'contains', confidence: 'EXTRACTED'}))};
  for (const width of [390, 1280]) {
    const view = render(fan, {width});
    const pts = positions(view); const h = Number(view.svg.attributes.viewBox.split(' ')[3]);
    const cam = camera(view); const radii = fan.nodes.map(n => radiusOf(n.degree));
    const sx = pts.map((p, i) => ({lo: cam.x + (p.x - radii[i]) * cam.k, hi: cam.x + (p.x + radii[i]) * cam.k}));
    const sy = pts.map((p, i) => ({lo: cam.y + (p.y - radii[i]) * cam.k, hi: cam.y + (p.y + radii[i]) * cam.k}));
    const minX = Math.min(...sx.map(v => v.lo)); const maxX = Math.max(...sx.map(v => v.hi));
    const minY = Math.min(...sy.map(v => v.lo)); const maxY = Math.max(...sy.map(v => v.hi));
    assert.ok(minX >= 0 && maxX <= width && minY >= 0 && maxY <= h, `all nodes in bounds at ${width}`);
    assert.ok(Math.abs((minX + maxX) / 2 - width / 2) <= width * .1, `x centred at ${width}`);
    assert.ok(Math.abs((minY + maxY) / 2 - h / 2) <= h * .1, `y centred at ${width}`);
    // Children of the hub (more than 24 ownership children) form concentric rings: few distinct radii, id-ordered angles.
    const hub = pts[0]; const rings = new Map();
    pts.slice(1).forEach((p, i) => { const r = Math.round(Math.hypot(p.x - hub.x, p.y - hub.y) / 2) * 2; rings.set(r, (rings.get(r) || 0) + 1); });
    assert.ok(rings.size <= 6 && rings.size >= 3, `fan uses concentric rings (${[...rings.keys()]}) at ${width}`);
    const edgeEls = view.viewport.children[0].children;
    assert.equal(edgeEls[0].attributes['data-relation-group'], 'ownership');
    const order = view.viewport.children.map(c => c.tagName);
    assert.ok(order.indexOf('G') < order.lastIndexOf('G'), 'edge group precedes node group');
  }
  const bigNode = render(model('project', 3)).nodes.length; assert.equal(bigNode, 3);
}
{ // Fan hubs reserve their ring against the world border: no child is clamped onto PAD and no bodies overlap.
  const files = ['a', 'b'].map(h => ({id: h, label: `${h}.rs`, kind: 'file', degree: 31, show_label: true, assurance: 'supported'}));
  const leaves = files.flatMap(f => Array.from({length: 30}, (_, i) => ({id: `${f.id}${i}`, label: `${f.id}${i}`, kind: 'file', degree: 1, show_label: false, assurance: 'supported'})));
  const two = {kind: 'project', title: 'Fan2', counts: {}, nodes: [...files, ...leaves],
    edges: leaves.map(l => ({source: l.id[0], target: l.id, relation: 'contains', confidence: 'EXTRACTED'}))};
  const clean = (view, nodes, label) => {
    const pts = positions(view); const r = nodes.map(n => radiusOf(n.degree));
    pts.forEach((p, i) => {
      assert.ok(p.x >= 19.99 && p.y >= 19.99, `${label}: node ${i} stays inside PAD (${p.x},${p.y})`);
      for (let j = i + 1; j < pts.length; j += 1) assert.ok(Math.hypot(p.x - pts[j].x, p.y - pts[j].y) >= r[i] + r[j], `${label}: bodies ${i}/${j} do not overlap`);
    });
  };
  clean(render(two, {width: 320}), two.nodes, 'two fans at 320');
  const big = {kind: 'project', title: 'Fan', counts: {}, nodes: [{id: 'hub', label: 'hub.rs', kind: 'file', degree: 90, show_label: true, assurance: 'supported'},
    ...Array.from({length: 90}, (_, i) => ({id: `leaf${i}`, label: `leaf${i}.rs`, kind: 'file', degree: 1, show_label: false, assurance: 'supported'}))],
    edges: Array.from({length: 90}, (_, i) => ({source: 'hub', target: `leaf${i}`, relation: 'contains', confidence: 'EXTRACTED'}))};
  const view = render(big, {width: 320}); dragNode(view, 0, 1, 1, 1); view.flushTimers();
  clean(view, big.nodes, 'dragged hub at 320');
}

// 3. Narrow stage measured synchronously at init; the ResizeObserver never fires.
{
  const view = render(model('project', 100), {initWidth: 370, noObserverFire: true});
  const [, vw, vh] = view.svg.attributes.viewBox.match(/^0 0 (\d+) (\d+)$/);
  assert.equal(Number(vw), 370, 'viewBox width comes from the measured stage');
  const cam = camera(view); assert.ok(cam.k !== 1 || cam.x !== 0 || cam.y !== 0, 'first fit ran');
  assert.ok(cam.k < 1.0001 || cam.k > 1.0001 || cam.x !== 0, 'camera is not the identity default');
  for (const n of view.nodes) {
    const [, x, y] = n.attributes.transform.match(/^translate\(([-\d.]+),([-\d.]+)\)$/);
    const sx = cam.x + Number(x) * cam.k; const sy = cam.y + Number(y) * cam.k;
    assert.ok(sx >= 0 && sx <= 370 && sy >= 0 && sy <= Number(vh), `node inside viewBox (${sx},${sy})`);
  }
}

{ // Round 5: narrow world height ignores node density; nothing waits on animation frames to become visible.
  for (const count of [3, 100, 300]) {
    const view = render(model('project', count), {initWidth: 370, innerWidth: 390, innerHeight: 844, noObserverFire: true});
    const [, vw, vh] = view.svg.attributes.viewBox.match(/^0 0 (\d+) (\d+)$/);
    assert.ok(Number(vw) <= 370, `measured width ${vw} <= stage clientWidth`);
    assert.ok(Number(vh) <= .7 * 844 && Number(vh) >= 320, `narrow world height ${vh} capped regardless of ${count} nodes`);
  }
  const early = render(model('project', 5), {initWidth: 370, noObserverFire: true});
  assert.ok(early.nodes.every(n => !('opacity' in n.attributes) && !/opacity:\s*0\b/.test(n.attributes.style || '')), 'nodes carry no hidden opacity');
  assert.match(css, /@keyframes @@ROOT@@-pop \{ from \{ transform: scale\(\.6\); \} \}/, 'entry keyframe never starts invisible');
}

{ // Round 7: the camera and settle never wait on animation frames.
  for (const [count, w] of [[100, 370], [200, 370], [100, 1280]]) {
    const view = render(model('project', count), {motion: true, noRaf: true, initWidth: w, innerWidth: w + 20, innerHeight: 844, noObserverFire: true});
    const check = label => {
      const c = camera(view);
      const [, vw, vh] = view.svg.attributes.viewBox.match(/^0 0 (\d+) (\d+)$/).map(Number);
      for (const p of positions(view)) {
        const sx = c.x + p.x * c.k; const sy = c.y + p.y * c.k;
        assert.ok(sx >= 0 && sx <= vw && sy >= 0 && sy <= vh, `${label}: node (${sx},${sy}) inside ${vw}x${vh}`);
      }
      return c;
    };
    const first = check('init without frames');
    assert.ok(first.k > 0 && first.k <= 1.25);
    view.flushTimers(); check('after fallback');
    view.get('zoom-in').fire('click');
    const before = camera(view).k; view.flushTimers();
    assert.ok(Math.abs(camera(view).k - Math.min(5, before * 1.35)) < 1e-2 || camera(view).k > before, 'zoom commits without frames');
  }
}

console.log('offline renderer: layouts (320/768/1024), non-color kind/status, keyboard/focus, selection persistence, empty/partial/truncated states and template parity passed');
