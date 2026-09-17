import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { webkit } from 'playwright';

const require = createRequire(import.meta.url);
const { compile } = require('../dist/compile.cjs');
const theme = { background: '#ffffff', foreground: '#222222', muted: '#666666', border: '#dddddd', accent: '#3267dd', kind: 'light' };
const source = `
import {useCanvasState, useCanvasAction} from '@zed/canvas';
export default function Report() {
  const dispatch = useCanvasAction();
  const [count, setCount] = useCanvasState('count', 0);
  return <main><h1 data-canvas-id="title">Canvas report</h1>
    <button onClick={() => setCount(previous => previous + 1)}>Count: {count}</button>
    <button onClick={() => dispatch({type:'askAgent', prompt:'Explain these results'})}>Ask Agent</button></main>;
}`;
let browser;
let vendor;
let shell;

before(async () => {
  browser = await webkit.launch();
  vendor = await readFile(new URL('../dist/vendor.js', import.meta.url), 'utf8');
  shell = await readFile(new URL('../dist/shell.html', import.meta.url), 'utf8');
});
after(async () => { await browser?.close(); require('../dist/esbuild/lib/main.js').stop(); });

// This is the WebKit boundary: the native host supplies source, state, theme,
// and a message handler, without exposing an agent or filesystem to the page.
async function openPreview(source, state = {}) {
  const page = await browser.newPage();
  const messages = [];
  await page.exposeFunction('receiveCanvasMessage', message => messages.push(JSON.parse(message)));
  await page.evaluate(() => { window.webkit = { messageHandlers: { zedCanvas: { postMessage: message => window.receiveCanvasMessage(message) } } }; });
  const javascript = await compile(source);
  const scripts = [vendor, javascript, `__zedCanvasHost.mount(${JSON.stringify(state)}, ${JSON.stringify(theme)});`]
    .map(script => `<script src="data:application/javascript;base64,${Buffer.from(script).toString('base64')}"></script>`).join('');
  await page.setContent(shell.replace('<!--CANVAS_SCRIPTS-->', scripts));
  return { page, messages };
}

test('state restores, buttons prepare prompts, and selection prevents the normal click', async () => {
  const { page, messages } = await openPreview(source, { count: 4 });
  await page.getByRole('button', { name: 'Count: 4', exact: true }).click();
  await page.getByRole('button', { name: 'Count: 5', exact: true }).waitFor();
  await page.getByRole('button', { name: 'Ask Agent', exact: true }).click();
  await page.evaluate(() => __zedCanvasHost.setSelecting(true));
  await page.getByRole('heading').click();
  await page.waitForFunction(() => !document.body.classList.contains('zed-selecting'));
  assert.ok(messages.some(message => message.kind === 'ready'));
  assert.ok(messages.some(message => message.kind === 'state' && message.key === 'count' && message.value === 5));
  assert.deepEqual(messages.filter(message => message.kind === 'action'), [{ kind: 'action', action: {type:'askAgent', prompt: 'Explain these results'} }]);
  assert.equal(messages.find(message => message.kind === 'selection').elements[0].id, 'title');
  assert.ok(messages.every(message => ['ready', 'state', 'action', 'selection'].includes(message.kind)));
  const restored = await openPreview(source.replace('Canvas report', 'Updated report'), { count: 5 });
  await restored.page.getByRole('button', { name: 'Count: 5', exact: true }).waitFor();
  await restored.page.close();
  await page.close();
});

test('runtime errors are visible and are reported without a false ready signal', async () => {
  const { page, messages } = await openPreview('export default function Report() { throw new Error("Broken report"); }');
  await page.getByRole('alert').waitFor();
  assert.match(await page.getByRole('alert').innerText(), /Broken report/);
  assert.ok(messages.some(message => message.kind === 'error' && message.error.includes('Broken report')));
  assert.ok(!messages.some(message => message.kind === 'ready'));
  await page.close();
});

test('the production shell blocks network requests and keeps HTML-like data inert', async () => {
  const { page } = await openPreview(source, { count: '</script><script>window.injected = true</script>' });
  await page.getByRole('heading').waitFor();
  const requests = [];
  page.on('request', request => { if (request.url().startsWith('https:')) requests.push(request.url()); });
  const result = await page.evaluate(async () => {
    try { await fetch('https://example.com/canvas-test'); return 'allowed'; }
    catch { return 'blocked'; }
  });
  assert.equal(result, 'blocked');
  assert.equal(await page.evaluate(() => window.injected), undefined);
  assert.equal(requests.length, 0);
  await page.close();
});

test('SDK reports preserve filters and accept host state and theme updates', async () => {
  const report = `import {Stack, Grid, H1, Stat, Table, Select, BarChart, useCanvasState} from '@zed/canvas';
  export default function Report() {
    const [group,setGroup] = useCanvasState('group','all');
    const rows = [{name:'A',count:12},{name:'B',count:8}].filter(row => group==='all'||row.name===group);
    return <Stack><H1>Measured results</H1><Select label='Group' value={group} onChange={setGroup} options={[{value:'all',label:'All'},{value:'A',label:'A'}]} />
      <Grid><Stat label='Total' value={rows.reduce((total,row)=>total+row.count,0)} /><Stat label='Groups' value={rows.length} /></Grid>
      <Table headers={['Group','Count']} rows={rows.map(row=>[row.name,row.count])} />
      <BarChart categories={rows.map(row=>row.name)} series={[{name:'Count',values:rows.map(row=>row.count)}]} xLabel='Group' yLabel='Count' />
    </Stack>;
  }`;
  const {page,messages} = await openPreview(report);
  await page.locator('.recharts-label').filter({ hasText: /^Group$/ }).waitFor();
  await page.waitForFunction(() => {
    const label = [...document.querySelectorAll('.recharts-label')].find(label => label.textContent === 'Group');
    const tick = [...document.querySelectorAll('.recharts-cartesian-axis-tick-value')].find(tick => tick.textContent === 'A');
    return label && tick && label.getBoundingClientRect().y > tick.getBoundingClientRect().y;
  });
  const axisLabel = await page.locator('.recharts-label').filter({ hasText: /^Group$/ }).evaluate(element => element.getBoundingClientRect().toJSON());
  const legend = await page.locator('.recharts-legend-wrapper').evaluate(element => element.getBoundingClientRect().toJSON());
  assert.ok(axisLabel && legend);
  assert.ok(axisLabel.y + axisLabel.height <= legend.y || legend.y + legend.height <= axisLabel.y, 'axis label and legend must not overlap');
  await page.getByRole('combobox').selectOption('A');
  await page.waitForFunction(() => document.querySelectorAll('tbody tr').length === 1);
  assert.ok(messages.some(message=>message.kind==='state'&&message.key==='group'&&message.value==='A'));
  await page.evaluate(() => __zedCanvasHost.updateState({group:'all'}));
  await page.waitForFunction(() => document.querySelectorAll('tbody tr').length === 2);
  await page.evaluate(theme=>__zedCanvasHost.updateTheme(theme), {...theme,background:'#111111',foreground:'#eeeeee',kind:'dark'});
  assert.equal(await page.evaluate(()=>getComputedStyle(document.body).backgroundColor),'rgb(17, 17, 17)');
  await page.close();
});
