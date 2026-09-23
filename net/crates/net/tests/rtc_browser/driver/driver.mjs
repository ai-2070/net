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
import os from 'node:os';
import path from 'node:path';
import readline from 'node:readline';

const ENGINES = { chromium, firefox, webkit };

/** @type {{engine: string, browser: any, context: any, persistent: boolean}|null} */
let live = null;
/** @type {Map<string, any>} */
const pages = new Map();

// ISOLATED BROWSING CONTEXTS, BY NAME
// -----------------------------------
//
// `live.context` above is the LAUNCH context and stays the default:
// an `open` with no `context` field lands there, exactly as every
// Stage 4b / Stage 5 witness expects.
//
// A *named* context is a second browsing context, and that is a
// stronger claim than a second tab. Two tabs in one context share
// one storage partition and ONE Web Locks namespace, so two leaves
// in them share their persisted identity and contend for the same
// lock — which is precisely what the "two tabs, one identity"
// witnesses measure. A browser↔browser session instead needs two
// leaves that are genuinely independent: separate storage (separate
// identity) and separate Web Locks (separate leader election). Only
// a separate context gives that.
//
// Firefox cannot use `browser.newContext()` here. It is launched
// through `launchPersistentContext`, so `context.browser()` may be
// null (the `browser ? browser.version() : 'unknown'` guard in
// `opLaunch` exists for that), and there is no Browser to ask for a
// second context. So on a persistent engine a named context is a
// SECOND persistent context on its own profile directory — which is
// also the strongest isolation available, a whole separate profile.
// That profile must be seeded with the same harness CA the first
// one was, or TLS to the anchor fails inside it.
/** @type {Map<string, {context: any, persistent: boolean, profileDir?: string}>} */
const contexts = new Map();
// What a later named context needs to be created the same way the
// launch context was: the resolved engine type, the executable, the
// flags/prefs `opLaunch` computed, the profile dir and the CA to
// seed a further profile with. Set beside `live`, cleared with it.
/** @type {{engine: string, type: any, executablePath: string|undefined, args: string[]|null, firefoxUserPrefs: object|null, profileDir: string, caPemPath: string|undefined, caNickname: string|undefined}|null} */
let launchSpec = null;
/** Which context each open page belongs to. @type {Map<string, any>} */
const pageContexts = new Map();

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
  // `RTCB_CERTUTIL` names the NSS certutil by ABSOLUTE path. On hosts
  // where a Microsoft `certutil` sits in the system directory (and
  // shell PATH editing is unreliable across spawn mechanisms), the
  // tool must be nameable without resolution games. Absent the env
  // var, plain `certutil` resolves as before.
  const CERTUTIL = process.env.RTCB_CERTUTIL || 'certutil';
  try {
    execFileSync(CERTUTIL, ['-N', '--empty-password', '-d', db], { stdio: 'ignore' });
  } catch {
    // An existing database is fine; a missing tool surfaces below.
  }
  try {
    execFileSync(
      CERTUTIL,
      ['-d', db, '-A', '-t', 'C,,', '-n', nickname, '-i', caPemPath],
      { stdio: 'pipe' },
    );
    if (fs.existsSync(path.join(profileDir, 'cert9.db'))) {
      // `nss: true` ONLY here: certutil reported success AND the
      // database it claims to have written exists. Every other
      // return says the profile was not seeded, which is what the
      // trust control must be able to distinguish — it was reading
      // a field nobody set, so a successful seed read as a failure
      // and the success string was printed as the reason.
      return {
        nss: true,
        how: `certutil -d ${db} -A -t C,, (Firefox's own NSS database)`,
        enterpriseRoots: false,
      };
    }
    return {
      nss: false,
      how: `certutil -A reported success but ${profileDir}/cert9.db is absent; the only \
remaining mechanism is security.enterprise_roots.enabled — see above`,
      enterpriseRoots: true,
    };
  } catch (e) {
    return {
      nss: false,
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
  // Captured out of the engine branches so a named context can be
  // created with the SAME flags / prefs this launch used.
  let ctxArgs = null;
  let ctxPrefs = null;

  if (engine === 'chromium') {
    const args = chromiumArgs(req.spkiPin);
    ctxArgs = args;
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
    ctxPrefs = prefs;
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
  launchSpec = {
    engine,
    type,
    executablePath: req.executablePath || undefined,
    args: ctxArgs,
    firefoxUserPrefs: ctxPrefs,
    profileDir,
    caPemPath: req.caPemPath,
    caNickname: req.caNickname,
  };
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

// Resolve a browsing context by name, creating it on first use.
// `null` / absent / empty is the launch context — today's path,
// untouched.
async function contextFor(name) {
  if (!name) return live.context;
  const existing = contexts.get(name);
  if (existing) return existing.context;
  if (!launchSpec) throw new Error('no launch spec recorded; cannot create a context');

  if (live.persistent) {
    // Firefox. A second persistent context on its own profile,
    // seeded with the same CA through the same code path — a
    // differently-trusted profile would fail TLS to the anchor and
    // take the Firefox leg down in CI.
    const dir = `${launchSpec.profileDir}-${name}`;
    fs.rmSync(dir, { recursive: true, force: true });
    const seeded = seedFirefoxProfile(dir, launchSpec.caPemPath, launchSpec.caNickname);
    const context = await launchSpec.type.launchPersistentContext(dir, {
      headless: true,
      executablePath: launchSpec.executablePath,
      firefoxUserPrefs: launchSpec.firefoxUserPrefs || undefined,
    });
    contexts.set(name, { context, persistent: true, profileDir: dir });
    log(`context ${name}: second persistent profile ${dir} — trust: ${seeded.how}`);
    return context;
  }

  if (!live.browser) {
    // Named, not silent. Falling back to the launch context here
    // would hand back a page that SHARES storage and Web Locks
    // while the witness reports two isolated contexts — the exact
    // false claim this registry exists to make impossible.
    throw new Error(
      `no_browser_for_context: engine ${live.engine} is not persistent yet exposes no ` +
        `Browser, so the isolated context ${name} cannot be created; refusing to fall ` +
        'back to the launch context, which would report isolation the page did not get',
    );
  }
  const context = await live.browser.newContext();
  contexts.set(name, { context, persistent: false });
  log(`context ${name}: new isolated browser context`);
  return context;
}

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
  const named =
    typeof req.context === 'string' && req.context.length > 0 ? req.context : null;
  const ctx = await contextFor(named);
  const page = await ctx.newPage();
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
  pageContexts.set(req.page, ctx);
  await page.goto(req.url, { waitUntil: 'domcontentloaded', timeout: req.timeoutMs || 30000 });
  return { url: page.url() };
}

async function opClosePage(req) {
  const page = pages.get(req.page);
  if (!page) throw new Error(`no page named ${req.page}`);
  pages.delete(req.page);
  pageContexts.delete(req.page);
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
  // The CDP session must be opened on the context that OWNS the
  // page, which for a named context is not `live.context`.
  const owner = pageContexts.get(req.page) || live.context;
  const session = await owner.newCDPSession(page);
  try {
    await session.send('Page.enable');
    await session.send('Page.setWebLifecycleState', { state: req.state });
  } finally {
    await session.detach().catch(() => {});
  }
  return { state: req.state };
}

// `BrowserContext.setOffline` — the network change plan §10's retry
// row is driven by.
//
// **Per CONTEXT, and the context that owns this page.** Stage 6's two
// tabs live in genuinely isolated contexts, so `live.context` is the
// wrong one for either of them: taking the launch context offline
// would change nothing a Stage 6 page can observe and the witness
// would pass while having disconnected nobody. The page name is
// therefore the argument, and the context is looked up from it —
// the same lookup `opLifecycle` does, for the same reason.
//
// The context's OTHER pages go offline with it, which is exactly
// right: a browsing context is the unit a network change happens to.
// It is also why the caller must name the side it means — "the
// network changed" is ambiguous about whose network.
//
// What this does and does not do, stated because the witness depends
// on it: it flips `navigator.onLine` and fires `offline`/`online` in
// every page of the context, and it fails the context's HTTP. It is
// NOT a link-layer break — Chromium's emulation sits on the URL
// loader, so an established ICE path over loopback keeps working.
// The row that needs a dead direct path has to cause one; this
// provides the network CHANGE, which is what the trigger is about.
//
// **Two mechanisms, and the second is the one that matters.**
// `BrowserContext.setOffline` fails the context's HTTP but does NOT
// move the renderer's network state on Chromium: measured, first
// run — `0 offline event(s)`, `navigator.onLine` unchanged, and so
// nothing a page listens for ever fired. DevTools' own Offline
// checkbox is `Network.emulateNetworkConditions`, which DOES notify
// the renderer, so that is sent per page as well. Both are applied:
// the first is what stops the page's HTTP, the second is what makes
// `navigator.onLine` and the `offline`/`online` events true.
//
// The reply carries `online`, read back out of the page, so a
// witness asserts on what the renderer BELIEVES rather than on the
// fact that a request was accepted.

/// Page name → the CDP session holding its network emulation.
///
/// Held because CDP emulation is scoped to its session: detaching
/// reverts it, so a session opened and closed around one
/// `emulateNetworkConditions` produces an offline window that lasts
/// microseconds and still fires the full event pair.
/** @type {Map<string, any>} */
const cdpNetwork = new Map();

async function opOffline(req) {
  const page = pages.get(req.page);
  if (!page) throw new Error(`no page named ${req.page}`);
  const offline = req.offline === true;
  const owner = pageContexts.get(req.page) || live.context;
  await owner.setOffline(offline);
  if (live.engine === 'chromium') {
    // **The session is HELD for the whole window.** CDP emulation
    // is scoped to the session that set it, so detaching reverts
    // it: the first version opened a session, sent `offline: true`
    // and detached in a `finally`, which fired `offline` and then
    // immediately `online` again — a window microseconds long that
    // read as a completed network change. The page saw the event
    // pair and `navigator.onLine` read TRUE a moment later, which
    // is how a reverted emulation looks exactly like a working one.
    let session = cdpNetwork.get(req.page);
    if (!session) {
      session = await owner.newCDPSession(page);
      await session.send('Network.enable');
      cdpNetwork.set(req.page, session);
    }
    await session.send('Network.emulateNetworkConditions', {
      offline,
      latency: 0,
      downloadThroughput: -1,
      uploadThroughput: -1,
    });
  }
  // Read BEFORE any detach, for the same reason.
  let online = null;
  try {
    online = await page.evaluate('navigator.onLine');
  } catch {
    // A page whose HTTP is down can still be evaluated in, but a
    // navigation in flight can refuse: the reading is evidence, not
    // the operation.
  }
  if (!offline) {
    const session = cdpNetwork.get(req.page);
    cdpNetwork.delete(req.page);
    if (session) await session.detach().catch(() => {});
  }
  return { offline, online };
}

async function opShutdown() {
  const it = live;
  const named = [...contexts.values()];
  live = null;
  launchSpec = null;
  contexts.clear();
  pages.clear();
  pageContexts.clear();
  // Named contexts first, then the launch one. Each close is
  // independent: one already-gone context must not strand the rest.
  for (const c of named) {
    try {
      await c.context.close();
    } catch (e) {
      log('shutdown context: ' + (e.message || String(e)));
    }
  }
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
  if (req.engine === 'webkit') {
    throw new Error(
      'WebKit has neither an SPKI-pin flag nor a profile-local trust store, so a probe ' +
        'here would report nothing about how that engine trusts the listener',
    );
  }
  // Firefox's control is the same shape with a different mechanism:
  // the difference between the two halves is whether the throwaway
  // profile's own cert9.db was seeded with this run's CA. Without it
  // the navigation must be refused (SEC_ERROR_UNKNOWN_ISSUER); with
  // it, it must complete — which is what makes every later TLS
  // observation attributable to the seeding rather than to whatever
  // else the machine happens to trust.
  let browser;
  let context;
  let profileDir;
  if (req.engine === 'firefox') {
    profileDir = fs.mkdtempSync(path.join(os.tmpdir(), 'rtcb-ffprobe-'));
    if (req.caPemPath) {
      const seeded = seedFirefoxProfile(profileDir, req.caPemPath, req.caNickname);
      if (!seeded.nss) {
        throw new Error(`the Firefox trust control could not seed its profile: ${seeded.how}`);
      }
    }
    context = await type.launchPersistentContext(profileDir, {
      headless: true,
      executablePath: req.executablePath || undefined,
    });
  } else {
    browser = await type.launch({
      headless: true,
      executablePath: req.executablePath || undefined,
      args: chromiumArgs(req.spkiPin),
    });
  }
  try {
    const page = context
      ? await context.newPage()
      : await (await browser.newContext()).newPage();
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
    const mechanism =
      req.engine === 'firefox'
        ? `profile-CA=${req.caPemPath ? 'seeded' : 'ABSENT'}`
        : `pin=${req.spkiPin ? 'present' : 'ABSENT'}`;
    log(
      `[tls-probe] ${req.engine} ${mechanism} ${req.url} -> ` +
        `${outcome.verified ? 'verified' : 'refused'}: ${outcome.detail}`,
    );
    return outcome;
  } finally {
    if (context) await context.close().catch(() => {});
    if (browser) await browser.close().catch(() => {});
    if (profileDir) fs.rmSync(profileDir, { recursive: true, force: true });
  }
}

const OPS = {
  launch: opLaunch,
  open: opOpen,
  close_page: opClosePage,
  eval: opEval,
  lifecycle: opLifecycle,
  offline: opOffline,
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
