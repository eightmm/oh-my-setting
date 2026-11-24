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
    append(...children) { children.forEach(child => { child.parent = this; this.children.push(child); }); }
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
  let rafCalls = 0;
  const context = {document, window: {matchMedia: () => ({matches: !options.motion})}, navigator: {},
    Option: function(text, value) { const option = new Element('option'); option.textContent = text; option.value = value; return option; },
    ResizeObserver: class {
      constructor(callback) { this.callback = callback; resizeCallback = callback; }
      observe() { frames.push(() => this.callback([{contentRect: {width: currentWidth}}])); }
    },
    requestAnimationFrame: callback => { rafCalls += 1; return frames.push(callback); }};
  let clock = 0;
  const drain = () => { let guard = 0; while (frames.length && guard++ < 5000) { clock += 16.7; frames.shift()(clock); } };
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
  return {root, svg, get: suffix => root.querySelector(`#@@ROOT@@-${suffix}`), nodes, viewport, document, resize, drain,
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
  assert.ok(vbH >= minHeight, `200-node height ${vbH} >= ${minHeight} at width ${w}`);
  const pts = view.nodes.map((n, idx) => {
    const [, x, y] = n.attributes.transform.match(/^translate\(([\d.]+),([\d.]+)\)$/);
    const r = Math.max(9, Math.min(14, 8 + Math.sqrt(denseData.nodes[idx].degree || 0)));
    assert.ok(Number(x) >= 18 && Number(x) <= vbW - 18, `200-node x=${x} within width ${vbW}`);
    assert.ok(Number(y) >= 18 && Number(y) <= vbH - 18, `200-node y=${y} within height ${vbH}`);
    return {x: Number(x), y: Number(y), r};
  });
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
assert.deepEqual(execGlyphs.slice(0, 6), ['▶', '✕', '○', '✓', '⏸', '⏸']);
assert.notEqual(execGlyphs[6], execGlyphs[2], 'ready must have a distinct glyph from pending');
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
assert.equal(invalid.svg.hidden, true);

const templatePlaceholder = render({nodes: [], edges: [], display: {resource_template: true}});
assert.equal(templatePlaceholder.get('error').hidden, false);
assert.match(templatePlaceholder.get('error').textContent, /No graph data/);

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
  for (const p of pts) {
    const sx = cam.x + p.x * cam.k; const sy = cam.y + p.y * cam.k;
    assert.ok(sx >= 0 && sx <= 736 && sy >= 0 && sy <= 520, `${kind} fit-to-view keeps node inside stage (${sx},${sy})`);
  }
  // Selecting a node moves/zooms the view; Reset and Escape return to fit-all.
  const before = camera(first);
  first.nodes[5].fire('click');
  assert.notDeepEqual(camera(first), before, 'click must focus the view on the node');
  const focused = camera(first); const row5 = pts[5];
  assert.ok(Math.abs(focused.x + row5.x * focused.k - 368) < 1 && Math.abs(focused.y + row5.y * focused.k - 260) < 1, 'selected node is centred');
  first.nodes[5].fire('keydown', {key: 'Escape'});
  assert.deepEqual(camera(first), before, 'Esc returns to fit-all');
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
  svgPts.fire('dblclick', {target: first.nodes[0]});
  assert.equal(first.nodes[0].attributes['data-pinned'], 'false');
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
  view.nodes[2].fire('click'); view.drain();
  assert.notDeepEqual(camera(view), settled);
  assert.deepEqual(positions(view), startPositions, 'camera moves do not move nodes');
}

console.log('offline renderer: layouts (320/768/1024), non-color kind/status, keyboard/focus, selection persistence, empty/partial/truncated states and template parity passed');
