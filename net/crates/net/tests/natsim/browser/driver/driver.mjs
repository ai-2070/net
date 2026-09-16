// The browser-control plane for ONE namespace of the NAT conformance
// matrix.
//
// One Node process per NAT'd namespace, launched by the runner as
// `ip netns exec nsim_a node driver.mjs`. Everything below it — this
// process, Playwright, the browser, every UDP socket ICE opens — is
// therefore inside that namespace and behind that namespace's
// simulated NAT, while the NDJSON control channel stays on the stdio
// pipes the runner already holds. That is the whole reason the drivers
// are spawned per namespace instead of one driver launching two
// browsers with `executablePath` wrappers: a wrapper puts the BROWSER
// in the namespace but leaves Playwright's own sockets outside, and
// every future "why is this page reaching something it should not"
// becomes a question about the wrapper.
//
// Protocol: one JSON request per line on stdin, one JSON response per
// line on stdout, correlated by `id`; everything else goes to stderr,
// which the runner forwards into the scenario log.
//
//   {"id":1,"op":"launch","engine":"chromium","spkiPin":"…","caPem":"…","profileDir":"…"}
//   {"id":2,"op":"open","url":"http://localhost:8080/?tab=a&control=…"}
//   {"id":3,"op":"shutdown"}
//
// TLS: there is no `ignoreHTTPSErrors` here and there cannot be. The
// anchor's listener serves a leaf issued by the run's own CA, and each
// engine is told about that key the narrowest prompt-free way it
// offers — Chromium a one-key SPKI pin, Firefox the CA in the NSS
// database inside its own launch profile. Verification stays on.

import { chromium, firefox } from 'playwright-core';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import readline from 'node:readline';

const ENGINES = { chromium, firefox };
const NSS_NICKNAME = 'net-mesh-natsim-harness-ca';

let live = null;
let page = null;

function log(line) {
  process.stderr.write(line + '\n');
}

function reply(obj) {
  process.stdout.write(JSON.stringify(obj) + '\n');
}

// Firefox reads its OWN NSS database (`cert9.db`) from inside the
// profile, so the profile must be seeded BEFORE launch and the launch
// must be persistent. `certutil` is the same NSS tool the Stage 4b
// harness uses on Linux; a run that cannot seed the profile is
// REFUSED, never weakened — an unverified TLS leg would prove
// something no deployment can rely on.
function seedFirefoxProfile(profileDir, caPemPath) {
  fs.mkdirSync(profileDir, { recursive: true });
  execFileSync('certutil', [
    '-d',
    `sql:${profileDir}`,
    '-A',
    '-t',
    'C,,',
    '-n',
    NSS_NICKNAME,
    '-i',
    caPemPath,
  ]);
  return `certutil -d sql:${profileDir}`;
}

function chromiumArgs(spkiPin) {
  return [
    // The netns rows run as root (netns + nft need it), and Chromium
    // refuses its sandbox as root. This is a throwaway browser in a
    // throwaway namespace; the alternative is no row at all.
    '--no-sandbox',
    '--disable-dev-shm-usage',
    // One key, this process, this run. NOT
    // `--ignore-certificate-errors`: every other certificate is still
    // verified.
    `--ignore-certificate-errors-spki-list=${spkiPin}`,
    // mDNS host-candidate obfuscation is left ON — production cannot
    // turn it off, and the two browsers sit on DIFFERENT private
    // subnets here, so a host candidate could not have connected them
    // anyway. Every direct pair in this matrix is reached through the
    // anchor's STUN (server-reflexive) or peer-reflexively, which is
    // exactly what the NAT flavors are being tested against.
    // NOTE for whoever closes the Chromium gap (S6_REPORT.md §6.12):
    // the engine's own per-check account is the one measurement left,
    // and TWO ways of getting it failed here. `browser.process()` does
    // not exist on Playwright's `Browser`, so draining its stderr
    // took the launch down with it; `--enable-logging
    // --log-file=<path> --vmodule=…` then produced no file at all
    // under headless Chromium. Removed rather than left in place: a
    // flag that reports nothing while appearing to work is the exact
    // failure this stage documented five times. `launchPersistentContext`
    // with `--user-data-dir`, or a `chrome://webrtc-internals` dump
    // from a headed run, are the untried paths.
  ];
}

async function opLaunch(req) {
  const engine = ENGINES[req.engine];
  if (!engine) throw new Error(`unknown engine: ${req.engine}`);
  let trust;
  if (req.engine === 'firefox') {
    const caPemPath = path.join(req.profileDir, 'harness-ca.pem');
    fs.mkdirSync(req.profileDir, { recursive: true });
    fs.writeFileSync(caPemPath, req.caPem);
    trust = seedFirefoxProfile(req.profileDir, caPemPath);
    const context = await firefox.launchPersistentContext(req.profileDir, {
      headless: true,
    });
    live = { engine: req.engine, context, browser: null, persistent: true };
  } else {
    const browser = await engine.launch({
      headless: true,
      chromiumSandbox: false,
      args: chromiumArgs(req.spkiPin),
    });
    const context = await browser.newContext();
    live = { engine: req.engine, context, browser, persistent: false };
    trust = 'spki-pin';
  }
  return { engine: req.engine, trust };
}

async function opOpen(req) {
  if (!live) throw new Error('open before launch');
  page = await live.context.newPage();
  page.on('console', (m) => log(`[page] ${m.type()}: ${m.text()}`));
  page.on('pageerror', (e) => log(`[page] ERROR ${e && e.message}`));
  // `domcontentloaded`, not `load`: the page's own work starts on
  // module evaluation and the runner drives it over HTTP from there.
  // Waiting for `load` would block on whatever the page is already
  // doing.
  await page.goto(req.url, { waitUntil: 'domcontentloaded', timeout: 60_000 });
  return { url: page.url() };
}

async function opShutdown() {
  try {
    if (live && live.persistent) await live.context.close();
    else if (live && live.browser) await live.browser.close();
  } catch (e) {
    log(`shutdown: ${e && e.message}`);
  }
  live = null;
  page = null;
  return {};
}

const OPS = { launch: opLaunch, open: opOpen, shutdown: opShutdown };

// Requests are served strictly in order: the runner issues one at a
// time, and an `open` that raced a `launch` would report a page on no
// browser at all.
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
      reply({ id: req.id, ok: false, error: `unknown op: ${req.op}` });
      return;
    }
    try {
      const out = await op(req);
      reply({ id: req.id, ok: true, ...out });
    } catch (e) {
      reply({ id: req.id, ok: false, error: (e && (e.message || String(e))) || 'unknown' });
    }
  });
});

rl.on('close', async () => {
  await queue;
  await opShutdown();
  process.exit(0);
});

// The namespace this driver — and therefore its browser, and every
// UDP socket ICE opens — is actually in, stated as a FACT observed
// from inside it rather than as the name the runner passed. The whole
// premise of the matrix is that tab a sits behind gateway a and tab b
// behind gateway b; `192.168.101.x` vs `192.168.102.x` is what makes
// that checkable when a row fails asymmetrically.
const addrs = Object.entries(os.networkInterfaces())
  .flatMap(([dev, list]) =>
    (list || []).filter((a) => a.family === 'IPv4').map((a) => `${dev}=${a.address}`),
  )
  .join(' ');
log(`netns ${process.env.NATSIM_NETNS || '(unnamed)'} addrs ${addrs} home ${process.env.HOME}`);

reply({ id: 0, ok: true, hello: 'natsim browser driver', engines: Object.keys(ENGINES) });
