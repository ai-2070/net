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
import { execFileSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import readline from 'node:readline';

const ENGINES = { chromium, firefox };
const NSS_NICKNAME = 'net-mesh-natsim-harness-ca';

let live = null;
let page = null;
let internals = null;
let wildcardPorts = 0;
let realNetworkPorts = 0;
const networkNames = new Set();

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
    // The engine's own account, into stderr WE own. Two earlier
    // attempts failed and the reason is worth keeping: these rows used
    // to launch Playwright's headless-SHELL build, which ignores
    // `--vmodule`, and Playwright's own `launch()` swallows the
    // stderr; `--log-file` was the wrong target too, because child
    // processes inherit stderr, not the parent's log file, and ICE
    // runs in a child.
    '--enable-logging=stderr',
    // `--v=1` and three modules, down from eighteen.
    //
    // Every line the [chromium-ice] print keeps is either `INFO`
    // (libwebrtc's own `RTC_LOG`, emitted as soon as logging is on:
    // `basic_port_allocator.cc`'s `Net[…]`, `Count of networks`,
    // `Allocate ports on`, `port.cc`'s network cost, the gathered
    // candidates) or `VERBOSE1` (`filtering_network_manager.cc`'s
    // permission status, `ipc_network_manager.cc`'s network list,
    // `socket_udp.cc`'s local address) — read off the severity tokens
    // of run 35138824048's log. What the removed fifteen modules added
    // was hundreds of `connection.cc` ping lines per second, which is
    // what pushed the job-log print past its own limit.
    '--v=1',
    // Three entries, each named for what it feeds:
    //
    // `basic_port_allocator` and `port` are the PRECONDITION's own
    // source — the `Net[…]` descriptors this driver counts and
    // `run_row` refuses a Chromium row without. They are INFO lines
    // and `--v=1` alone should carry them; they stay named because a
    // precondition that silently loses its own input would fail a
    // working row, which is worse than a wide flag.
    //
    // `peer_connection_dependency_factory` is a RECORD rather than a
    // diagnosis: it logs `WebRTC routing preferences: policy: …
    // multiple_routes: …` at VLOG(3) — the renderer's own statement of
    // the EFFECTIVE `WebRtcIPHandling` value, at the point where it
    // decides whether to enumerate interfaces at all. `chrome://policy`
    // would only show a *configured* enterprise policy, and renders it
    // inside a shadow root; this is the value that actually governs,
    // from the line that reads it. (The neighbouring `Active
    // WebRtcIPHandlingPolicy` line is a `DVLOG`, compiled out of a
    // release build, which is why the level is 3 and not 1.)
    '--vmodule=basic_port_allocator=2,port=2,peer_connection_dependency_factory=3',
    // mDNS obfuscation stays ON. Turning it off was a one-cycle
    // experiment and it EXONERATED mDNS: with a real host address
    // Chromium failed identically (`sent=192 gotResponse=0`), so the
    // switch is gone rather than left behind as a knob nobody should
    // reach for.
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
    // SPAWNED BY US, then attached over CDP.
    //
    // `--log-file` does not reach the renderer: Chromium's child
    // processes inherit stderr, not the parent's log file, and ICE
    // runs in the renderer. Thirteen thousand lines of
    // `chrome_debug.log` from one run were all the BROWSER process,
    // with nothing from `connection`, `stun_request` or
    // `p2p_transport_channel` even after the `--vmodule` names were
    // corrected. Playwright's own `launch()` swallows that stderr, so
    // the only way to read it is to own the process: spawn the
    // executable, keep its stderr, and attach with
    // `connectOverCDP`.
    //
    // The browser still runs in THIS namespace, which is the whole
    // point of a driver per namespace — this replaces how the process
    // is started, not where.
    fs.mkdirSync(req.profileDir, { recursive: true });
    const headed = !!process.env.DISPLAY;
    const exe = chromium.executablePath();
    const args = [
      ...chromiumArgs(req.spkiPin),
      '--remote-debugging-port=0',
      // OUTSIDE the uploaded tree. This profile contains unix
      // sockets, and `upload-artifact` refuses a tree containing
      // them — two runs uploaded NOTHING because of it, so the
      // evidence a cycle existed for was unreadable for a reason
      // unrelated to the measurement.
      `--user-data-dir=${fs.mkdtempSync(path.join(os.tmpdir(), 'natsim-cdp-'))}`,
      ...(headed ? [] : ['--headless=new']),
      'about:blank',
    ];
    const child = spawn(exe, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let wsEndpoint = null;
    const ready = new Promise((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new Error('no DevTools endpoint within 60s')),
        60_000,
      );
      let buffered = '';
      child.stderr.on('data', (chunk) => {
        const text = String(chunk);
        if (!wsEndpoint) {
          buffered += text;
          const m = buffered.match(/DevTools listening on (ws:\/\/\S+)/);
          if (m) {
            wsEndpoint = m[1];
            clearTimeout(timer);
            resolve(wsEndpoint);
          }
        }
        // Only the enumeration and ICE accounting, not thirteen
        // thousand lines of dbus and histograms: the log is evidence,
        // and evidence nobody can find is not evidence.
        for (const line of text.split('\n')) {
          if (
            /basic_port_allocator\.cc|port\.cc|p2p_transport_channel\.cc|filtering_network_manager\.cc|ipc_network_manager\.cc|peer_connection_dependency_factory\.cc|socket_udp\.cc/.test(
              line,
            )
          ) {
            log(`[chromium-ice] ${line.trim()}`);
          }
          // PER LINE, not per chunk: the first version tested the whole
          // stderr chunk, so it reported real=0 wildcard=0 on a run
          // whose log was full of `Net[…Wildcard…]` — a counter that
          // measured its own buffering rather than the browser.
          if (/Net\[/.test(line)) {
            if (/Wildcard/.test(line)) wildcardPorts += 1;
            else realNetworkPorts += 1;
            // The descriptor itself, deduplicated, so the row's
            // precondition failure can name the interface the engine
            // did or did not enumerate instead of only counting.
            const m = /Net\[([^\]]*)\]/.exec(line);
            if (m) networkNames.add(m[1]);
          }
        }
        // THE PRECONDITION, pinned so it cannot regress silently.
        //
        // Ports on `Net[any:0.0.0.x/0:Wildcard:id=0]` at cost 999 mean
        // ZERO enumerated interfaces and a wildcard bind, which drops
        // every inbound datagram before STUN parsing. A real network
        // (`Net[eth0:192.168.10x.x/24:Ethernet:id=1]`) is what makes a
        // Chromium row able to work at all, and `run_row` REFUSES a
        // Chromium tab that reports none.
      });
      child.on('error', reject);
      child.on('exit', (code) =>
        reject(new Error(`chromium exited with ${code} before DevTools`)),
      );
    });
    await ready;
    log(`chromium ${headed ? 'HEADED' : 'headless'} pid ${child.pid}, cdp ${wsEndpoint}`);
    const browser = await chromium.connectOverCDP(wsEndpoint);
    const context = browser.contexts()[0] || (await browser.newContext());
    // Opened NOW, not at shutdown. `chrome://webrtc-internals` only
    // records peer connections that exist while it is open, and the
    // leaf closes its connection the moment `connect` gives up.
    try {
      internals = await context.newPage();
      await internals.goto('chrome://webrtc-internals', {
        waitUntil: 'domcontentloaded',
        timeout: 15_000,
      });
    } catch (e) {
      internals = null;
      log(`webrtc-internals open: ${e && e.message}`);
    }
    live = { engine: req.engine, context, browser, persistent: false, child };
    trust = 'spki-pin';
  }
  return { engine: req.engine, trust };
}

async function opOpen(req) {
  if (!live) throw new Error('open before launch');
  // THE §6.12 FIX, and it is a permission, not a network — BUT IT IS
  // NOW THE ROW'S CHOICE, NOT THIS DRIVER'S.
  //
  // Chromium gates local-interface enumeration on media permission:
  // `FilteringNetworkManager` logged `received permission status:
  // denied` on every failing row, so the network list the browser
  // process had ALREADY delivered was withheld from WebRTC, which
  // then allocated `any any` wildcard ports — and a wildcard port
  // drops every inbound packet before STUN parsing. That is why
  // Chromium answered no Binding Request and credited no response
  // while every packet on the wire was valid, correctly
  // credentialed and correctly addressed, and why Firefox, which
  // has no such gate, was fine throughout.
  //
  // Granting camera/microphone for the page's own origin is what a
  // real user does before a call, and it is what the six conformance
  // rows and the Firefox control do. It is a HARNESS grant: no
  // product behaviour changes, no candidate is relabelled, no
  // deadline widened.
  //
  // `req.media === 'none'` is the PERMISSION-FREE leg. "The product
  // calls no media API" is true and is source evidence about the
  // product; it is not a measurement of the browsing context that
  // was never asked, and only a leg that withholds the grant can
  // make one. So the grant is a per-request decision and this driver
  // reports back what it actually did — an unreported grant and no
  // grant must not look the same to the runner.
  let media = 'none';
  if (req.media !== 'none' && live.engine !== 'firefox') {
    const origin = new URL(req.url).origin;
    // NOT swallowed. A failed `grantPermissions` used to be logged
    // and the row continued as though the grant had happened, which
    // is the same collapse as an unreported grant: the row would then
    // measure the ungranted environment while its verdict said
    // `granted`.
    await live.context.grantPermissions(['camera', 'microphone'], { origin });
    media = 'granted';
    log(`granted camera+microphone for ${origin}`);
  } else if (req.media === 'none') {
    log('media permission WITHHELD: product defaults, nothing granted, nothing prompted');
  } else {
    // Firefox has no such gate and Playwright cannot grant camera or
    // microphone to it at all, so the control row has always run
    // ungranted. Reported as the fact it is instead of as the flag
    // the runner passed.
    log('firefox: no media permission granted (no such gate, and no Playwright support)');
  }
  page = await live.context.newPage();
  page.on('console', (m) => log(`[page] ${m.type()}: ${m.text()}`));
  page.on('pageerror', (e) => log(`[page] ERROR ${e && e.message}`));
  // `domcontentloaded`, not `load`: the page's own work starts on
  // module evaluation and the runner drives it over HTTP from there.
  // Waiting for `load` would block on whatever the page is already
  // doing.
  await page.goto(req.url, { waitUntil: 'domcontentloaded', timeout: 60_000 });
  return { url: page.url(), media };
}

// What the port allocator enumerated, as a value the runner can
// assert on rather than a line a human has to find.
function networksSnapshot() {
  return { real: realNetworkPorts, wildcard: wildcardPorts, nets: [...networkNames] };
}

function reportNetworkEnumeration(at) {
  // Named plainly, both ways: the enumerated interface is a
  // precondition of every GRANTED Chromium row, and a run that
  // silently lost it would otherwise look like a new mystery.
  //
  // The line no longer claims inbound packets are dropped. §11.8
  // measured a `real=0` pair reaching the STUN endpoint, gathering
  // srflx and delivering application payloads both ways, so that
  // clause was a conclusion this instrument cannot support. It
  // reports what it counted, and names the permission gate that
  // explains a zero.
  const nets = networkNames.size === 0 ? '(none)' : [...networkNames].join(' ');
  log(
    `[chromium-ice] PRECONDITION networks at ${at}: real=${realNetworkPorts} ` +
      `wildcard=${wildcardPorts} nets=${nets}` +
      (realNetworkPorts === 0
        ? ' — ZERO enumerated networks; expected when no media permission was granted' +
          ' (FilteringNetworkManager gates the network list), REQUIRED of a granted row' +
          ' (S6_REPORT.md §6.12, §11.8)'
        : ''),
  );
}

// `chrome://webrtc-internals`, read before the browser closes.
//
// It is the engine's own record of every peer connection in this
// process: the ICE event log, the candidate pairs and their state
// transitions, and the getStats history — the account of a failing
// check that no external observer could supply. Chromium only, opened
// in its own tab so the row's page is untouched, and best-effort: a
// dump that fails must not fail a row.
async function dumpWebrtcInternals() {
  if (!internals) return;
  try {
    // A beat for the page to render the last events it received.
    await internals.waitForTimeout(1_500);
    const text = await internals.evaluate(() => document.body.innerText || '');
    for (const line of text.split('\n')) {
      const trimmed = line.trim();
      if (trimmed) log(`[webrtc-internals] ${trimmed}`);
    }
  } catch (e) {
    log(`webrtc-internals: ${e && e.message}`);
  }
}

async function opShutdown() {
  reportNetworkEnumeration('shutdown');
  await dumpWebrtcInternals();
  try {
    if (live && live.persistent) await live.context.close();
    else if (live && live.browser) await live.browser.close();
    // `connectOverCDP` detaches rather than terminating, so the
    // process we spawned is ours to end.
    if (live && live.child && live.child.exitCode === null) live.child.kill('SIGTERM');
  } catch (e) {
    log(`shutdown: ${e && e.message}`);
  }
  live = null;
  page = null;
  internals = null;
  // The counters travel in the reply: `run_row` refuses a Chromium
  // row that enumerated no interface, and it can only do that if the
  // number reaches it.
  return { networks: networksSnapshot() };
}

const OPS = { launch: opLaunch, open: opOpen, shutdown: opShutdown };

// Requests are served strictly in order: the runner issues one at a
// time, and an `open` that raced a `launch` would report a page on no
// browser at all.
// At EXIT as well as at shutdown: a driver whose row relaunched the
// browser prints a shutdown line before the engine has logged
// anything, and that zero says nothing about the run. The exit line
// is the one that covers the whole process.
process.on('exit', () => reportNetworkEnumeration('exit'));

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

// The default-local-address probe, from inside the namespace.
//
// libwebrtc (and Chromium's network service, which is what actually
// enumerates here) learns a "default local address" by connect()ing a
// throwaway UDP socket at a public IP; a namespace where that route
// does not resolve enumerates nothing. `setup.sh` already gives every
// browser namespace `default via <gateway>`, so it resolves — and the
// point of printing it is that the precondition is then a FACT in the
// log rather than an inference from the topology script. Reachability
// is irrelevant: nothing answers 8.8.8.8 in this lab and nothing has
// to.
try {
  const route = execFileSync('ip', ['route', 'get', '8.8.8.8'], { encoding: 'utf8' });
  log(`ip route get 8.8.8.8 -> ${route.trim().replace(/\s+/g, ' ')}`);
} catch (e) {
  log(`ip route get 8.8.8.8 FAILED: ${(e && e.message) || e}`);
}

// Is a `WebRtcIPHandling` policy being inherited from disk?
//
// The suspicion was that the headed CDP launch picks up a policy file
// that turns interface enumeration off. Recorded once, from the only
// places Chromium reads managed policy on Linux; the EFFECTIVE value
// is reported by the renderer itself (`peer_connection_dependency_
// factory.cc`: `WebRTC routing preferences: policy: …`) in the
// [chromium-ice] lines.
const policyDirs = [
  '/etc/chromium/policies/managed',
  '/etc/chromium/policies/recommended',
  '/etc/opt/chrome/policies/managed',
  '/etc/opt/chrome/policies/recommended',
];
log(
  `chromium managed-policy files: ${
    policyDirs
      .flatMap((dir) => {
        try {
          return fs.readdirSync(dir).map((f) => `${dir}/${f}`);
        } catch {
          return [];
        }
      })
      .join(' ') || '(none)'
  }`,
);

reply({ id: 0, ok: true, hello: 'natsim browser driver', engines: Object.keys(ENGINES) });
