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
  assert.equal(plain(home.norm({ alias: 'e', state: 'withheld', area: 'lab' })).area, null);
  assert.equal(home.norm({ state: 'fresh' }), null);
  assert.equal(plain(home.norm({ alias: 'd', state: 'bogus' })).state, 'missing');
});

test('groups projects by triage in any input order', () => {
  const list = cards(32).map(home.norm);
  const keys = g => Object.fromEntries(Object.entries(g).map(([k, v]) => [k, v.map(c => c.alias)]));
  const grouped = keys(home.groups(list));
  assert.deepEqual(grouped, keys(home.groups([...list].reverse())));
  assert.deepEqual(Object.keys(grouped), ['need', 'busy', 'calm', 'quiet', 'gone']);
  const by = new Map(list.map(c => [c.alias, c]));
  // Anything that needs you leads, most items first; unreachable projects never hide an attention count.
  const need = grouped.need.map(a => by.get(a));
  assert.ok(need.length > 0 && need.every(c => c.attention > 0));
  for (let i = 1; i < need.length; i++) assert.ok(need[i - 1].attention >= need[i].attention);
  assert.ok(grouped.gone.map(a => by.get(a)).every(c => ['missing', 'unavailable'].includes(c.state) && !c.attention));
  assert.ok(grouped.quiet.map(a => by.get(a)).every(c => c.state === 'stale'));
  assert.ok(grouped.busy.map(a => by.get(a)).every(c => c.active && c.state === 'fresh'));
  assert.equal(Object.values(grouped).flat().length, list.length);
  assert.deepEqual(plain(home.groups([home.norm({ alias: 'h', state: 'withheld' })]).gone.map(c => c.alias)), ['h']);
});

test('labels say why a project sits in its band and hide withheld details', () => {
  home.setLang('en');
  const need = home.norm({ alias: 'n', state: 'fresh', attention: 3, main: 'claude', crewTotal: 2, crew: [{ provider: 'codex' }] });
  assert.match(home.label(need), /^n, Needs you, fresh, 3 to check, Main claude, Workers 2/);
  assert.equal(home.label(home.norm({ alias: 'w', state: 'withheld', goal: 'secret' })), 'w, Out of reach, withheld');
});

test('draws a mixed roster without throwing', () => {
  assert.doesNotThrow(() => home.show([...cards(40), { alias: 'hidden', state: 'withheld' }].map(home.norm)));
  assert.doesNotThrow(() => home.show([]));
  // Opening a row, a quiet tag and a withheld tag builds each detail, including the withheld note.
  const roster = [...cards(10), { alias: 'hidden', state: 'withheld' }].map(home.norm);
  home.show(roster);
  const band = plain(home.groups(roster));
  // project-1 is a need row, project-9 a quiet tag (stale, nothing to check), project-3 an unreachable tag.
  assert.ok(band.need.some(c => c.alias === 'project-1') && band.quiet.some(c => c.alias === 'project-9') && band.gone.some(c => c.alias === 'project-3'));
  for (const alias of ['project-1', 'project-9', 'project-3', 'hidden', null]) assert.doesNotThrow(() => home.select(alias, true));
});

test('calls only the read-only card tool and opens no network path', () => {
  assert.ok(Buffer.byteLength(html) <= 256 * 1024);
  assert.deepEqual([...new Set(html.match(/\boms_[a-z_]+/g))], ['oms_list_projects']);
  assert.doesNotMatch(script, /\bfetch\(|XMLHttpRequest|WebSocket|EventSource|\bimport\(/);
});

test('shows stale on need rows with branch, broadens search safely without leaking withheld fields, and avoids village wording', () => {
  assert.doesNotMatch(html, /동네|평온|조용함|폴더 없음|folder missing|worker가|일하는 worker/);
  assert.equal(typeof home.matches, 'function');
  const staleNeed = home.norm({ alias: 'pay', state: 'stale', attention: 2, branch: 'feat/x', goal: 'Fix webhook', next: 'Approve', area: 'backend', main: 'claude', crewTotal: 3 });
  assert.equal(home.matches(staleNeed, 'feat/x'), true);
  assert.equal(home.matches(staleNeed, 'webhook'), true);
  assert.equal(home.matches(staleNeed, 'approve'), true);
  assert.equal(home.matches(staleNeed, 'backend'), true);
  assert.equal(home.matches(staleNeed, 'claude'), true);
  assert.equal(home.matches(staleNeed, 'missing-term'), false);
  const withheldRaw = { alias: 'secret-repo', state: 'withheld', branch: 'top-secret-branch', goal: 'secret-goal', next: 'secret-next', area: 'secret-area', main: 'codex' };
  const withheldNorm = home.norm(withheldRaw);
  for (const term of ['top-secret-branch', 'secret-goal', 'secret-next', 'secret-area', 'codex']) {
    assert.equal(home.matches(withheldNorm, term), false);
    assert.equal(home.matches(withheldRaw, term), false);
  }
  assert.equal(home.matches(withheldNorm, 'secret-repo'), true);
  assert.match(script, /c\.state==='stale'\?L\.st\.stale:null/);
  assert.match(script, /wbadge/);
  // Escape clears focused #find before clearing picked selection; HUD shows attention & active worker totals.
  assert.match(script, /attnTotal/);
  assert.match(script, /workersTotal/);
  assert.match(script, /document\.activeElement===el\('find'\)&&query/);
  home.setLang('en', true);
  home.setLang('ko', true);
});
