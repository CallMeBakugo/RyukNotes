import assert from 'node:assert/strict';
import test from 'node:test';
import { exactDocument, exactDiff, planMerge, constructCandidate, ExactResult, MergeLimitError } from '../src/lib/utils/three-way-merge.ts';
const choose = (plan, side) => Object.fromEntries(plan.hunks.filter(h => h.kind === 'overlapping').map(h => [h.id, side]));
const bytes = s => Buffer.byteLength(s, 'utf8');
function checkMapping(plan, candidate) {
  let end = 0, byteEnd = 0;
  for (const segment of candidate.mapping) {
    assert.equal(segment.result.from, end); assert.equal(segment.result.byteFrom, byteEnd);
    const source = plan[segment.source].text.slice(segment.sourceSpan.from, segment.sourceSpan.to);
    assert.equal(candidate.text.slice(segment.result.from, segment.result.to), source);
    assert.equal(segment.result.byteTo - segment.result.byteFrom, bytes(source));
    assert.equal(segment.sourceSpan.byteFrom, bytes(plan[segment.source].text.slice(0, segment.sourceSpan.from)));
    assert.equal(segment.sourceSpan.byteTo, bytes(plan[segment.source].text.slice(0, segment.sourceSpan.to)));
    end = segment.result.to; byteEnd = segment.result.byteTo;
  }
  assert.equal(end, candidate.text.length); assert.equal(byteEnd, bytes(candidate.text));
}
function replay(base, patches) {
  let value = base;
  for (const p of [...patches].reverse()) value = value.slice(0, p.base.from) + p.insert + value.slice(p.base.to);
  return value;
}
test('exact changes combine only as a user-constructed artifact, with byte/line mappings', () => {
  const plan = planMerge('a\r\nβ\r\nz\r\n', 'local\r\nβ\r\nz\r\n', 'a\r\nβ\r\n遠\r\n');
  assert.deepEqual(plan.hunks.map(h => h.kind), ['clean-local', 'clean-remote']);
  const result = constructCandidate(plan, {});
  assert.equal(result.text, 'local\r\nβ\r\n遠\r\n');
  checkMapping(plan, result);
});
test('same-position insertions and identical concurrent changes always require a choice', () => {
  for (const remote of ['REMOTE\nanchor\n', 'LOCAL\nanchor\n']) {
    const plan = planMerge('anchor\n', 'LOCAL\nanchor\n', remote);
    assert.equal(plan.hunks[0].kind, 'overlapping');
    assert.throws(() => constructCandidate(plan, {}), /Choose/);
    assert.equal(constructCandidate(plan, choose(plan, 'local')).text, 'LOCAL\nanchor\n');
    assert.equal(constructCandidate(plan, choose(plan, 'remote')).text, remote);
  }
});
for (const [name, base, local, remote] of [
  ['lists and tasks', '- [ ] task\n  continuation\n', '- [x] task\n  continuation\n', '- [ ] task\n  changed\n'],
  ['tables', '| A | B |\n|---|---|\n| 1 | 2 |\n', '| C | B |\n|---|---|\n| 1 | 2 |\n', '| A | B |\n|---|---|\n| 3 | 2 |\n'],
  ['reference definitions', '[ref]: /old\n\nprose\n', '[ref]: /new\n\nprose\n', '[ref]: /old\n\nchanged\n'],
  ['fenced code', '```js\none\ntwo\n```\n', '```js\nlocal\ntwo\n```\n', '```js\none\nremote\n```\n'],
  ['math', '$$\nx\ny\n$$\n', '$$\nlocal\ny\n$$\n', '$$\nx\nremote\n$$\n'],
  ['secrets', '```ryuk-secret\none\ntwo\n```\n', '```ryuk-secret\nlocal\ntwo\n```\n', '```ryuk-secret\none\nremote\n```\n'],
  ['legacy secrets', '```helix-secret\none\ntwo\n```\n', '```helix-secret\nlocal\ntwo\n```\n', '```helix-secret\none\nremote\n```\n'],
  ['raw HTML', '<div>\none\ntwo\n</div>\n', '<div>\nlocal\ntwo\n</div>\n', '<div>\none\nremote\n</div>\n'],
  ['wiki targets', '[[target]]\nprose\n', '[[new]]\nprose\n', '[[target]]\nchanged\n'],
  ['attachment references', '[file](.rn-attachments/id)\nprose\n', '[new](.rn-attachments/other)\nprose\n', '[file](.rn-attachments/id)\nchanged\n'],
]) test(`protected boundary: ${name} is explicit, never inferred clean`, () => {
  const plan = planMerge(base, local, remote);
  assert.ok(plan.hunks.some(h => h.kind === 'overlapping'));
  assert.throws(() => constructCandidate(plan, {}));
  checkMapping(plan, constructCandidate(plan, choose(plan, 'local')));
  assert.equal(constructCandidate(plan, choose(plan, 'local')).text, local);
  assert.equal(constructCandidate(plan, choose(plan, 'remote')).text, remote);
});
test('moves, repeated lines and structural delimiter edits refuse ambiguous alignment', () => {
  for (const [b, l, r] of [
    ['a\nx\na\ny\n', 'a\nLOCAL\na\ny\n', 'a\nx\na\nREMOTE\n'],
    ['one\ntwo\nthree\n', 'two\none\nthree\n', 'one\ntwo\nchanged\n'],
    ['plain\n\nother\n', '```\nplain\n\nother\n', 'plain\n\nchanged\n'],
  ]) {
    const plan = planMerge(b, l, r);
    assert.ok(plan.hunks.every(h => h.kind === 'overlapping'));
    assert.equal(constructCandidate(plan, choose(plan, 'local')).text, l);
    assert.equal(constructCandidate(plan, choose(plan, 'remote')).text, r);
  }
});
test('whitespace, CRLF, lone CR, missing final newline and Unicode are never normalized', () => {
  for (const value of ['', '\n', '\r\n', ' \t\r\n\rX', 'e\u0301🙂\r\n終', '\ufeffnote\r\n', '---\r\nnot metadata\r\n---']) {
    const doc = exactDocument(value);
    assert.equal(doc.lines.map(l => l.text).join(''), value);
    assert.equal(doc.bytes, bytes(value));
    const plan = planMerge(value, value + ' \t', value);
    const candidate = constructCandidate(plan, choose(plan, 'local'));
    assert.equal(candidate.text, value + ' \t'); checkMapping(plan, candidate);
  }
});
test('deterministic property corpus: exact diff replay, identity and one-sided reconstruction', () => {
  let seed = 0x12345678;
  const next = () => { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return seed; };
  const tokens = ['word', ' ', '\t', '🙂', 'e\u0301', '\r\n', '\n', '\r', '重复', '|'];
  for (let i = 0; i < 500; i++) {
    const generate = () => Array.from({ length: next() % 24 }, () => tokens[next() % tokens.length]).join('');
    const b = generate(), l = generate(), r = generate();
    const bd = exactDocument(b), ld = exactDocument(l);
    assert.equal(replay(b, exactDiff(bd, ld, 'local')), l);
    for (const [local, remote, expected, side] of [[l, b, l, 'local'], [b, r, r, 'remote'], [b, b, b, 'base']]) {
      const plan = planMerge(b, local, remote);
      const candidate = constructCandidate(plan, choose(plan, side));
      assert.equal(candidate.text, expected); checkMapping(plan, candidate);
    }
  }
});
test('result controller mapping preserves untouched mixed EOL and uses stated terminators for edits', () => {
  const result = new ExactResult('🙂\r\nsecond\nlast\r');
  assert.equal(result.display, '🙂\nsecond\nlast\n');
  result.edit([{ from: 3, to: 9, insert: 'new\nextra' }]);
  assert.equal(result.raw, '🙂\r\nnew\r\nextra\nlast\r');
  assert.throws(() => new ExactResult('\ud800'), /Unicode/);
  assert.throws(() => exactDocument('x'.repeat(2_000_001)), MergeLimitError);
  assert.throws(() => exactDiff(exactDocument(('a\n').repeat(1100)), exactDocument(('b\n').repeat(1100)), 'local'), MergeLimitError);
});

test('HTML blocks spanning blank lines and edited moves are uncertain whole boundaries', () => {
  for (const [b,l,r] of [
    ['<div>\n\nfirst\n\nlast\n\n</div>\n','<div>\n\nLOCAL\n\nlast\n\n</div>\n','<div>\n\nfirst\n\nREMOTE\n\n</div>\n'],
    ['A\nB\nC\n','B2\nA\nC\n','A\nB\nC2\n'],
  ]) {
    const p=planMerge(b,l,r);assert.ok(p.hunks.every(h=>h.kind==='overlapping'));
    assert.equal(constructCandidate(p,choose(p,'local')).text,l);
    assert.equal(constructCandidate(p,choose(p,'remote')).text,r);
  }
});

test('new protected syntax projects its whole target block back to the base before classifying other edits', () => {
  const base='first\n\ninside\n\nlast\n';
  const local='<div>\n\ninside\n\n</div>\n';
  const remote='first\n\nREMOTE\n\nlast\n';
  const plan=planMerge(base,local,remote);
  assert.ok(plan.hunks.every(h=>h.kind==='overlapping'));
  assert.equal(constructCandidate(plan,choose(plan,'local')).text,local);
  assert.equal(constructCandidate(plan,choose(plan,'remote')).text,remote);
});

test('property corpus: independent unique prose edits retain both sides with exact mixed line/byte ranges', () => {
  for(let seed=0;seed<300;seed++) {
    const count=4+seed%9, eols=['\n','\r\n','\r'];
    const base=Array.from({length:count},(_,i)=>`unique ${seed}:${i} 🙂${eols[(seed+i)%3]}`);
    const p=seed%count,q=(p+1+seed%(count-1))%count;
    const local=[...base],remote=[...base],expected=[...base];
    local[p]=`local ${seed} Ω${eols[(seed+p)%3]}`;remote[q]=`remote ${seed} 終${eols[(seed+q)%3]}`;
    expected[p]=local[p];expected[q]=remote[q];
    const plan=planMerge(base.join(''),local.join(''),remote.join(''));
    assert.ok(plan.hunks.every(h=>h.kind!=='overlapping'));
    const candidate=constructCandidate(plan,{});assert.equal(candidate.text,expected.join(''));checkMapping(plan,candidate);
  }
});

test('reference definitions remain structural with lone-CR line terminators', () => {
  const base='intro\r[ref]: /old\r\rfar prose\r';
  const local='INTRO\r[ref]: /new\r\rfar prose\r';
  const remote='intro\r[ref]: /old\r\rchanged far prose\r';
  const plan=planMerge(base,local,remote);
  assert.ok(plan.hunks.every(h=>h.kind==='overlapping'));
  assert.equal(constructCandidate(plan,choose(plan,'local')).text,local);
  assert.equal(constructCandidate(plan,choose(plan,'remote')).text,remote);
});
