import { build, context } from 'esbuild';
import { copyFileSync, mkdirSync, existsSync, readdirSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const out = join(root, 'dist', 'renderer');
mkdirSync(out, { recursive: true });

const isWatch = process.argv.includes('--watch');

// Watch mode = dev iteration: skip minify, emit sourcemaps for debugging.
// Plain build (used by `package`) = production: minify, no sourcemap.
/** @type {import('esbuild').BuildOptions} */
const config = {
  entryPoints: [join(root, 'src/renderer/renderer.ts')],
  bundle: true,
  outfile: join(out, 'renderer.js'),
  platform: 'browser',
  target: 'chrome120',
  format: 'esm',
  sourcemap: isWatch,
  minify: !isWatch,
};

async function copyAssets() {
  // Static files
  copyFileSync(join(root, 'src/renderer/index.html'), join(out, 'index.html'));
  copyFileSync(join(root, 'src/renderer/styles.css'), join(out, 'styles.css'));
}

if (isWatch) {
  const ctx = await context({
    ...config,
    plugins: [{
      name: 'assets-copier',
      setup(build) {
        build.onEnd(async () => {
          await copyAssets();
          console.log('Renderer rebuild complete, assets copied.');
        });
      }
    }]
  });
  await ctx.watch();
  console.log('Watching renderer...');
} else {
  await build(config);
  await copyAssets();
  console.log('Renderer build complete →', out);
}
