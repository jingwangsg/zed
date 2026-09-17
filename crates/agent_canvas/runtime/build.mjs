import * as esbuild from 'esbuild-wasm';
import * as React from 'react';
import * as JSX from 'react/jsx-runtime';
import { mkdir, copyFile, writeFile, readdir, readFile } from 'node:fs/promises';
import { dirname } from 'node:path';
import { createHash } from 'node:crypto';

await mkdir('dist', { recursive: true });
await esbuild.build({ entryPoints: ['vendor.jsx'], bundle: true, outfile: 'dist/vendor.js', platform: 'browser', format: 'iife', jsx: 'automatic', target: 'safari15', minify: true, define: { 'process.env.NODE_ENV': '"production"' }, legalComments: 'linked' });
const sdk = await esbuild.build({ entryPoints: ['sdk.jsx'], write: false, format: 'esm', metafile: true });
await writeFile('dist/modules.json', JSON.stringify({ react: Object.keys(React), 'react/jsx-runtime': Object.keys(JSX), '@zed/canvas': Object.values(sdk.metafile.outputs)[0].exports }));
for (const file of ['compile.cjs', 'shell.html', 'sdk.d.ts']) await copyFile(file, 'dist/' + file);
for (const file of ['package.json', 'lib/main.js', 'bin/esbuild', 'wasm_exec.js', 'wasm_exec_node.js', 'esbuild.wasm', 'LICENSE.md']) {
  const destination = 'dist/esbuild/' + file;
  await mkdir(dirname(destination), { recursive: true });
  await copyFile('node_modules/esbuild-wasm/' + file, destination);
}
await mkdir('dist/typescript', { recursive: true });
for (const file of await readdir('node_modules/typescript/lib')) {
  if (file === 'typescript.js' || file.endsWith('.d.ts')) await copyFile('node_modules/typescript/lib/' + file, 'dist/typescript/' + file);
}
await writeFile('dist/typescript/package.json', '{"type":"commonjs"}');
await copyFile('node_modules/typescript/LICENSE.txt', 'dist/typescript/LICENSE.txt');
for (const [directory, files] of [
  ['react', ['index.d.ts', 'global.d.ts', 'jsx-runtime.d.ts', 'jsx-dev-runtime.d.ts']],
  ['csstype', ['index.d.ts']],
]) {
  await mkdir('dist/types/' + directory, { recursive: true });
  const source = directory === 'react' ? 'node_modules/@types/react/' : 'node_modules/csstype/';
  for (const file of files) await copyFile(source + file, 'dist/types/' + directory + '/' + file);
  await copyFile(source + 'LICENSE', 'dist/types/' + directory + '/LICENSE');
}
const version = createHash('sha256');
for (const file of ['dist/vendor.js', 'dist/compile.cjs', 'dist/sdk.d.ts', 'dist/modules.json', 'dist/shell.html', 'dist/typescript/typescript.js', 'dist/esbuild/esbuild.wasm', 'dist/types/react/index.d.ts', 'dist/types/react/jsx-runtime.d.ts', 'dist/types/csstype/index.d.ts']) version.update(await readFile(file));
await writeFile('dist/version', version.digest('hex'));
esbuild.stop();
