import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { runInNewContext } from 'node:vm';
import ts from 'typescript';
import { Schema } from '@tiptap/pm/model';
import { createEditorMarkdownParser as markdown } from '../src/lib/editor/markdownParser.ts';
import { externalNoteReferences } from '../src/lib/editor/externalNote.ts';
import { serializeInlineMarkdown } from '../src/lib/editor/markdown.ts';
import { dbAttachmentTarget, noteAssetRoot, resolveFileReference, encodeMarkdownPath } from '../src/lib/utils/paths.ts';

const adapterSource = await readFile(new URL('../src/lib/api-db.ts', import.meta.url), 'utf8');
const apiSource = await readFile(new URL('../src/lib/api.ts', import.meta.url), 'utf8');
const editorSource = await readFile(new URL('../src/lib/components/Editor.svelte', import.meta.url), 'utf8');
function load(source, globals = {}) {
  const exports = {};
  const context = { exports, ...globals };
  runInNewContext(ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText, context);
  return { exports, context };
}
function functions(source, names) {
  const ast = ts.createSourceFile('fixture.ts', source, ts.ScriptTarget.Latest, true);
  return ast.statements.filter(n => ts.isFunctionDeclaration(n) && names.includes(n.name?.text))
    .map(n => n.getText(ast) + `\nexports.${n.name.text} = ${n.name.text};`).join('\n');
}
function adapter(invoke) {
  return load(adapterSource, { require: name => {
    assert.equal(name, '@tauri-apps/api/core'); return { invoke };
  } }).exports;
}
function api(invoke) {
  return load(functions(apiSource, ['saveAttachment', 'readAttachmentFile', 'openFile', 'copyFileTo', 'exportDbToMd', 'importMdFolderIntoDb']), {
    ...adapter(invoke), invoke, dbAttachmentTarget, looksLikeDbVault: path => path.endsWith('.rn'),
  }).exports;
}
const reference = '.rn-attachments/54fd232f-c42a-432b-8ba8-e1b346d23a07';

test('non-images route through capped native BLOB insertion with captured vault and owner', async () => {
  const calls = [];
  const f = api(async (name, args) => { calls.push([name, args]); return reference; });
  const bytes = [0, 255, 13, 10];
  assert.equal(await f.saveAttachment('/vault.rn', 'file.bin', bytes, 'right-note'), reference);
  assert.equal(calls[0][0], 'db_save_attachment');
  assert.equal(JSON.stringify(calls[0][1]), JSON.stringify({ vaultPath: '/vault.rn', noteId: 'right-note', name: 'file.bin', data: bytes }));
  await assert.rejects(f.saveAttachment('/vault.rn', 'no-owner', bytes), /Select a note/);
  assert.equal(calls.length, 1);
  await f.saveAttachment('/directory', 'file.bin', bytes, 'unused');
  assert.equal(calls[1][0], 'save_attachment');
  assert.equal('noteId' in calls[1][1], false);
});

test('download routing tokens pin the vault without fabricating filesystem BLOB paths', () => {
  const vault = '/notes/snow 雪 #1.rn';
  const root = noteAssetRoot('right-note', vault, []);
  assert.equal(root, vault);
  const token = resolveFileReference(reference + '#preview', 'right-note', root).path;
  assert.ok(token.startsWith('rnattachment:'));
  assert.deepEqual(dbAttachmentTarget(token), { vault, id: reference.split('/')[1] });
  assert.equal(dbAttachmentTarget('/made-up.rn/' + reference), null);
  assert.equal(dbAttachmentTarget('rnattachment:///%ZZ/id'), null);
  // External viewer references resolve in their own directory, never the DB.
  assert.equal(noteAssetRoot('/export/note.md', vault, []), null);
  assert.equal(resolveFileReference('.ryuknotes/attachments/file.pdf', '/export/note.md', null).path,
    '/export/.ryuknotes/attachments/file.pdf');
});

test('download retains its original vault across a save dialog and honors cancellation', async () => {
  const calls = [];
  const f = adapter(async (name, args) => {
    calls.push([name, args]);
    if (name === 'db_attachment_info') return { filename: 'report.pdf' };
  });
  let finish;
  const saving = f.dbDownloadAttachment('/captured.rn', 'id', name => {
    assert.equal(name, 'report.pdf'); return new Promise(resolve => { finish = resolve; });
  });
  await new Promise(resolve => setImmediate(resolve));
  finish('/selected/report.pdf');
  await saving;
  assert.deepEqual(calls.map(([name]) => name), ['db_attachment_info', 'db_download_attachment']);
  assert.equal(calls[1][1].vaultPath, '/captured.rn');
  assert.equal(calls[1][1].destination, '/selected/report.pdf');
  await f.dbDownloadAttachment('/captured.rn', 'id', async () => null);
  assert.equal(calls.length, 3);
});

test('native download errors propagate with no filesystem fallback', async () => {
  const f = adapter(async (name) => {
    if (name === 'db_attachment_info') return { filename: 'x' };
    throw new Error('vault switched or destination denied');
  });
  await assert.rejects(f.dbDownloadAttachment('/old.rn', 'id', async () => '/dest'), /vault switched/);
  const calls = [];
  const b = api(async (name, args) => { calls.push([name, args]); });
  const token = resolveFileReference(reference, 'note', '/old.rn').path;
  await b.copyFileTo(token, '/selected');
  assert.equal(calls[0][0], 'db_download_attachment');
  assert.equal(calls[0][1].vaultPath, '/old.rn');
});

const script = editorSource.slice(editorSource.indexOf('>') + 1, editorSource.indexOf('</script>'));
function editorFixture(sourceMode, save) {
  const origin = { vault: '/right.rn', path: 'right', noteId: 'right', sourceMode };
  const inserted = [];
  let current = true;
  let blocked = false;
  let epoch = 0;
  const f = load(functions(script, ['handleFilePaste', 'handleFileDrop', 'insertFile', 'uploadAsset']), {
    captureOrigin: () => origin, canInsert: candidate => current && candidate === origin,
    workspace: { isBlocked: () => blocked, captureOperation: () => {
      const captured = epoch; return () => !blocked && captured === epoch;
    } }, saveAttachment: save,
    saveImage: () => assert.fail('non-images must not use the image command'),
    encodeMarkdownPath, console, isMobile: false, $appConfig: { pdf_preview: true },
    source: { state: { selection: { main: { from: 0, to: 0 } } }, replace: text => inserted.push(text) },
    editor: { commands: { insertContent: value => inserted.push(value) } },
  });
  return { ...f.exports, origin, inserted, switchNote: () => { current = false; }, block: () => { blocked = true; epoch++; }, unblock: () => { blocked = false; } };
}
for (const input of ['paste', 'drop']) {
  test(`${input} preserves multiple non-image files and captured pane ownership`, async () => {
    const calls = [];
    const f = editorFixture(true, async (...args) => { calls.push(args); return reference; });
    const files = ['report.pdf', 'raw.bin'].map(name => ({
      name, type: name.endsWith('.pdf') ? 'application/pdf' : '',
      arrayBuffer: async () => Uint8Array.of(0, 255).buffer,
    }));
    const event = { preventDefault() {} };
    if (input === 'paste') {
      event.clipboardData = { items: files.map(file => ({ getAsFile: () => file })) };
      assert.equal(f.handleFilePaste(event), true);
    } else {
      event.dataTransfer = { files };
      assert.equal(f.handleFileDrop(event), true);
    }
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(calls.length, 2);
    assert.ok(calls.every(args => args[0] === '/right.rn' && args[3] === 'right'));
    assert.deepEqual(f.inserted, [
      `[report.pdf (0 kB)](${reference}) `, `[raw.bin (0 kB)](${reference}) `,
    ]);
  });
}

test('DB PDFs are standard inline links even with PDF preview enabled', async () => {
  const f = editorFixture(false, async () => reference);
  await f.uploadAsset(f.origin, 'paper.pdf', [37, 80, 68, 70], 'pdf');
  assert.equal(f.inserted[0][0].type, 'text');
  assert.equal(f.inserted[0][0].marks[0].attrs.href, reference);
  assert.equal(f.inserted[0][0].text, 'paper.pdf (0 kB)');
});

test('stale pane completions and maintenance barriers cannot insert or redirect attachments', async () => {
  let finish;
  let calls = 0;
  const f = editorFixture(true, () => { calls++; return new Promise(resolve => { finish = resolve; }); });
  const pending = f.uploadAsset(f.origin, 'file', [0], 'file');
  f.switchNote();
  finish(reference);
  await pending;
  assert.equal(f.inserted.length, 0);
  const g = editorFixture(true, () => assert.fail('barrier must prevent storage'));
  g.block();
  await g.uploadAsset(g.origin, 'file', [0], 'file');
  assert.equal(g.inserted.length, 0);
  assert.equal(calls, 1);
});

test('attachment links survive rich/source serialization, viewer import references and snapshot Markdown parsing', () => {
  const schema = new Schema({ nodes: { doc: { content: 'block+' }, paragraph: { content: 'inline*', group: 'block' }, text: { group: 'inline' } }, marks: { link: { attrs: { href: {} } } } });
  const node = schema.node('paragraph', null, [schema.text('paper.pdf (2 kB)', [schema.marks.link.create({ href: reference + '#page=2' })])]);
  const source = serializeInlineMarkdown(node);
  assert.equal(source, `[paper.pdf (2 kB)](${reference}#page=2)`);
  const html = markdown().render(source);
  assert.ok(html.includes(`href="${reference}#page=2"`));
  const refs = externalNoteReferences(source);
  assert.equal(refs.length, 1);
  assert.equal(refs[0].href, reference + '#page=2');
  const portable = source.replace('.rn-attachments/', '.ryuknotes/attachments/');
  assert.equal(externalNoteReferences(portable)[0].href, portable.slice(portable.indexOf('](') + 2, -1));
});

test('file reads and BLOB writes completing after a finished vault job cannot insert', async () => {
  let finishRead;
  const f = editorFixture(true, () => assert.fail('stale read must not reach BLOB storage'));
  const pendingRead = f.insertFile({ name: 'file', type: '', arrayBuffer: () => new Promise(resolve => { finishRead = resolve; }) });
  f.block(); f.unblock();
  finishRead(Uint8Array.of(1).buffer);
  await pendingRead;
  assert.equal(f.inserted.length, 0);
  let finishSave;
  const g = editorFixture(true, () => new Promise(resolve => { finishSave = resolve; }));
  const pendingSave = g.uploadAsset(g.origin, 'file', [0], 'file');
  g.block(); g.unblock();
  finishSave(reference);
  await pendingSave;
  assert.equal(g.inserted.length, 0);
});


test('native file reads and import/export forward the captured vault to registered commands', async () => {
  const calls = [];
  const f = api(async (name, args) => {
    calls.push([name, args]);
    return name === 'db_read_attachment_file' ? [0, 255, 13] : 2;
  });
  assert.deepEqual(Array.from(await f.readAttachmentFile('/selected/file', '/captured.rn')), [0, 255, 13]);
  await f.exportDbToMd('/selected/export', '/captured.rn');
  await f.importMdFolderIntoDb('/selected/source', '/captured.rn');
  assert.deepEqual(calls.map(([name]) => name), ['db_read_attachment_file', 'db_export_md', 'db_import_md_vault']);
  assert.ok(calls.every(([, args]) => args.vaultPath === '/captured.rn'));
});

test('the real open-file adapter recognizes attachment download tokens before filesystem opening', async () => {
  let captured;
  const f = load(functions(apiSource, ['openFile']), {
    dbAttachmentTarget,
    dbDownloadAttachment: async (vault, id) => { captured = [vault, id]; },
    invoke: () => assert.fail('BLOBs must not be passed to the filesystem opener'),
  }).exports;
  await f.openFile(resolveFileReference(reference, 'right', '/displayed.rn').path);
  assert.deepEqual(captured, ['/displayed.rn', reference.split('/')[1]]);
});

test('oversized browser files are rejected before reading their bytes', async () => {
  const f = editorFixture(true, () => assert.fail('oversized file must not reach storage'));
  await f.insertFile({ name: 'huge', type: '', size: 25 * 1024 * 1024 + 1,
    arrayBuffer: () => assert.fail('oversized file must not be allocated'),
  });
  assert.equal(f.inserted.length, 0);
});

test('context-menu file paste cannot replay a clipboard read across a completed vault job', async () => {
  let finish;
  let current = true;
  const f = load(functions(script, ['ctxPaste']), {
    editor: {}, $readOnly: false, $viewerNote: null,
    captureOrigin: () => ({}), canInsert: () => true,
    workspace: { captureOperation: () => () => current },
    readWebClipboard: () => new Promise(resolve => { finish = resolve; }),
    replayPaste: () => assert.fail('a stale clipboard read must not be replayed'),
  }).exports;
  const pending = f.ctxPaste();
  current = false;
  finish({ files: [{ name: 'report.pdf' }] });
  await pending;
});

test('previously imported PDF embed markup uses a download link rather than an iframe', () => {
  const ast = ts.createSourceFile('Editor.ts', script, ts.ScriptTarget.Latest, true);
  const declaration = ast.statements.find(n => ts.isVariableStatement(n)
    && n.declarationList.declarations.some(d => d.name.getText(ast) === 'PdfEmbed'));
  const node = declaration.declarationList.declarations.find(d => d.name.getText(ast) === 'PdfEmbed');
  const method = node.initializer.arguments[0].properties.find(p => p.name?.getText(ast) === 'renderHTML');
  const globals = {
    isMobile: false, $appConfig: { pdf_preview: true, pdf_height: 600 },
    mergeAttributes: attrs => attrs, $activeNotePath: 'right',
    currentNoteAssetRoot: () => '/displayed.rn', resolveFileReference,
    convertFileSrc: () => assert.fail('DB PDFs must never be handed to the filesystem renderer'),
  };
  const f = load(`const node = { ${method.getText(ast)} }; exports.render = node.renderHTML;`, globals).exports;
  const rendered = f.render({ HTMLAttributes: { src: reference, name: 'paper.pdf' } });
  assert.equal(rendered[2][0], 'a');
  assert.equal(rendered[2][1].href, reference);
  const pipeline = load(functions(script, ['markdownToHtml']), {
    ...globals, mdit: markdown(), stripTitleH1: text => text,
    normalizeImageHtml: html => html,
  }).exports;
  const html = pipeline.markdownToHtml(`<div data-pdf-src="${reference}" data-pdf-name="paper.pdf"></div>`);
  assert.ok(!html.includes('<iframe'));
  assert.ok(html.includes(`href="${reference}"`));
});

const settingsSource = await readFile(new URL('../src/lib/components/SettingsPanel.svelte', import.meta.url), 'utf8');
const settingsScript = settingsSource.slice(settingsSource.indexOf('>') + 1, settingsSource.indexOf('</script>'));
for (const action of ['handleExportMd', 'handleImportMdFolder']) {
  test(`${action} holds the all-pane barrier and passes the vault captured before the picker`, async () => {
    let locked = 0;
    let releases = 0;
    const native = async (folder, vault) => {
      assert.equal(folder, '/selected');
      assert.equal(vault, '/displayed.rn');
      assert.equal(locked, 2);
      return action === 'handleExportMd' ? 2 : { notesImported: 2, notesTrashed: 0, versionsImported: 1, warnings: [] };
    };
    const f = load(functions(settingsScript, [action]), {
      maintenanceVault: '/displayed.rn', mdExportLoading: false, mdImportLoading: false,
      mdExportResult: null, mdImportResult: null, mdImportError: null,
      openDialog: async () => '/selected',
      onBeginMaintenance: async () => { locked = 2; return async () => { locked = 0; releases++; }; },
      exportDbToMd: native, importMdFolderIntoDb: native,
    });
    await f.exports[action]();
    assert.equal(locked, 0);
    assert.equal(releases, 1);
    assert.equal(f.context.mdExportLoading, false);
    assert.equal(f.context.mdImportLoading, false);
  });
}

test('export refuses a failed pane flush and a vault change while choosing the destination', async () => {
  const globals = {
    maintenanceVault: '/old.rn', mdExportLoading: false, mdExportResult: null,
    openDialog: async () => '/selected', onBeginMaintenance: async () => null,
    exportDbToMd: () => assert.fail('failed barriers must prevent native export'),
  };
  const f = load(functions(settingsScript, ['handleExportMd']), globals);
  await f.exports.handleExportMd();
  assert.match(f.context.mdExportResult, /Save every pane/);
  let choose;
  const g = load(functions(settingsScript, ['handleExportMd']), {
    ...globals, openDialog: () => new Promise(resolve => { choose = resolve; }),
    onBeginMaintenance: () => assert.fail('a stale destination must not acquire the new vault'),
  });
  const pending = g.exports.handleExportMd();
  g.context.maintenanceVault = '/new.rn';
  choose('/selected');
  await pending;
  assert.equal(g.context.mdExportLoading, false);
});
