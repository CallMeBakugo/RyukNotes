import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, copyFileSync, existsSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

function fixture(t, lddBody) {
  const root = mkdtempSync(join(tmpdir(), 'ryuk-linux-install-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const tools = join(root, 'tools');
  mkdirSync(tools);
  copyFileSync(new URL('../scripts/install-linux.sh', import.meta.url), join(root, 'install.sh'));
  for (const binary of ['ryuknotes', 'ryuk']) writeFileSync(join(root, binary), binary);
  writeFileSync(join(tools, 'uname'), '#!/bin/sh\necho Linux\n', { mode: 0o755 });
  writeFileSync(join(tools, 'ldd'), `#!/bin/sh\n${lddBody}\n`, { mode: 0o755 });
  // Tests must never invoke a real package manager, even if prompt ordering regresses.
  writeFileSync(join(tools, 'sudo'), '#!/bin/sh\necho unexpected-package-install >&2\nexit 99\n', { mode: 0o755 });
  return {
    root,
    run(input) {
      return spawnSync('/bin/sh', [join(root, 'install.sh')], {
        encoding: 'utf8', input,
        env: { ...process.env, PATH: `${tools}:/usr/bin:/bin`, HOME: root, XDG_BIN_HOME: join(root, 'installed') },
      });
    },
  };
}

test('Linux installer reports missing libraries and refuses to copy binaries', t => {
  const f = fixture(t, 'echo "libwebkit2gtk-4.1.so.0 => not found"');
  const result = f.run('n\ny\n');
  assert.equal(result.status, 1);
  assert.match(result.stderr, /libwebkit2gtk-4\.1\.so\.0 => not found/);
  assert.match(result.stderr, /Installation stopped/);
  assert.equal(existsSync(join(f.root, 'installed')), false);
});

test('Linux installer checks the CLI as well as the GUI', t => {
  const f = fixture(t, 'case "$1" in */ryuk) echo "libc.so.6: version GLIBC_TEST not found"; exit 1;; esac\necho "all resolved"');
  const result = f.run('n\n');
  assert.equal(result.status, 1);
  assert.match(result.stderr, /GLIBC_TEST not found/);
  assert.equal(existsSync(join(f.root, 'installed')), false);
});

test('Linux installer preserves files when installation is declined', t => {
  const f = fixture(t, 'echo "all resolved"');
  const result = f.run('n\nn\n');
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /No binaries installed/);
  assert.equal(existsSync(join(f.root, 'installed')), false);
});

test('Linux installer copies both checked binaries only after confirmation', t => {
  const f = fixture(t, 'echo "all resolved"');
  // Unknown distros have only the final prompt; supported distros have two.
  const probe = f.run('n\nn\n');
  const hasDependencyPrompt = probe.stdout.includes('Run this command now?');
  const result = f.run(hasDependencyPrompt ? 'n\ny\n' : 'y\n');
  assert.equal(result.status, 0, result.stderr);
  for (const binary of ['ryuknotes', 'ryuk']) {
    assert.equal(readFileSync(join(f.root, 'installed', binary), 'utf8'), binary);
  }
});
