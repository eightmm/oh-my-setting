// Run with node --test crates/oms-mcp-server/tests/home-panel.test.mjs.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { createContext, runInContext } from 'node:vm';

const html = readFileSync(new URL('../src/home-panel.html', import.meta.url), 'utf8');
const script = html.match(/<script>([\s\S]+?)<\/script>/)[1];

// Every DOM lookup returns an inert node, so the page script loads outside a browser and its pure logic can be tested.
function inert() {
  const store = {};
  return new Proxy(function () {}, {
    get(_, key) {
      if (key === Symbol.iterator) return function* () {};
      if (key === Symbol.toPrimitive) return () => '';
      if (key === 'then') return undefined;
      if (!(key in store)) store[key] = inert();
      return store[key];
    },
    set(_, key, value) { store[key] = value; return true; },
    apply() { return inert(); },
  });
}

function load() {
  const ctx = {
    document: inert(), navigator: { language: 'en-US' }, console, performance,
    setTimeout: () => 0, clearTimeout() {}, setInterval: () => 0,
    requestAnimationFrame: () => 0, cancelAnimationFrame() {}, addEventListener() {},
    matchMedia: () => ({ matches: true }),
  };
  ctx.window = ctx;
  ctx.parent = ctx;
  createContext(ctx);
  runInContext(script, ctx);
  return ctx.OMSHome;
}
const home = load();
const plain = value => JSON.parse(JSON.stringify(value));

const AREAS = ['chem-ml', 'vision', 'web', null];
function cards(n, offset = 0) {
  return Array.from({ length: n }, (_, k) => {
    const i = k + offset;
    return {
      alias: `project-${i}`, state: ['fresh', 'stale', 'missing', 'unavailable'][i % 4],
      observedAt: '2026-11-27T09:00:00Z', branch: 'main', dirty: i % 2 === 0, goal: 'g', plan: { done: 1, total: 3 },
      attention: i % 3, next: 'n', active: i % 2, main: ['codex', 'claude', 'antigravity'][i % 3],
      crew: [{ provider: 'agy', role: 'builder', id: `${i}`.padStart(12, '0') }], crewTotal: 1 + (i % 2), degraded: false,
      area: AREAS[i % AREAS.length],
    };
  });
}

test('cards come only from structuredContent.cards', () => {
  const list = cards(3);
  assert.equal(home.cardsFrom({ structuredContent: { cards: list } }).length, 3);
  // The text content is a fixed sentence; a catalog without cards means a one-project binding.
  assert.equal(home.cardsFrom({ content: [{ type: 'text', text: JSON.stringify({ cards: list }) }] }), null);
  assert.equal(home.cardsFrom({ structuredContent: { panel: 'OMS home', readOnly: true } }), null);
});

test('normalizes the card contract, including trimmed and withheld cards', () => {
  const [full, trimmed, withheld] = [
    { alias: 'a', state: 'fresh', observedAt: '2026-11-27T09:00:00+00:00', active: 2, main: 'agy', next: 'review',
      crew: [{ provider: 'claude', role: 'security-audit-and-more-words', id: 'abcdef012345' }], crewTotal: 5, degraded: true },
    { alias: 'b', state: 'fresh', trimmed: true },
    { alias: 'c', state: 'withheld' },
  ].map(card => plain(home.norm(card)));
  assert.equal(full.main, 'antigravity');
  assert.equal(full.active, true);
  assert.equal(full.next, 'review');
  assert.equal(full.total, 5);
  assert.equal(full.crew[0].role.length <= 24, true);
  assert.equal(full.degraded, true);
  assert.deepEqual([trimmed.active, trimmed.total, trimmed.crew.length, trimmed.main], [false, 0, 0, null]);
  assert.deepEqual([withheld.state, withheld.total, withheld.crew.length], ['withheld', 0, 0]);
  assert.equal(home.norm({ state: 'fresh' }), null);
  assert.equal(plain(home.norm({ alias: 'd', state: 'bogus' })).state, 'missing');
});

test('the same set of projects draws the same map in any order', () => {
  const list = cards(32).map(home.norm);
  const spots = world => [...world.homes].map(([alias, h]) => `${alias}@${h.i},${h.j}`).sort();
  assert.deepEqual(spots(home.layout(list)), spots(home.layout([...list].reverse())));
  const world = home.layout(list);
  assert.deepEqual(plain(world.islands.map(s => s.name)), ['chem-ml', 'vision', 'web', '']);
  for (const [, h] of world.homes) assert.equal(h.island.name, h.c.area || '');
});

test('adding a project keeps every other island laid out the same', () => {
  const list = cards(32).map(home.norm);
  const added = [...list, home.norm({ ...cards(1, 99)[0], area: 'web' })];
  const local = world => new Map([...world.homes].map(([alias, h]) => [alias, `${h.island.name}:${h.i - h.island.I},${h.j - h.island.J}`]));
  const before = local(home.layout(list)), after = local(home.layout(added));
  for (const [alias, spot] of before) if (!spot.startsWith('web:')) assert.equal(after.get(alias), spot, alias);
});

test('worker names are stable per worker id and distinct within a project', () => {
  const card = home.norm({ alias: 'x', state: 'fresh', crew: ['a', 'b', 'c'].map(n => ({ provider: 'codex', id: n.repeat(12) })), crewTotal: 3 });
  const names = home.crewOf(card).map(w => w.name);
  assert.equal(new Set(names).size, 3);
  assert.deepEqual(home.crewOf(card).map(w => w.name), names);
  const renamed = home.norm({ alias: 'x', state: 'fresh', crew: [{ provider: 'codex', id: 'aaaaaaaaaaaa' }], crewTotal: 1 });
  assert.equal(home.crewOf(renamed)[0].name, names[0]);
});

test('draws a mixed world without throwing', () => {
  assert.doesNotThrow(() => home.show([...cards(40), { alias: 'hidden', state: 'withheld' }].map(home.norm)));
  assert.doesNotThrow(() => home.show([]));
});

test('calls only the read-only card tool and opens no network path', () => {
  assert.ok(Buffer.byteLength(html) <= 128 * 1024);
  assert.deepEqual([...new Set(html.match(/\boms_[a-z_]+/g))], ['oms_list_projects']);
  assert.doesNotMatch(script, /\bfetch\(|XMLHttpRequest|WebSocket|EventSource|\bimport\(/);
});
