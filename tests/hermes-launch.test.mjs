import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { runInNewContext } from 'node:vm';
import ts from 'typescript';

const api = await readFile(new URL('../src/lib/api.ts', import.meta.url), 'utf8');
const editor = await readFile(new URL('../src/lib/components/Editor.svelte', import.meta.url), 'utf8');
function compileFunction(source, name, globals) {
  const ast = ts.createSourceFile('fixture.ts', source, ts.ScriptTarget.Latest, true);
  const declaration = ast.statements.find(n => ts.isFunctionDeclaration(n) && n.name.text === name);
  assert.ok(declaration, name);
  const js = ts.transpileModule(declaration.getText(ast).replace(/^export /, ''), {
    compilerOptions: { target: ts.ScriptTarget.ES2022 },
  }).outputText;
  return runInNewContext(`${js}\n${name}`, globals);
}

test('GUI start and restore pin the displayed vault through the real terminal adapter', async () => {
  const calls = [];
  const spawnRyukTerminal = compileFunction(api, 'spawnRyukTerminal', {
    invoke: async (name, args) => calls.push({ name, args: Array.from(args.args) }),
  });
  const script = editor.slice(editor.indexOf('<script'), editor.indexOf('</script>')).replace(/^<script[^>]*>/, '');
  for (const [name, args] of [
    ['generateSession', ['session', 'start', '--notes', 'note-a']],
    ['restoreSession', ['session', 'restore', '20261015_120000_ab12cd']],
  ]) {
    const action = compileFunction(script, name, {
      spawnRyukTerminal, $appConfig: { active_vault: '/displayed vault.rn' },
      $activeNotePath: 'note-a', sessionId: '20261015_120000_ab12cd', console,
    });
    await action();
    assert.deepEqual(calls.at(-1), {
      name: 'spawn_ryuk_terminal', args: ['--vault', '/displayed vault.rn', ...args],
    });
  }
  // Future resume-for-note callers use the same required vault boundary.
  await spawnRyukTerminal(['session', 'resume-for-note', 'note-a'], '/other.rn');
  assert.deepEqual(calls.at(-1).args, ['--vault', '/other.rn', 'session', 'resume-for-note', 'note-a']);
  const count = calls.length;
  for (const missing of [null, undefined, '']) {
    await assert.rejects(spawnRyukTerminal(['session', 'start'], missing), /displayed vault/);
  }
  assert.equal(calls.length, count);
});
