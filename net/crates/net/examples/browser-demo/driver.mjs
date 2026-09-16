// The demo's **browser-control plane** — the same shape as the merged
// runner's (`tests/rtc_browser/driver/driver.mjs`): one Node process,
// NDJSON on stdio, one JSON request per line in, one reply per line
// out, correlated by `id`. Everything else goes to stderr, which the
// host forwards as `[driver] …`.
//
// WHY A SECOND DRIVER AND NOT THE HARNESS'S
// -----------------------------------------
// The harness driver belongs to the witness harness: it launches ONE
// engine from a spec with Firefox profiles, WebKit, mDNS and
// UDP-blocked knobs, and a TLS control. The demo needs a strict
// subset (Chromium, two contexts, a page each) plus two things the
// harness must NOT have: a HEADED mode, and the three
// throttling-defeat flags a 60 Hz send loop needs in a window that
// may be occluded or backgrounded. Copying the harness driver's
// launch spec into a demo would couple the demo to a test harness's
// evolution, and adding a headed mode to the harness would put a
// browser window in front of whoever runs CI. So: one small driver,
// the same protocol, no shared file.
//
// TLS: there is no `ignoreHTTPSErrors` here and there cannot be. The
// host mints a CA and a leaf for `localhost` and passes
// `--ignore-certificate-errors-spki-list=<base64(SHA-256(SPKI))>` of
// THAT leaf key. Verification stays on, every other certificate is
// still verified, and nothing is written to any platform trust store
// — so running the demo by hand never raises a security dialog.

import { chromium } from 'playwright-core';
import readline from 'node:readline';

/** @type {{browser: any}|null} */
let live = null;
/** @type {Map<string, any>} */
const contexts = new Map();
/** @type {Map<string, any>} */
const pages = new Map();

function log(line) {
  process.stderr.write(line + '\n');
}

function reply(obj) {
  process.stdout.write(JSON.stringify(obj) + '\n');
}

// One place for the flags, so the headed and headless runs differ in
// EXACTLY one field.
//
// The throttling trio is load-bearing, not hygiene: Chromium clamps
// timers and skips `requestAnimationFrame` in a page it considers
// backgrounded or occluded, and BOTH demo windows are in that state
// for most of a headed run (one is behind the other) and arguably all
// of a headless one. Without them the measured send rate collapses to
// the background clamp and the demo would report a limit that belongs
// to the browser's power policy rather than to the mesh.
//
// SwiftShader is the same kind of flag for the renderer: a headless
// Chromium with no GPU has no hardware WebGL, and `WebGLRenderer`
// throws rather than degrading. The demo asserts that three.js
// actually rendered frames, so the software rasteriser has to be
// available — and `--enable-unsafe-swiftshader` is what makes it
// available in current Chromium instead of a console warning and a
// failed context.
function chromiumArgs(spkiPin) {
  const args = [
    '--no-sandbox',
    '--disable-background-timer-throttling',
    '--disable-renderer-backgrounding',
    '--disable-backgrounding-occluded-windows',
    '--use-gl=angle',
    '--use-angle=swiftshader',
    '--enable-unsafe-swiftshader',
    '--window-size=760,600',
  ];
  if (spkiPin) args.push(`--ignore-certificate-errors-spki-list=${spkiPin}`);
  return args;
}

async function opLaunch(req) {
  if (live) throw new Error('a browser is already live; shutdown first');
  const args = chromiumArgs(req.spkiPin);
  const browser = await chromium.launch({
    headless: !!req.headless,
    executablePath: req.executablePath || undefined,
    args,
  });
  live = { browser };
  return {
    version: browser.version(),
    headless: !!req.headless,
    trust: req.spkiPin
      ? `--ignore-certificate-errors-spki-list=${req.spkiPin} (this run's leaf SPKI only; ` +
        'verification stays on, no trust store is written)'
      : 'no pin passed — TLS to the host listener will be refused',
  };
}

// TWO ISOLATED BROWSING CONTEXTS, one per tab.
//
// Not two tabs in one context: §8 gives an origin ONE node, and two
// tabs on one origin share a storage partition and a Web Locks
// namespace — so they would share one leaf identity and contend for
// one leader election instead of being two peers. A separate context
// is what makes "two tabs exchanging positions" two nodes.
async function contextFor(name) {
  const existing = contexts.get(name);
  if (existing) return existing;
  if (!live) throw new Error('no browser is live');
  const context = await live.browser.newContext({ viewport: null });
  contexts.set(name, context);
  return context;
}

async function opOpen(req) {
  const context = await contextFor(req.context || 'default');
  const page = await context.newPage();
  page.on('console', (m) => log(`[${req.name}] ${m.type()}: ${m.text()}`));
  page.on('pageerror', (e) => log(`[${req.name}] pageerror: ${e.message}`));
  pages.set(req.name, page);
  await page.goto(req.url, { waitUntil: 'domcontentloaded', timeout: 60_000 });
  return { url: page.url() };
}

// Read one JSON value out of a page. The demo's assertions are made
// on the HOST against the anchor's own counters; this exists for the
// two facts only the page can report — the rate it achieved and
// whether three.js rendered — and it returns them verbatim rather
// than interpreting them.
async function opState(req) {
  const page = pages.get(req.name);
  if (!page) throw new Error(`no page named ${req.name}`);
  const state = await page.evaluate(() =>
    globalThis.__demo ? globalThis.__demo.state() : null,
  );
  return { state };
}

async function opShutdown() {
  for (const [, page] of pages) {
    try {
      await page.close();
    } catch {
      /* a page the browser already tore down */
    }
  }
  pages.clear();
  for (const [, context] of contexts) {
    try {
      await context.close();
    } catch {
      /* same */
    }
  }
  contexts.clear();
  if (live) {
    try {
      await live.browser.close();
    } catch {
      /* same */
    }
    live = null;
  }
  return { closed: true };
}

const OPS = {
  launch: opLaunch,
  open: opOpen,
  state: opState,
  shutdown: opShutdown,
};

// Requests are served STRICTLY IN ORDER: the host issues one at a
// time and an `open` that raced a `launch` would report a page on a
// browser that does not exist yet.
const rl = readline.createInterface({ input: process.stdin });
let queue = Promise.resolve();

rl.on('line', (line) => {
  if (!line.trim()) return;
  let req;
  try {
    req = JSON.parse(line);
  } catch (e) {
    log(`unparseable request: ${line}`);
    return;
  }
  queue = queue.then(async () => {
    const op = OPS[req.op];
    if (!op) {
      reply({ id: req.id, ok: false, error: `unknown op ${req.op}` });
      return;
    }
    try {
      const result = await op(req);
      reply({ id: req.id, ok: true, ...result });
    } catch (e) {
      reply({ id: req.id, ok: false, error: e && e.message ? e.message : String(e) });
    }
  });
});

rl.on('close', async () => {
  await opShutdown().catch(() => {});
  process.exit(0);
});

reply({ id: 0, ok: true, hello: 'net browser-demo driver' });
