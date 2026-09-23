#!/usr/bin/env node
// Dependency-free browser smoke test. Build web/pkg first, then run with Node
// 22+ and APTERONOTUS_CHROMIUM pointing to a Chromium executable if necessary.
import assert from 'node:assert/strict';
import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const output = path.join(root, 'target/browser-smoke');
const webRoot = path.join(root, 'web');
await fs.mkdir(output, { recursive: true });
const runDirectory = await fs.mkdtemp(path.join(output, 'run-'));
const downloads = path.join(runDirectory, 'downloads');
await fs.mkdir(downloads);
const valid = '-- Browser round trip\r\nlocal v = voice {graph = function() return sine(220) * 0.05 >> pan(0) end}\r\nplay(v, "c4 ~")\r\n';
const invalid = '-- 水\r\nlocal value = )\r\n';
const searchable = '-- é 水🌊 first\r\n-- 水🌊 second\r\nlocal orbit = 1\r\n';
await fs.writeFile(path.join(runDirectory, 'valid.eod'), valid);
await fs.writeFile(path.join(runDirectory, 'invalid.eod'), invalid);
await fs.writeFile(path.join(runDirectory, 'search.eod'), searchable);
await fs.writeFile(path.join(runDirectory, 'oversized.eod'), Buffer.alloc(1024 * 1024 + 1));

const server = http.createServer(async (request, response) => {
  try {
    const pathname = decodeURIComponent(new URL(request.url, 'http://localhost').pathname);
    const file = path.resolve(webRoot, `.${pathname === '/' ? '/index.html' : pathname}`);
    if (!file.startsWith(`${webRoot}${path.sep}`)) throw Error('outside web root');
    const body = await fs.readFile(file);
    response.setHeader('Content-Type', file.endsWith('.wasm') ? 'application/wasm'
      : file.endsWith('.js') ? 'application/javascript' : file.endsWith('.svg') ? 'image/svg+xml' : 'text/html');
    response.end(body);
  } catch { response.writeHead(404); response.end(); }
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const profile = path.join(runDirectory, 'profile');
let browser, ws, browserLog = '';
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(check, label, milliseconds = 15000) {
  const end = Date.now() + milliseconds;
  while (Date.now() < end) {
    const value = await check();
    if (value) return value;
    await delay(100);
  }
  throw Error(`Timed out: ${label}`);
}
try {
  browser = spawn(process.env.APTERONOTUS_CHROMIUM || 'chromium', [
    '--headless=new', '--no-sandbox', '--disable-dev-shm-usage', '--remote-debugging-port=0',
    `--user-data-dir=${profile}`, '--no-first-run', '--no-default-browser-check',
    '--use-gl=angle', '--use-angle=swiftshader', '--enable-unsafe-swiftshader', 'about:blank',
  ], { stdio: ['ignore', 'ignore', 'pipe'] });
  let spawnError;
  browser.on('error', error => { spawnError = error; });
  browser.stderr.on('data', chunk => { browserLog += chunk; });
  const port = await until(async () => {
    if (spawnError) throw spawnError;
    try { return Number((await fs.readFile(path.join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0]); }
    catch { return false; }
  }, 'Chromium debugging port');
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  ws = new WebSocket(targets.find(target => target.type === 'page').webSocketDebuggerUrl);
  await new Promise(resolve => ws.addEventListener('open', resolve, { once: true }));
  let sequence = 0;
  const pending = new Map(), exceptions = [];
  let fileChoosers = 0;
  ws.addEventListener('message', event => {
    const message = JSON.parse(event.data);
    if (message.id) {
      const request = pending.get(message.id);
      pending.delete(message.id);
      if (request) {
        clearTimeout(request.timeout);
        if (message.error) request.reject(Error(JSON.stringify(message.error)));
        else request.resolve(message.result);
      }
    } else if (message.method === 'Runtime.exceptionThrown') exceptions.push(message.params);
    else if (message.method === 'Page.fileChooserOpened') fileChoosers++;
  });
  const call = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++sequence;
    const timeout = setTimeout(() => { pending.delete(id); reject(Error(`CDP timeout: ${method}`)); }, 30000);
    pending.set(id, { resolve, reject, timeout });
    ws.send(JSON.stringify({ id, method, params }));
  });
  const evaluate = async expression => {
    const result = await call('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    if (result.exceptionDetails) throw Error(JSON.stringify(result.exceptionDetails));
    return result.result.value;
  };
  const click = async (x, y) => {
    for (const type of ['mousePressed', 'mouseReleased']) {
      await call('Input.dispatchMouseEvent', { type, x, y, button: 'left', clickCount: 1 });
    }
    await delay(100);
  };
  const key = async (key, code, windowsVirtualKeyCode, modifiers = 2) => {
    for (const type of ['keyDown', 'keyUp']) {
      await call('Input.dispatchKeyEvent', { type, key, code, windowsVirtualKeyCode, modifiers });
    }
    await delay(100);
  };
  const screenshot = async name => {
    const shot = await call('Page.captureScreenshot');
    await fs.writeFile(path.join(runDirectory, `${name}.png`), Buffer.from(shot.data, 'base64'));
  };
  const importFile = async name => {
    await click(240, 130);
    const before = fileChoosers;
    await key('o', 'KeyO', 79);
    await until(() => fileChoosers > before, 'import command opens the file picker');
    await until(() => evaluate("!!document.querySelector('input[type=file]')"), 'file picker');
    const input = await call('Runtime.evaluate', { expression: "document.querySelector('input[type=file]')" });
    await call('DOM.setFileInputFiles', { files: [path.join(runDirectory, name)], objectId: input.result.objectId });
    await delay(450);
  };
  const download = async name => {
    const file = path.join(downloads, name);
    await fs.rm(file, { force: true });
    await click(240, 130);
    await key('s', 'KeyS', 83);
    return until(async () => {
      try { return await fs.readFile(file, 'utf8'); } catch { return false; }
    }, `download ${name}`);
  };
  await call('Page.enable');
  await call('Page.setInterceptFileChooserDialog', { enabled: true });
  await call('Runtime.enable');
  await call('Emulation.setDeviceMetricsOverride', { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
  await call('Page.setDownloadBehavior', { behavior: 'allow', downloadPath: downloads });
  await call('Page.addScriptToEvaluateOnNewDocument', { source: `
    window.__apteronotusContexts = [];
    const OriginalContext = window.AudioContext;
    window.AudioContext = class extends OriginalContext {
      constructor(...args) { super(...args); window.__apteronotusContexts.push(this); }
    };
  ` });
  await call('Page.navigate', { url: `http://127.0.0.1:${server.address().port}/` });
  await until(() => evaluate("document.querySelector('apteronotus-app')?.shadowRoot?.querySelector('canvas')?.width === 1280"), 'canvas', 60000);
  await delay(600);
  await importFile('search.eod');
  await key('f', 'KeyF', 70);
  await call('Input.insertText', { text: '水🌊' });
  await delay(250);
  await key('F3', 'F3', 114, 8); // backwards wraps from first to last
  await key('F3', 'F3', 114, 0); // forward wraps to first
  await key('F3', 'F3', 114, 0); // second occurrence
  await screenshot('search-unicode');
  await key('Enter', 'Enter', 13, 0); // wraps to first, keeping the search focused
  await key('Escape', 'Escape', 27, 0);
  await call('Input.insertText', { text: 'orbit' });
  await delay(200);
  assert.equal(await download('search.eod'), searchable.replace('é 水🌊 first', 'é orbit first'),
    'search navigation must select exact Unicode text and return editor focus on Escape');
  await key('f', 'KeyF', 70);
  await call('Input.insertText', { text: 'orbit' });
  await delay(250);
  await key('Enter', 'Enter', 13, 8);
  await key('Escape', 'Escape', 27, 0);
  await call('Input.insertText', { text: 'phase' });
  await delay(200);
  assert.equal(await download('search.eod'), searchable
    .replace('é 水🌊 first', 'é orbit first').replace('local orbit', 'local phase'),
    'reopening Find selects the previous query; Shift+Enter navigates backwards');
  await importFile('invalid.eod');
  await screenshot('syntax-before-run');
  assert.equal(await download('invalid.eod'), invalid, 'Unicode and CRLF must survive import/export');
  await key('F8', 'F8', 119, 0);
  await call('Input.insertText', { text: '-- ' });
  await delay(350);
  assert.equal(await download('invalid.eod'), invalid.replace('local value', '-- local value'),
    'F8 must jump to the diagnostic line using characters, not UTF-8 byte offsets');
  await importFile('valid.eod');
  assert.equal(await download('valid.eod'), valid);
  await evaluate(`
    window.__originalArrayBuffer = Blob.prototype.arrayBuffer;
    Blob.prototype.arrayBuffer = function() {
      return new Promise(resolve => { window.__releaseImport = () => window.__originalArrayBuffer.call(this).then(resolve); });
    };
  `);
  await importFile('invalid.eod');
  await until(() => evaluate("typeof window.__releaseImport === 'function'"), 'delayed file read');
  await click(240, 130);
  await key('a', 'KeyA', 65);
  const edited = 'local edited_while_reading = 42';
  await call('Input.insertText', { text: edited });
  await delay(200);
  await evaluate('window.__releaseImport(); Blob.prototype.arrayBuffer = window.__originalArrayBuffer; true');
  await delay(450);
  assert.equal(await download('valid.eod'), edited, 'a late import must not replace a newer edit');
  await importFile('oversized.eod');
  assert.equal(await download('valid.eod'), edited, 'an oversized import must leave the buffer intact');
  assert.equal(await evaluate('window.__apteronotusContexts.length'), 0, 'file operations and syntax checks must never start audio');
  await importFile('valid.eod');
  await key('Enter', 'Enter', 13);
  await until(() => evaluate("window.__apteronotusContexts.some(context => context.state === 'running')"), 'Run starts audio');
  await screenshot('playing-imported-source');
  const contexts = await evaluate('window.__apteronotusContexts.length');
  await click(240, 130);
  await key('a', 'KeyA', 65);
  await call('Input.insertText', { text: "local broken = '水' + )" });
  await delay(500);
  assert.equal(await evaluate('window.__apteronotusContexts.length'), contexts, 'typing must not recreate audio');
  assert.equal(await evaluate("window.__apteronotusContexts.some(context => context.state === 'running')"), true);
  await screenshot('syntax-over-playing-source');
  await key('.', 'Period', 190);
  assert.deepEqual(exceptions, [], 'browser reported an uncaught exception');
  console.log(`Browser smoke passed: Unicode search/navigation, import/export, exact UTF-8/CRLF, stale/oversized import protection, syntax without evaluation, and explicit Run.\nArtifacts: ${runDirectory}`);
} finally {
  ws?.close();
  browser?.kill();
  server.close();
  await fs.writeFile(path.join(runDirectory, 'browser.log'), browserLog);
}
