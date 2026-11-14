// Host browser harness. Uses installed Vite/Svelte only; no downloads, native IPC
// or live vault access. A real browser is required to run the mounted assertions.
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { fileURLToPath } from 'node:url';
export const config = {
  configFile: false,
  root: fileURLToPath(new URL('../',import.meta.url)),
  plugins: [svelte()],
  resolve: { alias: { $lib: fileURLToPath(new URL('../src/lib',import.meta.url)) } },
  server: { host:'127.0.0.1',port:5189,strictPort:true },
  build: { outDir: '/tmp/ryuk-ui-acceptance-build', emptyOutDir: true, rollupOptions: { input:fileURLToPath(new URL('../tests/ui/index.html',import.meta.url)) } },
};
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  if (process.argv.includes('--build')) {
    const { build } = await import('vite');await build(config);
  } else {
    const server = await createServer(config);
    await server.listen();
    console.log('Open http://127.0.0.1:5189/tests/ui/index.html in a focused browser tab. The page runs mounted assertions and reports PASS/FAIL. No native/vault operations run.');
  }
}
