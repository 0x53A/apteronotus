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
const midiStress = !!process.env.APTERONOTUS_MIDI_STRESS;
const midiOnly = !!process.env.APTERONOTUS_MIDI_ONLY || midiStress;
const hardwareGpu = midiStress && !!process.env.APTERONOTUS_MIDI_STRESS_HARDWARE_GPU;
const audioIsolation = midiStress && !!process.env.APTERONOTUS_MIDI_STRESS_AUDIO_ISOLATION;
const stressProfile = audioIsolation ? 'audio-isolation: WebGL draws disabled, UI/MIDI pumping retained'
  : hardwareGpu ? 'normal UI: hardware GPU requested' : 'normal UI: forced SwiftShader';
const stressSource = midiStress ? await fs.readFile(path.join(root, 'crates/test-fixtures/midi-load.eod'), 'utf8') : null;
const profiledWasmBytes = (await fs.stat(path.join(webRoot, 'pkg/apteronotus_app_bg.wasm'))).size;
await fs.mkdir(output, { recursive: true });
const runDirectory = await fs.mkdtemp(path.join(output, 'run-'));
if (midiStress) await fs.writeFile(path.join(runDirectory, 'midi-stress.eod'), stressSource);
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
let browser, ws, browserWs, browserLog = '';
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
    ...(hardwareGpu ? ['--enable-gpu', '--ignore-gpu-blocklist']
      : ['--use-gl=angle', '--use-angle=swiftshader', '--enable-unsafe-swiftshader']),
    'about:blank',
  ], { stdio: ['ignore', 'ignore', 'pipe'] });
  let spawnError;
  browser.on('error', error => { spawnError = error; });
  browser.stderr.on('data', chunk => { browserLog += chunk; });
  const port = await until(async () => {
    if (spawnError) throw spawnError;
    try { return Number((await fs.readFile(path.join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0]); }
    catch { return false; }
  }, 'Chromium debugging port');
  let callBrowser;
  if (midiStress) {
    const browserPath = (await fs.readFile(path.join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[1].trim();
    browserWs = new WebSocket(`ws://127.0.0.1:${port}${browserPath}`);
    await new Promise(resolve => browserWs.addEventListener('open', resolve, { once: true }));
    let browserSequence = 0;
    const browserPending = new Map();
    browserWs.addEventListener('message', event => {
      const message = JSON.parse(event.data), request = browserPending.get(message.id);
      if (!request) return;
      browserPending.delete(message.id); clearTimeout(request.timeout);
      if (message.error) request.reject(Error(JSON.stringify(message.error)));
      else request.resolve(message.result);
    });
    callBrowser = (method, params = {}) => new Promise((resolve, reject) => {
      const id = ++browserSequence;
      const timeout = setTimeout(() => { browserPending.delete(id); reject(Error(`Browser CDP timeout: ${method}`)); }, 15000);
      browserPending.set(id, { resolve, reject, timeout });
      browserWs.send(JSON.stringify({ id, method, params }));
    });
  }
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  ws = new WebSocket(targets.find(target => target.type === 'page').webSocketDebuggerUrl);
  await new Promise(resolve => ws.addEventListener('open', resolve, { once: true }));
  let sequence = 0;
  const pending = new Map(), exceptions = [], browserMessages = [];
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
    else if (message.method === 'Log.entryAdded') browserMessages.push(message.params.entry.text);
    else if (message.method === 'Page.fileChooserOpened') fileChoosers++;
  });
  const call = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++sequence;
    const timeout = setTimeout(() => { pending.delete(id); reject(Error(`CDP timeout: ${method}`)); }, midiStress ? 60000 : 30000);
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
    try {
      await until(() => fileChoosers > before, 'import command opens the file picker');
    } catch (error) {
      await screenshot('import-picker-failed');
      throw Error(`${error.message}: ${JSON.stringify(browserMessages)}`);
    }
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
  await call('Log.enable');
  await call('Emulation.setDeviceMetricsOverride', { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
  await call('Page.setDownloadBehavior', { behavior: 'allow', downloadPath: downloads });
  await call('Page.addScriptToEvaluateOnNewDocument', { source: `
    window.__apteronotusContexts = [];
    window.__hostErrors = [];
    window.__copiedLinks = [];
    Object.defineProperty(navigator, 'clipboard', {value: {
      writeText(text) { window.__copiedLinks.push(text); return Promise.resolve(); },
    }});
    // Exercise the real Rust Web MIDI bindings and UI without depending on
    // attached hardware or a permission dialog in an unattended browser.
    window.__midiRequests = [];
    window.__midiInputs = [];
    window.MIDIInput = class {
      constructor(id, name) {
        this.id = id; this.name = name; this.state = 'connected';
        this.onmidimessage = null; this.opens = 0; this.closes = 0;
      }
      open() { this.opens++; return Promise.resolve(this); }
      close() { this.closes++; return Promise.resolve(this); }
      emit(data) { this.onmidimessage?.(new MIDIMessageEvent('midimessage', { data: new Uint8Array(data) })); }
    };
    window.MIDIAccess = class {
      constructor() { this.inputs = new Map(window.__midiInputs.map(input => [input.id, input])); }
    };
    window.__midiInputs.push(new MIDIInput('keyboard-a', 'Keyboard A'), new MIDIInput('keyboard-b', 'Keyboard B'));
    Object.defineProperty(navigator, 'requestMIDIAccess', { value: options => {
      window.__midiRequests.push(options);
      return Promise.resolve(new MIDIAccess());
    } });
    // Observe production audio buffers at the browser boundary. This measures
    // the actual rendered output rather than a test-only engine counter.
    window.__audioRms = [];
    window.__stress = { recording: false, callbacks: [], uiCallbacks: [], buffers: [], frames: [], longTasks: [] };
    if (${midiStress}) {
      const originalListen = AudioBufferSourceNode.prototype.addEventListener;
      AudioBufferSourceNode.prototype.addEventListener = function(type, listener, ...options) {
        if (type === 'ended' && typeof listener === 'function') {
          const original = listener;
          listener = function(...args) {
            const started = performance.now();
            try { return original.apply(this, args); }
            finally { if (window.__stress.recording) window.__stress.callbacks.push(performance.now() - started); }
          };
        }
        return originalListen.call(this, type, listener, ...options);
      };
      const originalRaf = window.requestAnimationFrame.bind(window);
      window.requestAnimationFrame = callback => originalRaf(now => {
        const started = performance.now();
        try { return callback(now); }
        finally { if (window.__stress.recording) window.__stress.uiCallbacks.push(performance.now() - started); }
      });
      let previousFrame = performance.now();
      const frame = now => {
        if (window.__stress.recording) window.__stress.frames.push(now - previousFrame);
        previousFrame = now; originalRaf(frame);
      };
      originalRaf(frame);
      new PerformanceObserver(list => {
        if (window.__stress.recording) window.__stress.longTasks.push(...list.getEntries().map(entry => entry.duration));
      }).observe({ type: 'longtask', buffered: false });
    }
    const originalStart = AudioBufferSourceNode.prototype.start;
    AudioBufferSourceNode.prototype.start = function(...args) {
      if (this.buffer) {
        const samples = this.buffer.getChannelData(0);
        let energy = 0;
        for (const sample of samples) energy += sample * sample;
        window.__audioRms.push(Math.sqrt(energy / samples.length));
        if (window.__stress.recording) window.__stress.buffers.push({
          durationMs: this.buffer.duration * 1000,
          scheduledMarginMs: ((args[0] || 0) - this.context.currentTime) * 1000,
          rms: Math.sqrt(energy / samples.length),
          at: performance.now(),
        });
        if (window.__audioRms.length > 100) window.__audioRms.shift();
      }
      return originalStart.apply(this, args);
    };
    document.addEventListener('DOMContentLoaded', () => {
      const player = document.querySelector('apteronotus-app');
      player.addEventListener('apteronotus-error', event => window.__hostErrors.push(event.detail));
      player.setAttribute('filename', 'hosted.eod');
      if (!location.hash.startsWith('#library=')) player.setAttribute('source', ${JSON.stringify(searchable)});
    }, { once: true });
    const OriginalContext = window.AudioContext;
    window.AudioContext = class extends OriginalContext {
      constructor(...args) { super(...args); window.__apteronotusContexts.push(this); }
    };
  ` });
  await call('Page.navigate', { url: `http://127.0.0.1:${server.address().port}/#library=three-notes` });
  await until(() => evaluate("document.querySelector('apteronotus-app')?.shadowRoot?.querySelector('canvas')?.width === 1280"), 'canvas', 60000);
  await delay(600);
  assert.equal(await evaluate('window.__midiRequests.length'), 0, 'page loading must never request MIDI permission');
  if (!midiStress) {
  // Hash navigation opens library entries without activation or permission.
  await evaluate("location.hash = 'library=three-notes'; true");
  await delay(300);
  assert.equal(await download('three-notes.eod'), await fs.readFile(path.join(root, 'crates/app/assets/examples/three-notes.eod'), 'utf8'));
  assert.equal(await evaluate('window.__midiRequests.length'), 0);
  assert.equal(await evaluate('window.__apteronotusContexts.length'), 0, 'opening a library link must not open audio');
  await click(345, 20);
  await screenshot('library-groups');
  await click(455, 53);
  await until(() => evaluate('window.__copiedLinks.length === 1'), 'copying the library entry link');
  assert.equal(await evaluate('window.__copiedLinks[0]'), `http://127.0.0.1:${server.address().port}/#library=three-notes`);
  await click(800, 300);
  await evaluate("location.hash = 'library=unknown'; true");
  await delay(200);
  assert.equal(await download('three-notes.eod'), await fs.readFile(path.join(root, 'crates/app/assets/examples/three-notes.eod'), 'utf8'));
  await evaluate("location.hash = 'library=live-controls'; true");
  await delay(300);
  assert.equal(await download('live-controls.eod'), await fs.readFile(path.join(root, 'crates/app/assets/examples/live-controls.eod'), 'utf8'));
  await evaluate('history.back(); true');
  await delay(250);
  assert.equal(await download('live-controls.eod'), await fs.readFile(path.join(root, 'crates/app/assets/examples/live-controls.eod'), 'utf8'), 'unknown history entries leave the document alone');
  await evaluate('history.back(); true');
  await delay(250);
  assert.equal(await download('three-notes.eod'), await fs.readFile(path.join(root, 'crates/app/assets/examples/three-notes.eod'), 'utf8'), 'Back restores the linked demo');
  }
  await evaluate(`document.querySelector('apteronotus-app').setAttribute('source', ${JSON.stringify(searchable)}); true`);
  await delay(300);
  if (!midiOnly) {
  assert.equal(await download('hosted.eod'), searchable, 'pre-mount host source preserves Unicode and CRLF');
  await evaluate(`document.querySelector('apteronotus-app').setAttribute('source', ${JSON.stringify(valid)}); true`);
  await delay(300);
  assert.equal(await download('hosted.eod'), valid, 'host can load a new source after mounting');
  await evaluate("document.querySelector('apteronotus-app').setAttribute('source', 'x'.repeat(1024 * 1024 + 1)); true");
  await until(() => evaluate('window.__hostErrors.length === 1'), 'oversized host source emits a diagnostic');
  await delay(300);
  assert.equal(await download('hosted.eod'), valid, 'oversized host source leaves the editor intact');
  await evaluate(`(() => {
    const player = document.querySelector('apteronotus-app');
    player.addEventListener('apteronotus-error', () => {
      player.setAttribute('source', ${JSON.stringify(invalid)});
    }, { once: true });
    player.setAttribute('source', 'x'.repeat(1024 * 1024 + 1));
    return true;
  })()`);
  await until(() => evaluate('window.__hostErrors.length === 2'), 'host error handler corrects source');
  await delay(300);
  assert.equal(await download('hosted.eod'), invalid, 'error handler can synchronously replace source');
  assert.equal(await evaluate('window.__apteronotusContexts.length'), 0, 'host loading never starts audio');
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
  await evaluate(`document.querySelector('apteronotus-app').setAttribute('source', ${JSON.stringify(invalid)}); true`);
  await delay(400);
  assert.equal(await evaluate('window.__apteronotusContexts.length'), contexts, 'host replacement never evaluates or recreates audio');
  assert.equal(await evaluate("window.__apteronotusContexts.some(context => context.state === 'running')"), true, 'host loading keeps the active program playing');
  await key('.', 'Period', 190);
  await until(() => evaluate("window.__apteronotusContexts.every(context => context.state !== 'running')"), 'Stop closes audio');
  await evaluate(`document.querySelector('apteronotus-app').setAttribute('source', ${JSON.stringify(valid)}); true`);
  await delay(300);
  await key('Enter', 'Enter', 13);
  await until(() => evaluate("window.__apteronotusContexts.some(context => context.state === 'running')"), 'Run after Stop starts audio');
  }
  const midiSource = 'local expression = control {name="Expression", range={0,1}, default=1}\nmidi(expression, {cc=82, channel=1})\nlocal v = voice {graph = function(n) return sine(n.hz) * 0.1 * expression * adsr(0.01, 0.03, 1, 0.05) >> pan(0) end}\nmidi(v)\n';
  await evaluate(`document.querySelector('apteronotus-app').setAttribute('source', ${JSON.stringify(midiSource)}); true`);
  await delay(300);
  await key('Enter', 'Enter', 13);
  await delay(1000);
  assert.equal(await evaluate('window.__midiRequests.length'), 0, 'Run must never request MIDI permission');
  const silent = async label => {
    await evaluate('window.__audioRms = []; true');
    await until(() => evaluate('window.__audioRms.length >= 3 && window.__audioRms.slice(-3).every(rms => rms < 0.000001)'), label);
  };
  await click(415, 20);
  await screenshot('midi-before-enable');
  if (!midiStress) {
    await click(460, 280); // Computer keyboard, available before Web MIDI permission.
    await click(800, 300); // Close settings.
    await click(100, 54); // Focus Play computer keyboard.
    await screenshot('computer-keyboard');
    const computerKey = async (key, code, windowsVirtualKeyCode, type, extra = {}) => {
      await call('Input.dispatchKeyEvent', { type, key, code, windowsVirtualKeyCode, modifiers: 0, ...extra });
      await delay(100);
    };
    const computerOn = async () => {
      await evaluate('window.__audioRms = []; true');
      await computerKey('a', 'KeyA', 65, 'keyDown');
      try {
        await until(() => evaluate('window.__audioRms.some(rms => rms > 0.01)'), 'computer key produces production audio without MIDI permission');
      } catch (error) {
        await screenshot('computer-keyboard-failed');
        throw error;
      }
    };
    await computerOn();
    await computerKey('a', 'KeyA', 65, 'keyDown', { autoRepeat: true });
    await computerKey('a', 'KeyA', 65, 'keyUp');
    await silent('one computer key-up releases an auto-repeated key');
    await computerOn();
    await computerKey('d', 'KeyD', 68, 'keyDown');
    await computerKey('g', 'KeyG', 71, 'keyDown');
    await computerKey('a', 'KeyA', 65, 'keyUp');
    assert.ok(await evaluate('window.__audioRms.slice(-3).some(rms => rms > 0.01)'), 'releasing one computer key leaves the chord sounding');
    await key('Escape', 'Escape', 27, 0);
    await silent('Escape releases a computer keyboard chord');
    await computerKey('d', 'KeyD', 68, 'keyUp');
    await computerKey('g', 'KeyG', 71, 'keyUp');
    await click(100, 54);
    await computerOn();
    await click(800, 300);
    await silent('returning to the editor releases held computer keys');
    await computerKey('a', 'KeyA', 65, 'keyUp');
    // An actual text insertion in the editor must stay silent.
    await call('Input.dispatchKeyEvent', { type: 'keyDown', key: 'a', code: 'KeyA', windowsVirtualKeyCode: 65, text: 'a', modifiers: 0 });
    await computerKey('a', 'KeyA', 65, 'keyUp');
    await silent('typing into the editor never plays the computer keyboard');
    await key('z', 'KeyZ', 90); // Undo the inserted text.
    await delay(400); // Allow compile-only diagnostics to settle before locating the panel.
    await click(100, 54);
    await computerOn();
    await evaluate("(() => { const input = document.createElement('input'); input.id = 'outside-player'; document.body.append(input); input.focus(); return true; })()");
    await silent('focus leaving the component releases computer keys');
    await evaluate("document.querySelector('#outside-player').remove(); true");
    await computerKey('a', 'KeyA', 65, 'keyUp');
    assert.equal(await evaluate('window.__midiRequests.length'), 0, 'computer keyboard never requests Web MIDI access');
    assert.equal(await download('hosted.eod'), midiSource, 'playing computer keys leaves the source intact');
    await click(415, 20);
    await click(460, 280); // Disable the computer keyboard.
    await click(800, 300);
    await click(415, 20);
  }
  await click(465, 53);
  await until(() => evaluate('window.__midiRequests.length === 1'), 'explicit Enable MIDI requests permission');
  assert.equal(await evaluate('window.__midiRequests[0].sysex'), false, 'keys do not request SysEx');
  await click(415, 20);
  await screenshot('midi-enabled');
  await click(440, 132);
  await until(() => evaluate('window.__midiInputs[0].opens === 1 && typeof window.__midiInputs[0].onmidimessage === "function"'), 'selecting keyboard attaches its callback');
  await click(800, 300);
  const noteOn = async index => {
    await evaluate(`window.__audioRms = []; window.__midiInputs[${index}].emit([0x90, 60, 100]); true`);
    await until(() => evaluate('window.__audioRms.some(rms => rms > 0.01)'), 'MIDI note-on produces audible production output');
  };
  if (midiStress) {
    await evaluate(`document.querySelector('apteronotus-app').setAttribute('source', ${JSON.stringify(stressSource)}); true`);
    await delay(300);
    await key('Enter', 'Enter', 13);
    await delay(1500);
    const renderer = await evaluate(`(() => {
      const canvas = document.querySelector('apteronotus-app').shadowRoot.querySelector('canvas');
      const gl = canvas.getContext('webgl2') || canvas.getContext('webgl');
      if (!gl) return { unavailable: true };
      const extension = gl.getExtension('WEBGL_debug_renderer_info');
      return {
        vendor: gl.getParameter(gl.VENDOR), renderer: gl.getParameter(gl.RENDERER),
        unmaskedVendor: extension ? gl.getParameter(extension.UNMASKED_VENDOR_WEBGL) : null,
        unmaskedRenderer: extension ? gl.getParameter(extension.UNMASKED_RENDERER_WEBGL) : null,
      };
    })()`);
    const gpu = await callBrowser('SystemInfo.getInfo').then(info => info.gpu)
      .catch(error => ({ unavailable: error.message }));
    if (audioIsolation) {
      // Preserve eframe's animation callbacks, document state and MIDI/audio
      // polling. Only actual GPU draws are suppressed after UI setup.
      await evaluate(`(() => {
        window.__isolatedDrawCalls = 0;
        for (const Context of [window.WebGLRenderingContext, window.WebGL2RenderingContext]) {
          if (!Context) continue;
          for (const method of ['drawArrays', 'drawElements', 'drawArraysInstanced', 'drawElementsInstanced', 'drawRangeElements']) {
            if (typeof Context.prototype[method] === 'function') Context.prototype[method] = function() { window.__isolatedDrawCalls++; };
          }
        }
        return true;
      })()`);
    }
    const metadata = { source: 'crates/test-fixtures/midi-load.eod', wasmBytes: profiledWasmBytes,
      profile: stressProfile, hardwareGpuRequested: hardwareGpu, webGlDrawsDisabled: audioIsolation, renderer, gpu };
    console.log(JSON.stringify({ stressMetadata: { profile: stressProfile,
      wasmBytes: profiledWasmBytes, renderer, devices: gpu.devices,
      gpuRenderer: gpu.auxAttributes?.glRenderer } }));
    const processSnapshot = async () => ({ at: performance.now(),
      processes: await callBrowser('SystemInfo.getProcessInfo').then(info => info.processInfo)
        .catch(error => [{ unavailable: error.message }]) });
    let cpuStart;
    const results = [];
    const summarize = values => {
      if (!values.length) return { count: 0 };
      values.sort((a, b) => a - b);
      const percentile = p => values[Math.min(values.length - 1, Math.floor(p * values.length))];
      return { count: values.length, median: percentile(.5), p95: percentile(.95), max: values.at(-1) };
    };
    const report = async (keys, label) => {
      const measurements = await evaluate('window.__stress.recording = false; JSON.parse(JSON.stringify(window.__stress))');
      const buffers = measurements.buffers;
      const result = {
        label, keys,
        callbackMs: summarize(measurements.callbacks),
        uiCallbackMs: summarize(measurements.uiCallbacks),
        audioBufferMs: buffers[0]?.durationMs,
        scheduledMarginMs: summarize(buffers.map(buffer => buffer.scheduledMarginMs)),
        lateBuffers: buffers.filter(buffer => buffer.scheduledMarginMs < 0).length,
        firstNonzeroBufferMs: keys === 1 ? buffers.find(buffer => buffer.rms > .00001)?.at - measurements.emittedAt : undefined,
        frameGapMs: summarize(measurements.frames),
        longTaskMs: summarize(measurements.longTasks),
      };
      if (cpuStart) {
        const end = await processSnapshot(), wallSeconds = (end.at - cpuStart.at) / 1000;
        result.processCpu = end.processes.map(process => {
          const previous = cpuStart.processes.find(before => before.id === process.id);
          return { type: process.type, pid: process.id,
            percent: previous && Number.isFinite(process.cpuTime) ? 100 * (process.cpuTime - previous.cpuTime) / wallSeconds : null };
        });
        cpuStart = null;
      }
      if (audioIsolation) result.suppressedDrawCalls = await evaluate('window.__isolatedDrawCalls');
      results.push(result); console.log(JSON.stringify(result));
      await fs.writeFile(path.join(runDirectory, 'midi-stress.json'), JSON.stringify({
        ...metadata, results,
      }, null, 2));
    };
    const begin = async () => evaluate('window.__stress = { recording: true, callbacks: [], uiCallbacks: [], buffers: [], frames: [], longTasks: [], emittedAt: performance.now() }; true');
    for (const count of [1, 4, 8, 16, 32, 100]) {
      if (count === 100) cpuStart = await processSnapshot();
      await begin();
      await evaluate(`for (let key = 24; key < ${24 + count}; key++) window.__midiInputs[0].emit([0x90, key, 100]); true`);
      await delay(2500);
      await report(count, 'held');
      await evaluate('window.__midiInputs[0].emit([0xb0, 123, 0]); true');
      await delay(1000);
    }
    await begin();
    await evaluate(`(async () => {
      for (let repeat = 0; repeat < 6; repeat++) {
        for (let key = 24; key < 124; key++) window.__midiInputs[0].emit([0x90, key, 100]);
        await new Promise(resolve => setTimeout(resolve, 80));
        for (let key = 24; key < 124; key++) window.__midiInputs[0].emit([0x80, key, 0]);
        await new Promise(resolve => setTimeout(resolve, 80));
      }
    })()`);
    await delay(1000);
    await report(100, 'six retrigger bursts, 80 ms gates/rests');
    await fs.writeFile(path.join(runDirectory, 'midi-stress.json'), JSON.stringify({
      ...metadata,
      results,
    }, null, 2));
  } else {
  await silent('MIDI-only document is silent before keys');
  await noteOn(0);
  await evaluate('window.__midiInputs[0].emit([0xb1, 82, 0]); true');
  await delay(200);
  assert.ok(await evaluate('window.__audioRms.slice(-3).some(rms => rms > 0.01)'), 'CC on another channel leaves a held note sounding');
  await evaluate('window.__midiInputs[0].emit([0xb0, 82, 0]); true');
  await silent('mapped fader mutes an already held voice');
  await evaluate('window.__audioRms = []; window.__midiInputs[0].emit([0xb0, 82, 127]); true');
  await until(() => evaluate('window.__audioRms.some(rms => rms > 0.01)'), 'mapped fader restores an already held voice');
  await screenshot('midi-cc-fader');
  await evaluate('window.__midiInputs[0].emit([0x80, 60, 0]); true');
  await silent('MIDI note-off releases the rendered voice');
  // CPAL Web Audio advances the DSP from AudioBufferSourceNode ended events.
  // A suspended context therefore freezes its sample clock while wall time
  // keeps advancing. Wall-clock MIDI placement would insert seconds of silence.
  await evaluate("Promise.all(window.__apteronotusContexts.filter(context => context.state === 'running').map(context => context.suspend()))");
  await delay(300); // Let any already queued ended callback finish.
  await evaluate('window.__audioRms = []; true');
  await delay(3500);
  assert.equal(await evaluate('window.__audioRms.length'), 0,
    'suspending Web Audio must freeze production rendering for the clock regression');
  await evaluate("Promise.all(window.__apteronotusContexts.filter(context => context.state === 'suspended').map(context => context.resume()))");
  await evaluate('window.__audioRms = []; window.__midiInputs[0].emit([0x90, 60, 100]); true');
  await until(() => evaluate('window.__audioRms.some(rms => rms > 0.01)'),
    'MIDI onset after a suspended audio clock is rendered promptly', 1000);
  await evaluate('window.__midiInputs[0].emit([0x80, 60, 0]); true');
  await silent('MIDI release after suspension stays on the rendered clock');
  await noteOn(0);
  await click(415, 20);
  await screenshot('midi-selected');
  await click(440, 160);
  try {
    await until(() => evaluate('window.__midiInputs[0].onmidimessage == null && window.__midiInputs[0].closes === 1 && window.__midiInputs[1].opens === 1'), 'switching keyboards clears and closes the old input');
  } catch (error) {
    await screenshot('midi-switch-failed');
    throw Error(`${error.message}: ${JSON.stringify(await evaluate('window.__midiInputs.map(input => ({ id: input.id, opens: input.opens, closes: input.closes, handler: typeof input.onmidimessage }))'))}`);
  }
  await click(800, 300);
  await silent('switching keyboards releases held keys');
  await noteOn(1);
  await click(415, 20);
  await click(460, 279);
  await click(800, 300);
  await silent('Release all MIDI keys button releases a held voice');
  await noteOn(1);
  await evaluate("window.__midiInputs[1].state = 'disconnected'; true");
  await until(() => evaluate('window.__midiInputs[1].onmidimessage == null && window.__midiInputs[1].closes === 1'), 'unplugged keyboard callback is removed');
  await silent('unplugging releases held keys');
  }
  await evaluate(`(() => {
    const player = document.querySelector('apteronotus-app');
    player.addEventListener('apteronotus-error', () => player.remove(), { once: true });
    player.setAttribute('source', 'x'.repeat(1024 * 1024 + 1));
    return true;
  })()`);
  await until(() => evaluate("!document.querySelector('apteronotus-app')"), 'host error handler removes component');
  await until(() => evaluate("window.__apteronotusContexts.every(context => context.state !== 'running')"), 'removing the component stops playback');
  assert.deepEqual(exceptions, [], 'browser reported an uncaught exception');
  const scope = midiStress ? 'Web MIDI stress measurements complete' : midiOnly ? 'Web MIDI passed' : 'Browser smoke passed';
  console.log(`${scope}.\nArtifacts: ${runDirectory}`);
} finally {
  ws?.close();
  browserWs?.close();
  browser?.kill();
  server.close();
  await fs.writeFile(path.join(runDirectory, 'browser.log'), browserLog);
}
