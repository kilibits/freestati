import { build } from 'esbuild';
import { copyFileSync, mkdirSync, existsSync, readdirSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const out = join(root, 'dist', 'renderer');
mkdirSync(out, { recursive: true });

// Bundle TypeScript renderer → single JS file
await build({
  entryPoints: [join(root, 'src/renderer/renderer.ts')],
  bundle: true,
  outfile: join(out, 'renderer.js'),
  platform: 'browser',
  target: 'chrome120',
  format: 'esm',
  sourcemap: true,
  minify: false,
});

// Static files
copyFileSync(join(root, 'src/renderer/index.html'), join(out, 'index.html'));
copyFileSync(join(root, 'src/renderer/styles.css'), join(out, 'styles.css'));

// Copy AG Grid CSS from node_modules
const agStylesDir = join(root, 'node_modules/ag-grid-community/styles');
if (existsSync(agStylesDir)) {
  for (const file of readdirSync(agStylesDir)) {
    if (file.endsWith('.css')) {
      copyFileSync(join(agStylesDir, file), join(out, file));
    }
  }
}

console.log('Renderer build complete →', out);
