// The merged browser runner's **browser-control plane**.
//
// One Node process, driven by `runner/src/browser.rs` over NDJSON on
// stdio: one JSON request per line on stdin, one JSON response per
// line on stdout, correlated by `id`. Everything else the driver
// prints goes to stderr, which the runner forwards as `[driver] …`.
//
// WHY PLAYWRIGHT, AND WHY ONLY HERE
// ---------------------------------
// Stage 4b launched a browser process itself (`launch_chromium`) and
// drove the page through an HTTP step protocol. The launch half was
// Chromium-only: a hard-coded `--headless=new`, `--user-data-dir`
// and an executable-path search. Playwright replaces exactly that
// half, and buys the three things Stage 5 needs and a raw process
// spawn cannot give:
//
//   * **engines.** chromium | firefox | webkit from one code path,
//     each with the right profile handling and the right
//     obfuscate-host-addresses knob.
//   * **tabs.** Two pages on ONE origin, independently addressable —
//     which is what "two tabs share one identity" means.
//   * **tab lifecycle.** `Page.setWebLifecycleState` over CDP, the
//     only way to freeze a tab and prove the stale-leader fence.
//
// The page↔runner step protocol stays over HTTP. It carries Net
// packet bytes and anchor-built payloads, it is engine-agnostic, and
// it is the half Playwright has nothing to add to — re-plumbing it
// through CDP would have made Firefox impossible.
//
// TLS: there is no `ignoreHTTPSErrors` in this file, and there
// cannot be. The runner installs its CA where each engine looks for
// it and this driver only tells the engine where to look.

import { chromium, firefox, webkit } from 'playwright-core';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import readline from 'node:readline';

const ENGINES = { chromium, firefox, webkit };

/** @type {{engine: string, browser: any, context: any, persistent: boolean}|null} */
let live = null;
/** @type {Map<string, any>} */
const pages = new Map();

function log(line) {
  process.stderr.write(line + '\n');
}

function reply(obj) {
  process.stdout.write(JSON.stringify(obj) + '\n');
}

// ---------------------------------------------------------------------
// Trust: put the harness CA where THIS engine looks for it
// ---------------------------------------------------------------------
//
// Chromium: `--ignore-certificate-errors-spki-list=<pin>` when the
// runner supplies one (the Windows path), and otherwise the NSS store
// the runner seeded (the Linux/CI path). The pin is
// `base64(SHA-256(SubjectPublicKeyInfo))` of the one leaf key the
// harness just minted: verification stays ON, every other certificate
// is still verified, and an impostor presenting a different key is
// still rejected. It is emphatically NOT
// `--ignore-certificate-errors`, and the runner asserts the
// difference with a control (`tls_probe`) before any witness runs.
//
// Firefox: reads its OWN NSS database, `cert9.db` inside the
// profile. So Firefox must run under a persistent profile directory
// we seed before launch. Two mechanisms, in order:
//
//   1. `certutil -d sql:<profile> -A` — the NSS tool, the same one
//      the runner uses for Chromium on Linux. Authoritative: the CA
//      lands in the profile Firefox will read.
//   2. `security.enterprise_roots.enabled` — Firefox reads the
//      PLATFORM store. This is only reachable where the `certutil` on
//      PATH is Microsoft's rather than NSS's, and the runner REFUSES
//      the run when it happens: the harness never writes a platform
//      certificate store, because both adding and removing a root
//      raise a modal security dialog on the desktop of whoever runs
//      it. So this branch is a diagnosis, not a fallback.
//
// Both are reported back so the runner can print which one a run
// actually used; a run that got neither is refused, not weakened.
function seedFirefoxProfile(profileDir, caPemPath, nickname) {
  fs.mkdirSync(profileDir, { recursive: true });
  const db = 'sql:' + profileDir;
  try {
    execFileSync('certutil', ['-N', '--empty-password', '-d', db], { stdio: 'ignore' });
  } catch {
    // An existing database is fine; a missing tool surfaces below.
  }
  try {
    execFileSync(
      'certutil',
      ['-d', db, '-A', '-t', 'C,,', '-n', nickname, '-i', caPemPath],
      { stdio: 'pipe' },
    );
    if (fs.existsSync(path.join(profileDir, 'cert9.db'))) {
      return { how: `certutil -d ${db} -A -t C,, (Firefox's own NSS database)`, enterpriseRoots: false };
    }
    return {
      how: `certutil -A reported success but ${profileDir}/cert9.db is absent; the only \
remaining mechanism is security.enterprise_roots.enabled — see above`,
      enterpriseRoots: true,
    };
  } catch (e) {
    return {
      how: `NSS certutil unusable (${(e.message || String(e)).split('\n')[0]}); \
the only remaining mechanism is security.enterprise_roots.enabled, which reads \
a platform root store this harness never writes — the runner refuses the run`,
      enterpriseRoots: true,
    };
  }
}

// Chromium's flags, in one place so the run's browser and the TLS
// control differ in EXACTLY one thing: whether the pin is passed.
function chromiumArgs(spkiPin) {
  const args = [
    '--no-sandbox',
    '--disable-gpu',
    '--disable-background-timer-throttling',
    '--disable-renderer-backgrounding',
  ];
  if (spkiPin) args.push(`--ignore-certificate-errors-spki-list=${spkiPin}`);
  return args;
}

// ---------------------------------------------------------------------
// launch
// ---------------------------------------------------------------------

async function opLaunch(req) {
  if (live) throw new Error('a browser is already live; shutdown first');
  const engine = req.engine;
  const type = ENGINES[engine];
  if (!type) throw new Error(`unknown engine ${engine}`);

  const profileDir = req.profileDir;
  fs.rmSync(profileDir, { recursive: true, force: true });
  fs.mkdirSync(profileDir, { recursive: true });

  let trust = 'the OS / NSS store the runner installed the CA into';
  // What this launch did to the engine's UDP, verbatim, so a ledger
  // line can name the mechanism instead of claiming one.
  let udp = 'untouched: the engine gathers UDP candidates normally';
  let browser = null;
  let context = null;
  let persistent = false;

  if (engine === 'chromium') {
    const args = chromiumArgs(req.spkiPin);
    // Chromium's `WebRtcHideLocalIpsWithMdns` is ON by default and
    // that default is what the mDNS witness measures. The flag is
    // only ever passed to turn it OFF, deliberately.
    if (req.disableMdns) args.push('--disable-features=WebRtcHideLocalIpsWithMdns');
    if (req.spkiPin) {
      trust =
        '--ignore-certificate-errors-spki-list=' +
        req.spkiPin +
        ' (this run\'s leaf SPKI only; verification stays on, the OS store is untouched)';
    }
    // The UDP-blocked profile, engine-side: Chrome's own enterprise
    // `WebRTCIPHandlingPolicy=disable_non_proxied_udp`, with NO proxy
    // configured. WebRTC may then use UDP only through a proxy, and
    // there is none — measured effect: gathering completes with ZERO
    // candidates, not even a host one, while HTTPS is untouched.
    if (req.webrtcUdpOff) {
      args.push('--force-webrtc-ip-handling-policy=disable_non_proxied_udp');
      udp =
        '--force-webrtc-ip-handling-policy=disable_non_proxied_udp with no proxy configured: ' +
        'WebRTC has no non-proxied UDP at all (gathering yields zero candidates)';
    }
    browser = await type.launch({
      headless: true,
      executablePath: req.executablePath || undefined,
      args,
    });
    context = await browser.newContext();
  } else if (engine === 'firefox') {
    const seeded = seedFirefoxProfile(profileDir, req.caPemPath, req.caNickname);
    trust = seeded.how;
    const prefs = {
      // Firefox's equivalent of Chromium's mDNS obfuscation.
      'media.peerconnection.ice.obfuscate_host_addresses': !req.disableMdns,
      'media.peerconnection.enabled': true,
      // A headless Firefox with no permission prompt still needs
      // DataChannels, which need no media permission — but ICE on a
      // loopback-only host does need the loopback candidate.
      'media.peerconnection.ice.loopback': true,
      'network.proxy.type': 0,
    };
    if (seeded.enterpriseRoots) prefs['security.enterprise_roots.enabled'] = true;
    // Firefox's counterpart: ICE may only use a proxy, and none is
    // configured.
    if (req.webrtcUdpOff) {
      prefs['media.peerconnection.ice.proxy_only'] = true;
      udp =
        'media.peerconnection.ice.proxy_only=true with network.proxy.type=0: ICE may only ' +
        'go through a proxy and none is configured';
    }
    context = await type.launchPersistentContext(profileDir, {
      headless: true,
      executablePath: req.executablePath || undefined,
      firefoxUserPrefs: prefs,
    });
    persistent = true;
    browser = context.browser();
  } else {
    // WebKit. Best effort, recorded: it has no obfuscation knob we
    // can set and its trust store handling is platform private.
    if (req.webrtcUdpOff) {
      throw new Error(
        'WebKit has no UDP-handling knob this driver can set, so the UDP-blocked profile ' +
          'cannot be established on it from here',
      );
    }
    browser = await type.launch({
      headless: true,
      executablePath: req.executablePath || undefined,
    });
    context = await browser.newContext();
    trust = 'WebKit uses the platform trust store; not configurable from here';
  }

  live = { engine, browser, context, persistent };
  return {
    version: browser ? browser.version() : 'unknown',
    trust,
    mdnsObfuscation: !req.disableMdns,
    udp,
  };
}
// ---------------------------------------------------------------------
// pages
// ---------------------------------------------------------------------

async function opOpen(req) {
  if (!live) throw new Error('no browser is live');
  if (pages.has(req.page)) throw new Error(`page ${req.page} is already open`);
  // A persistent context (Firefox) opens with one blank page, and
  // that page IS the browser window: close the last page and the
  // window goes with it, so the next `newPage` dies with
  // "can't access property delayedStartupPromise, window is null".
  // It used to be adopted for the first `open`, and then the 4b
  // half's `close_page` took the window down before Stage 5 could
  // open its tab. So the blank page is now left alone as the window
  // anchor and every `open` gets a genuinely new tab.
  const page = await live.context.newPage();
  // The pages POST their own narrative to `/harness/log`, so
  // forwarding `console.log` too would double every line. Only what
  // the page did NOT choose to report is forwarded: warnings,
  // errors and the engine's own complaints.
  page.on('console', (m) => {
    const type = m.type();
    if (type === 'log' || type === 'debug' || type === 'info') return;
    log(`[${req.page}] ${type}: ${m.text()}`);
  });
  page.on('pageerror', (e) => log(`[${req.page}] pageerror: ${e.message}`));
  page.on('crash', () => log(`[${req.page}] CRASHED`));
  pages.set(req.page, page);
  await page.goto(req.url, { waitUntil: 'domcontentloaded', timeout: req.timeoutMs || 30000 });
  return { url: page.url() };
}

async function opClosePage(req) {
  const page = pages.get(req.page);
  if (!page) throw new Error(`no page named ${req.page}`);
  pages.delete(req.page);
  await page.close({ runBeforeUnload: false });
  return {};
}

async function opEval(req) {
  const page = pages.get(req.page);
  if (!page) throw new Error(`no page named ${req.page}`);
  const value = await page.evaluate(req.expr);
  return { value };
}

// `Page.setWebLifecycleState` is CDP, so it is Chromium only. A
// request for it on another engine is an ERROR, never a silent no-op:
// a frozen-tab witness that quietly did not freeze the tab proves
// nothing.
async function opLifecycle(req) {
  const page = pages.get(req.page);
  if (!page) throw new Error(`no page named ${req.page}`);
  if (live.engine !== 'chromium') {
    throw new Error(
      `Page.setWebLifecycleState is CDP-only; ${live.engine} cannot freeze a tab`,
    );
  }
  const session = await live.context.newCDPSession(page);
  try {
    await session.send('Page.enable');
    await session.send('Page.setWebLifecycleState', { state: req.state });
  } finally {
    await session.detach().catch(() => {});
  }
  return { state: req.state };
}

async function opShutdown() {
  const it = live;
  live = null;
  pages.clear();
  if (!it) return {};
  try {
    if (it.persistent) await it.context.close();
    else await it.browser.close();
  } catch (e) {
    log('shutdown: ' + (e.message || String(e)));
  }
  return {};
}

// ---------------------------------------------------------------------
// the TLS control
// ---------------------------------------------------------------------
//
// One throwaway browser, one NAVIGATION, one fact. The runner calls
// this twice — once WITHOUT the pin and once WITH it — and refuses to
// run unless the first fails the handshake and the second succeeds.
// That is what makes "the pin is why TLS verifies" an assertion
// rather than an assumption: without the negative half a leftover
// root in the machine's store would look exactly the same.
//
// A navigation, NOT a `fetch`. A cross-origin `fetch(url, {mode:
// 'no-cors'})` is rejected here even when TLS verified perfectly —
// Chromium's Opaque Response Blocking refuses a no-cors response that
// is not an image, audio or video — so a fetch-based control reports
// "refused" for both halves and proves nothing. It was measured
// doing exactly that. A navigation carries the engine's own verdict:
// an HTTP status means the certificate verified, and a rejection
// names the reason (`net::ERR_CERT_AUTHORITY_INVALID`), which is the
// discriminator this control needs. The page loads no harness script,
// so it never touches the step protocol.
async function opTlsProbe(req) {
  const type = ENGINES[req.engine];
  if (!type) throw new Error(`unknown engine ${req.engine}`);
  if (req.engine !== 'chromium') {
    throw new Error(
      `the SPKI-pin control is Chromium-only; ${req.engine} has no such flag, so a probe ` +
        'here would report nothing about how that engine trusts the listener',
    );
  }
  const browser = await type.launch({
    headless: true,
    executablePath: req.executablePath || undefined,
    args: chromiumArgs(req.spkiPin),
  });
  try {
    const page = await (await browser.newContext()).newPage();
    let outcome;
    try {
      const res = await page.goto(req.url, { waitUntil: 'domcontentloaded', timeout: 20000 });
      outcome = {
        verified: true,
        detail: `the engine completed the TLS handshake and returned HTTP ${
          res ? res.status() : 'no response object'
        }`,
      };
    } catch (e) {
      outcome = {
        verified: false,
        detail: ((e && (e.message || String(e))) || 'navigation rejected').split('\n')[0],
      };
    }
    log(
      `[tls-probe] pin=${req.spkiPin ? 'present' : 'ABSENT'} ${req.url} -> ` +
        `${outcome.verified ? 'verified' : 'refused'}: ${outcome.detail}`,
    );
    return outcome;
  } finally {
    await browser.close().catch(() => {});
  }
}

const OPS = {
  launch: opLaunch,
  open: opOpen,
  close_page: opClosePage,
  eval: opEval,
  lifecycle: opLifecycle,
  tls_probe: opTlsProbe,
  shutdown: opShutdown,
  ping: async () => ({ pong: true }),
};

// ---------------------------------------------------------------------
// the loop
// ---------------------------------------------------------------------
//
// Requests are served STRICTLY IN ORDER. The runner issues one at a
// time and the witnesses are written against that: an `open` that
// raced a `launch` would report a page on the previous engine.

const rl = readline.createInterface({ input: process.stdin });
let queue = Promise.resolve();

rl.on('line', (line) => {
  const text = line.trim();
  if (!text) return;
  let req;
  try {
    req = JSON.parse(text);
  } catch (e) {
    reply({ id: 0, ok: false, error: 'unparseable request: ' + text.slice(0, 120) });
    return;
  }
  queue = queue.then(async () => {
    const op = OPS[req.op];
    if (!op) {
      reply({ id: req.id, ok: false, error: `unknown op ${req.op}` });
      return;
    }
    try {
      const out = await op(req);
      reply({ id: req.id, ok: true, ...out });
    } catch (e) {
      const msg = (e && (e.message || String(e))) || 'unknown driver error';
      reply({ id: req.id, ok: false, error: msg.split('\nCall log')[0] });
    }
    if (req.op === 'exit') process.exit(0);
  });
});

rl.on('close', async () => {
  await opShutdown();
  process.exit(0);
});

reply({ id: 0, ok: true, hello: 'rtc-browser driver', engines: Object.keys(ENGINES) });
