import assert from 'node:assert/strict';
import test from 'node:test';
import { comparableBody, compareHistory, splitSnapshot } from './history-comparison.ts';

function rowsOf(comparison) {
  return comparison.chunks.flatMap((chunk) => chunk.rows);
}

async function compare(current, snapshot) {
  const comparison = await compareHistory(current, snapshot);
  const rows = rowsOf(comparison);
  const normalize = (body) => body.replace(/\r\n/g, '\n');
  const restoreText = (excludedKind) => rows
    .filter((row) => row.kind !== excludedKind)
    .map((row) => row.text + (row.newline ? '\n' : ''))
    .join('');

  assert.equal(restoreText('added'), normalize(current), 'all current content is retained');
  assert.equal(restoreText('removed'), normalize(snapshot), 'all snapshot content is retained');
  assert.equal(comparison.added, rows.filter((row) => row.kind === 'added').length);
  assert.equal(comparison.removed, rows.filter((row) => row.kind === 'removed').length);

  let currentLine = 1;
  let snapshotLine = 1;
  for (const row of rows) {
    assert.equal(row.currentLine, row.kind === 'added' ? null : currentLine++);
    assert.equal(row.snapshotLine, row.kind === 'removed' ? null : snapshotLine++);
    assert.equal(typeof row.newline, 'boolean');
    assert.equal(row.text.includes('\n'), false);
  }
  for (let index = 0; index < comparison.chunks.length; index++) {
    const chunk = comparison.chunks[index];
    assert.ok(chunk.rows.length > 0, 'empty chunks are not displayed');
    assert.notEqual(chunk.kind, comparison.chunks[index - 1]?.kind, 'adjacent chunks are merged');
    if (chunk.kind === 'unchanged') {
      assert.ok(chunk.rows.every((row) => row.kind === 'same'));
    }
  }
  return comparison;
}

function lines(count) {
  return Array.from({ length: count }, (_, index) => `line ${index + 1}`);
}

function document(lines) {
  return lines.join('\n') + '\n';
}

test('restoring a snapshot adds snapshot-only lines and removes current-only lines', async () => {
  const comparison = await compare('keep\nlive\n', 'keep\narchived\nextra\n');
  assert.equal(comparison.added, 2);
  assert.equal(comparison.removed, 1);
  assert.deepEqual(rowsOf(comparison).map((row) => [row.kind, row.text]), [
    ['same', 'keep'],
    ['removed', 'live'],
    ['added', 'archived'],
    ['added', 'extra'],
  ]);
});

test('insertion keeps both line-number columns aligned with their own documents', async () => {
  const comparison = await compare('first\nlast', 'first\ninserted\nlast');
  assert.deepEqual(rowsOf(comparison), [
    { kind: 'same', text: 'first', currentLine: 1, snapshotLine: 1, newline: true },
    { kind: 'added', text: 'inserted', currentLine: null, snapshotLine: 2, newline: true },
    { kind: 'same', text: 'last', currentLine: 2, snapshotLine: 3, newline: false },
  ]);
});

test('deletion leaves snapshot line numbers unchanged by removed rows', async () => {
  const comparison = await compare('first\ndeleted\nlast', 'first\nlast');
  assert.deepEqual(rowsOf(comparison), [
    { kind: 'same', text: 'first', currentLine: 1, snapshotLine: 1, newline: true },
    { kind: 'removed', text: 'deleted', currentLine: 2, snapshotLine: null, newline: true },
    { kind: 'same', text: 'last', currentLine: 3, snapshotLine: 2, newline: false },
  ]);
});

test('replacement presents the current line before its snapshot replacement', async () => {
  const comparison = await compare('first\nlive\nlast\n', 'first\narchived\nlast\n');
  assert.deepEqual(rowsOf(comparison), [
    { kind: 'same', text: 'first', currentLine: 1, snapshotLine: 1, newline: true },
    { kind: 'removed', text: 'live', currentLine: 2, snapshotLine: null, newline: true },
    { kind: 'added', text: 'archived', currentLine: null, snapshotLine: 2, newline: true },
    { kind: 'same', text: 'last', currentLine: 3, snapshotLine: 3, newline: true },
  ]);
});

test('empty bodies and blank lines remain distinct without phantom trailing rows', async () => {
  const cases = [
    ['', '', 0, 0],
    ['', '\n', 1, 0],
    ['\n', '', 0, 1],
    ['', '\n\n', 2, 0],
    ['\n', '\n', 0, 0],
    ['\n\n', '\n', 0, 1],
    ['', 'text', 1, 0],
    ['text', '', 0, 1],
  ];
  for (const [current, snapshot, added, removed] of cases) {
    const comparison = await compare(current, snapshot);
    assert.equal(comparison.added, added);
    assert.equal(comparison.removed, removed);
  }
  assert.deepEqual((await compare('', '')).chunks, []);
  assert.deepEqual(rowsOf(await compare('', '\n')), [
    { kind: 'added', text: '', currentLine: null, snapshotLine: 1, newline: true },
  ]);
});

test('adding a final newline is visible even when the line text is identical', async () => {
  const comparison = await compare('last', 'last\n');
  assert.equal(comparison.added, 1);
  assert.equal(comparison.removed, 1);
  assert.deepEqual(rowsOf(comparison), [
    { kind: 'removed', text: 'last', currentLine: 1, snapshotLine: null, newline: false },
    { kind: 'added', text: 'last', currentLine: null, snapshotLine: 1, newline: true },
  ]);
});

test('removing a final newline retains the visible missing-newline flag', async () => {
  const comparison = await compare('last\n', 'last');
  assert.deepEqual(rowsOf(comparison).map((row) => [row.kind, row.text, row.newline]), [
    ['removed', 'last', true],
    ['added', 'last', false],
  ]);
});

test('CRLF-only differences compare equal, including blank lines and the final newline', async () => {
  const comparison = await compare('first\r\n\r\nlast\r\n', 'first\n\nlast\n');
  assert.equal(comparison.added, 0);
  assert.equal(comparison.removed, 0);
  assert.deepEqual(rowsOf(comparison).map((row) => [row.text, row.newline]), [
    ['first', true], ['', true], ['last', true],
  ]);
  const withoutFinalNewline = await compare('last\r\n', 'last');
  assert.equal(withoutFinalNewline.added, 1);
  assert.equal(withoutFinalNewline.removed, 1);
});

test('whitespace and standalone carriage returns are not ignored', async () => {
  const whitespace = await compare('same \n\t\nlast', 'same\n \nlast');
  assert.equal(whitespace.added, 2);
  assert.equal(whitespace.removed, 2);
  const carriageReturn = await compare('first\rlast', 'firstlast');
  assert.equal(carriageReturn.added, 1);
  assert.equal(carriageReturn.removed, 1);
  assert.equal(rowsOf(carriageReturn)[0].text, 'first\rlast');
});

test('snapshot splitting retains raw BOM/CRLF metadata and untouched body bytes', () => {
  const metadata = '\uFEFF---\r\ntitle: Snapshot\r\ndescription: "inline --- stays here"\r\n---\r\n';
  const body = '# Body\r\n\r\n---\r\ntrailing spaces  ';
  assert.deepEqual(splitSnapshot(metadata + body), { metadata, body });
});

test('frontmatter closes only at a complete delimiter line, not inline YAML values', () => {
  const metadata = '---\ntitle: "before --- after"\nother: ---\n---\n';
  const body = 'body\n---\nremaining body\n';
  assert.deepEqual(splitSnapshot(metadata + body), { metadata, body });
  assert.deepEqual(splitSnapshot('---\n---'), { metadata: '---\n---', body: '' });
  assert.deepEqual(splitSnapshot('---\ntitle: archived\n---'), {
    metadata: '---\ntitle: archived\n---', body: '',
  });
});

test('incomplete, nonleading and non-line delimiters remain entirely in the body', () => {
  const bodies = [
    '',
    '---',
    '---\ntitle: "inline --- is not a closing line"\nbody',
    '---\ntitle: archived\n--- suffix\nbody',
    '---\ntitle: archived\r---\nbody',
    '---\ntitle: archived\n---\r',
    'intro\n---\ntitle: archived\n---\nbody',
    ' ---\ntitle: archived\n---\nbody',
    '\uFEFFbody\r\n',
  ];
  for (const body of bodies) {
    assert.deepEqual(splitSnapshot(body), { body, metadata: '' });
  }
});

test('metadata changes do not become body changes when consumers compare split bodies', async () => {
  const current = splitSnapshot('---\ntitle: Live\ntags: [live]\n---\nsame body\n');
  const snapshot = splitSnapshot('---\ntitle: Archived\ntags: [old]\n---\nsame body\n');
  const comparison = await compare(current.body, snapshot.body);
  assert.notEqual(current.metadata, snapshot.metadata);
  assert.equal(comparison.added, 0);
  assert.equal(comparison.removed, 0);
});

test('long edge runs retain all hidden rows with exactly three visible context lines', async () => {
  const current = lines(25);
  const snapshot = [...current];
  snapshot[12] = 'archived line 13';
  const comparison = await compare(document(current), document(snapshot));
  assert.deepEqual(comparison.chunks.map((chunk) => [chunk.kind, chunk.rows.length]), [
    ['unchanged', 9], ['lines', 8], ['unchanged', 9],
  ]);
  assert.deepEqual(comparison.chunks[1].rows.map((row) => row.text), [
    'line 10', 'line 11', 'line 12', 'line 13', 'archived line 13',
    'line 14', 'line 15', 'line 16',
  ]);
  assert.deepEqual(comparison.chunks[0].rows.map((row) => row.currentLine), [1, 2, 3, 4, 5, 6, 7, 8, 9]);
  assert.deepEqual(comparison.chunks[2].rows.map((row) => row.snapshotLine), [17, 18, 19, 20, 21, 22, 23, 24, 25]);
});

test('long middle runs preserve three context rows on each side for expansion', async () => {
  const current = lines(16);
  const snapshot = [...current];
  snapshot[0] = 'archived first';
  snapshot[15] = 'archived last';
  const comparison = await compare(document(current), document(snapshot));
  assert.deepEqual(comparison.chunks.map((chunk) => [chunk.kind, chunk.rows.length]), [
    ['lines', 5], ['unchanged', 8], ['lines', 5],
  ]);
  assert.deepEqual(comparison.chunks[0].rows.filter((row) => row.kind === 'same').map((row) => row.text), [
    'line 2', 'line 3', 'line 4',
  ]);
  assert.deepEqual(comparison.chunks[1].rows.map((row) => row.text), current.slice(4, 12));
  assert.deepEqual(comparison.chunks[2].rows.filter((row) => row.kind === 'same').map((row) => row.text), [
    'line 13', 'line 14', 'line 15',
  ]);
});

test('overlapping context is not duplicated and the seventh middle line can collapse', async () => {
  for (const unchangedCount of [6, 7]) {
    const current = ['live first', ...lines(unchangedCount), 'live last'];
    const snapshot = ['archived first', ...lines(unchangedCount), 'archived last'];
    const comparison = await compare(document(current), document(snapshot));
    const hidden = comparison.chunks.filter((chunk) => chunk.kind === 'unchanged');
    assert.equal(hidden.length, unchangedCount === 6 ? 0 : 1);
    if (hidden.length) assert.deepEqual(hidden[0].rows.map((row) => row.text), ['line 4']);
    assert.equal(rowsOf(comparison).filter((row) => row.kind === 'same').length, unchangedCount);
  }
});

test('identical notes have zero additions/removals and retain long unchanged content', async () => {
  const short = await compare('same\nbody', 'same\nbody');
  assert.equal(short.added, 0);
  assert.equal(short.removed, 0);
  assert.deepEqual(short.chunks.map((chunk) => chunk.kind), ['lines']);

  const body = document(lines(3000));
  const long = await compare(body, body);
  assert.equal(long.added, 0);
  assert.equal(long.removed, 0);
  assert.deepEqual(long.chunks.map((chunk) => [chunk.kind, chunk.rows.length]), [['unchanged', 3000]]);
});

test('large notes preserve hidden content as well as an appended snapshot line', async () => {
  const current = document(lines(3000));
  const comparison = await compare(current, current + 'appended\n');
  assert.equal(comparison.added, 1);
  assert.equal(comparison.removed, 0);
  assert.deepEqual(comparison.chunks.map((chunk) => [chunk.kind, chunk.rows.length]), [
    ['unchanged', 2997], ['lines', 4],
  ]);
});

test('comparable bodies ignore only leading and trailing blank lines', () => {
  assert.equal(comparableBody(''), '');
  assert.equal(comparableBody('\n\n'), '');
  assert.equal(comparableBody('body'), 'body\n');
  assert.equal(comparableBody('\n\n# T\nbody\n\n'), '# T\nbody\n');
  assert.equal(comparableBody('\r\n# T\r\nbody\r\n'), '# T\r\nbody\n');
  assert.equal(comparableBody('a\n\n\nb'), 'a\n\n\nb\n');
  assert.equal(comparableBody(comparableBody('x\n')), 'x\n');
});

test('a stripped current body matches a snapshot that keeps its file whitespace', async () => {
  // The app hands the review the note body without edge newlines; the stored snapshot keeps them.
  const current = '# T\nbody';
  for (const raw of [
    '---\ntitle: T\n---\n# T\nbody\n',
    '---\ntitle: T\n---\n\n# T\nbody\n',
    '---\r\ntitle: T\r\n---\r\n\r\n# T\r\nbody\r\n',
  ]) {
    const comparison = await compareHistory(comparableBody(current), comparableBody(splitSnapshot(raw).body));
    assert.equal(comparison.added, 0, JSON.stringify(raw));
    assert.equal(comparison.removed, 0, JSON.stringify(raw));
  }
  const changed = await compareHistory(
    comparableBody(current),
    comparableBody(splitSnapshot('---\ntitle: T\n---\n# T\nedited\n').body),
  );
  assert.deepEqual([changed.added, changed.removed], [1, 1]);
  assert.deepEqual(
    rowsOf(changed).filter((row) => row.kind !== 'same').map((row) => [row.kind, row.text]),
    [['removed', 'body'], ['added', 'edited']],
  );
});
