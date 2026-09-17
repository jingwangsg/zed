import { test, after } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
const { compile } = require('../dist/compile.cjs');
after(() => require('../dist/esbuild/lib/main.js').stop());

test('compiles a React report with state and charts', async () => {
  const output = await compile("import {useCanvasState, BarChart} from '@zed/canvas'; export default function Report() { const [value] = useCanvasState('count', 1); return <BarChart categories={['sample']} series={[{name:'value', values:[value]}]} />; }");
  assert.match(output, /CanvasModule/);
  assert.match(output, /sourceMappingURL/);
});

test('rejects filesystem, URL and unprovided package imports', async () => {
  for (const path of ['node:fs', '/etc/passwd', './secret.json', 'https://example.com/script.js', 'lodash']) {
    await assert.rejects(compile(`import value from ${JSON.stringify(path)}; export default () => <div>{value}</div>`), /Unsupported Canvas import/);
  }
});

test('returns a source location for malformed TSX and unknown SDK exports', async () => {
  await assert.rejects(compile('export default () => <div>'), /L1:/);
  await assert.rejects(compile("import {runShell} from '@zed/canvas'; export default () => <div>{runShell()}</div>"), /has no exported member/);
});

test('checks SDK property and data types before rendering', async () => {
  await assert.rejects(compile("import {BarChart} from '@zed/canvas'; export default () => <BarChart categories={['A']} series={[{name:'count',values:['twelve']}]} />"), /Type 'string' is not assignable to type 'number'/);
  await assert.rejects(compile("import {Stat} from '@zed/canvas'; export default () => <Stat label='Errors' value={3} tone='neon' />"), /neon/);
  await assert.rejects(compile("import {CardHeader} from '@zed/canvas'; export default () => <CardHeader />"), /title/);
});
