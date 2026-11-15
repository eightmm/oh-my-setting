// Exercise the actual shared renderer with no CDN or browser dependencies.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const template = fs.readFileSync(path.join(__dirname, '../src/graph.html'), 'utf8');
const mcp = fs.readFileSync(path.join(__dirname, '../../oms-mcp-server/src/project-graph.html'), 'utf8');
const script = html => html.match(/\(\(\) => \{[\s\S]*?\n\}\)\(\);/)[0];
assert.equal(script(mcp).replaceAll('oms-graph-view-19ff9c65bfcf', '@@ROOT@@'), script(template));

function render(data) {
  const ids = new Map();
  let document;
  class Element {
    constructor(tag = 'div') {
      this.tagName = tag.toUpperCase(); this.children = []; this.attributes = {}; this.events = {};
      this.value = ''; this.textContent = ''; this.hidden = false;
      const classes = new Set();
      this.classList = {toggle(name, enabled) { if (enabled) classes.add(name); else classes.delete(name); }, contains: name => classes.has(name)};
    }
    append(...children) { children.forEach(child => { child.parent = this; this.children.push(child); }); }
    replaceChildren(...children) { this.children = []; this.append(...children); }
    setAttribute(name, value) {
      this.attributes[name] = String(value);
      if (name === 'class') String(value).split(' ').forEach(c => this.classList.toggle(c, true));
    }
    querySelector(selector) {
      if (selector.startsWith('#')) {
        if (!ids.has(selector)) ids.set(selector, new Element(selector.endsWith('-svg') ? 'svg' : 'div'));
        return ids.get(selector);
      }
      if (selector === '.oms-graph-stage') return stage;
      return this.children.find(child => child.tagName.toLowerCase() === selector) || null;
    }
    querySelectorAll() { return []; }
    addEventListener(name, handler) { (this.events[name] ||= []).push(handler); }
    fire(name, event = {}) { for (const handler of this.events[name] || []) handler({target: this, preventDefault() {}, ...event}); }
    get options() { return this.children; }
    focus() { document.activeElement = this; }
    blur() { document.activeElement = null; }
    setSelectionRange() {}
    getBoundingClientRect() { return {width: 736, height: 520}; }
    closest(selector) { return this.classList.contains(selector.slice(1)) ? this : this.parent?.closest(selector); }
  }
  const root = new Element(); const stage = new Element(); const svg = root.querySelector('#@@ROOT@@-svg');
  svg.append(new Element('title'));
  root.querySelector('#@@ROOT@@-data').textContent = JSON.stringify(data);
  document = new Element();
  document.getElementById = () => root;
  document.createElementNS = (_namespace, tag) => new Element(tag);
  document.createElement = tag => new Element(tag);
  document.createTextNode = text => ({textContent: text});
  const frames = [];
  const context = {document, window: {matchMedia: () => ({matches: true})}, navigator: {},
    Option: function(text, value) { const option = new Element('option'); option.textContent = text; option.value = value; return option; },
    ResizeObserver: class { constructor(callback) { this.callback = callback; } observe() { frames.push(() => this.callback([{contentRect: {width: 736}}])); } },
    requestAnimationFrame: callback => frames.push(callback)};
  vm.runInNewContext(script(template), context);
  while (frames.length) frames.shift()();
  const viewport = svg.children.find(child => child.tagName === 'G');
  const nodes = viewport?.children.filter(child => child.tagName === 'G').flatMap(group => group.children).filter(child => child.tagName === 'G') || [];
  return {get: suffix => root.querySelector(`#@@ROOT@@-${suffix}`), nodes, viewport, document};
}
function model(kind, count) {
  return {kind, title: `OMS ${kind === 'project' ? 'Project' : 'Execution'} Graph`, counts: {},
    nodes: Array.from({length: count}, (_, i) => ({id: `n${i}`,label: `Node ${i}`,path: `src/${i}.sh`,kind: 'file',show_label: i === 0,degree: 1,assurance: 'supported',status: 'done'})),
    edges: count > 1 ? [{source: 'n0',target: 'n1',relation: 'calls',confidence: 'REPEAT'}] : []};
}
for (const kind of ['project', 'execution']) for (const count of [0, 1, 30]) {
  const data = model(kind, count); const view = render(data);
  assert.equal(view.nodes.length, count);
  assert.equal(view.get('error').textContent, '');
  if (!count) continue;
  const positions = view.nodes.map(node => node.attributes.transform);
  assert.ok(positions.every(position => /^translate\([\d.]+,[\d.]+\)$/.test(position)));
  assert.deepEqual(render(data).nodes.map(node => node.attributes.transform), positions);
  view.get('zoom-in').fire('click'); assert.match(view.viewport.attributes.transform, /scale\(1.35\)/);
  view.get('zoom-out').fire('click'); assert.match(view.viewport.attributes.transform, /scale\(1\)/);
  view.get('search').value = 'missing'; view.get('search').fire('input');
  assert.ok(view.nodes.slice(1).every(node => node.classList.contains('is-muted')));
  view.get('search').focus(); view.document.fire('keydown', {key: 'Escape', target: view.get('search')});
  assert.equal(view.get('search').value, '');
  if (count > 1) {
    view.get('confidence').value = 'EXTRACTED'; view.get('confidence').fire('change');
    assert.ok(view.viewport.children[0].children[0].classList.contains('is-muted'));
    view.nodes[1].fire('click'); assert.match(view.get('detail').textContent, /n1/);
    assert.ok(view.get('draft').value.includes('n1'));
  }
  view.get('reset').fire('click'); assert.match(view.viewport.attributes.transform, /scale\(1\)/);
  assert.equal(view.get('search').value, '');
}
const invalid = render({nodes: null, edges: []});
assert.match(invalid.get('error').textContent, /No graph data/);
console.log('offline renderer: 6 deterministic layouts; search, filters, zoom/reset, inspection and template parity passed');
