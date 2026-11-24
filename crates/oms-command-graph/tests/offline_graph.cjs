// Exercise the actual shared renderer with no CDN or browser dependencies.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const template = fs.readFileSync(path.join(__dirname, '../src/graph.html'), 'utf8');
const mcp = fs.readFileSync(path.join(__dirname, '../../oms-mcp-server/src/project-graph.html'), 'utf8');
const script = html => html.match(/\(\(\) => \{[\s\S]*?\n\}\)\(\);/)[0];
const style = html => html.match(/<style>([\s\S]*?)<\/style>/)[1];
assert.equal(script(mcp).replaceAll('oms-graph-view-19ff9c65bfcf', '@@ROOT@@'), script(template));
assert.equal(style(mcp).replaceAll('oms-graph-view-19ff9c65bfcf', '@@ROOT@@'), style(template));

// No new external dependencies, CDNs, network calls, or frameworks.
for (const [name, html] of [['graph.html', template], ['project-graph.html', mcp]]) {
  const externalScripts = [...html.matchAll(/<script\b[^>]*\bsrc="([^"]+)"/g)].map(m => m[1]);
  assert.deepEqual(externalScripts, ['https://cdn.jsdelivr.net/npm/d3@7.9.0/dist/d3.min.js'], `${name} external scripts`);
  assert.doesNotMatch(html, /\b(fetch|XMLHttpRequest|@import)\b/, `${name} must not make network calls`);
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
  for (const [cls, attr, val, label] of [
    ['oms-project-key', 'data-assurance', 'supported', 'Test-linked'],
    ['oms-project-key', 'data-assurance', 'needs-evidence', 'Needs evidence'],
    ['oms-project-key', 'data-assurance', 'attention', 'Attention'],
    ['oms-project-key', 'data-assurance', 'test-evidence', 'Test evidence'],
    ['oms-exec-key', 'data-status', 'finished', 'Finished'],
    ['oms-exec-key', 'data-status', 'active', 'Active'],
    ['oms-change-key', 'data-change', 'changed', 'Changed'],
    ['oms-change-key', 'data-change', 'impacted', 'Impacted'],
    ['oms-kind-key', 'data-kind-key', 'file', 'File / module'],
    ['oms-kind-key', 'data-kind-key', 'symbol', 'Symbol / func'],
    ['oms-kind-key', 'data-kind-key', 'test', 'Test / gate'],
    ['oms-kind-key', 'data-kind-key', 'agent', 'Agent'],
    ['oms-kind-key', 'data-kind-key', 'terminal', 'Terminal']
  ]) {
    const item = new Element('span');
    item.setAttribute('class', cls);
    if (attr === 'data-kind-key') item.setAttribute('data-kind-key', val);
    item.textContent = label;
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
  const context = {document, window: {matchMedia: () => ({matches: true})}, navigator: {},
    Option: function(text, value) { const option = new Element('option'); option.textContent = text; option.value = value; return option; },
    ResizeObserver: class {
      constructor(callback) { this.callback = callback; resizeCallback = callback; }
      observe() { frames.push(() => this.callback([{contentRect: {width: currentWidth}}])); }
    },
    requestAnimationFrame: callback => frames.push(callback)};
  vm.runInNewContext(script(template), context);
  while (frames.length) frames.shift()();
  const viewport = svg.children.find(child => child.tagName === 'G');
  const nodes = viewport?.children.filter(child => child.tagName === 'G').flatMap(group => group.children).filter(child => child.tagName === 'G') || [];
  const resize = nextWidth => {
    currentWidth = nextWidth;
    if (resizeCallback) resizeCallback([{contentRect: {width: nextWidth}}]);
    while (frames.length) frames.shift()();
  };
  return {root, svg, get: suffix => root.querySelector(`#@@ROOT@@-${suffix}`), nodes, viewport, document, resize};
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
  view.get('zoom-in').fire('click'); assert.match(view.viewport.attributes.transform, /scale\(1.35\)/);
  view.get('zoom-out').fire('click'); assert.match(view.viewport.attributes.transform, /scale\(1\)/);
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
  view.get('reset').fire('click'); assert.match(view.viewport.attributes.transform, /scale\(1\)/);
  assert.equal(view.get('search').value, '');
}

// 2. Narrow / medium / wide responsive widths (320 / 768 / 1024 CSS px).
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
  counts: {shown_nodes: 4, total_nodes: 4, shown_edges: 2, total_edges: 2},
  nodes: [
    {id: 'e1', label: 'Plan', kind: 'agent', degree: 1, focus: true, show_label: true, status: 'active'},
    {id: 'e2', label: 'Gate', kind: 'gate', degree: 2, focus: false, show_label: true, status: 'failed'},
    {id: 'e3', label: 'Verify', kind: 'node', degree: 1, focus: false, show_label: true, status: 'pending'},
    {id: 'e4', label: 'Finish', kind: 'terminal', degree: 1, focus: false, show_label: true, status: 'finished'}
  ],
  edges: [
    {source: 'e1', target: 'e2', relation: 'next', confidence: 'NORMAL'},
    {source: 'e2', target: 'e4', relation: 'pass', confidence: 'NORMAL'}
  ]
};
const execView = render(richExec, {width: 768});
assert.equal(execView.get('legend').hidden, false, 'execution graph must also show status/kind legend');
assert.deepEqual(execView.nodes.map(n => n.querySelector('.oms-graph-glyph').textContent), ['▶', '✕', '○', '✓']);

// 4. Keyboard navigation, selection persistence across search/filter/zoom/resize, Escape & clear focus restoration.
assert.equal(richView.nodes[0].attributes.role, 'button');
assert.equal(richView.nodes[0].attributes.tabindex, '0');
assert.equal(richView.nodes[0].attributes['aria-pressed'], 'true');
assert.match(richView.nodes[0].attributes['aria-label'], /main\.rs.*file.*Test-linked/);
// Out-of-scope node (distance 3 with default_scope='neighbors') is excluded from tab order.
assert.equal(richView.nodes[4].attributes.tabindex, '-1');

// Select fn1 via keyboard Enter without stealing focus to draft textarea.
richView.nodes[1].focus();
richView.nodes[1].fire('keydown', {key: 'Enter'});
assert.equal(richView.get('node').value, 'fn1');
assert.equal(richView.nodes[1].attributes['aria-pressed'], 'true');
assert.equal(richView.document.activeElement, richView.nodes[1], 'keyboard node selection must not steal focus to draft');

// ArrowRight moves focus to next visible node.
richView.nodes[1].fire('keydown', {key: 'ArrowRight'});
assert.equal(richView.document.activeElement, richView.nodes[2]);

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

console.log('offline renderer: layouts (320/768/1024), non-color kind/status, keyboard/focus, selection persistence, empty/partial/truncated states and template parity passed');
