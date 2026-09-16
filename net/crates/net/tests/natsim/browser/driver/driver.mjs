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
    // The engine's own per-check account. Two earlier attempts failed
    // and the reason is worth keeping: these rows launch Playwright's
    // headless-SHELL build, which ignores `--vmodule`, and
    // `--enable-logging=stderr` goes to a pipe Playwright swallows.
    // `=stderr`, and WE own the process now, so nothing swallows it.
    // `--log-file` was the wrong target: child processes inherit
    // stderr, not the parent's log file, and ICE runs in a child.
    '--enable-logging=stderr',
    // `--v=1`, not `--v=0` with a `--vmodule` list. The previous
    // combination produced only `services/network/p2p/socket_udp.cc`
    // lines — the BROWSER-process socket layer — because libwebrtc's
    // own `RTC_LOG` in the RENDERER maps onto Chrome's verbose
    // logging rather than onto per-file `--vmodule` overrides, and at
    // `--v=0` none of it is emitted. The discard accounting for a
    // STUN response lives exactly there.
    '--v=1',
    // MODULE names, not paths. `*/p2p/*` and `*p2p*` matched only
    // `services/network/p2p/socket_udp.cc`, which is the
    // browser-process socket layer; libwebrtc's own files are matched
    // by their bare module name. These four are where the discard
    // accounting lives — 'Received STUN binding request with bad
    // ufrag/pwd', 'unrecognized transaction', 'Rejecting … integrity'.
    // `network` and `basic_port_allocator` are the enumerator: they
    // log 'Ignoring network' with the REASON a candidate interface was
    // rejected, which is the one thing the wildcard fallback does not
    // explain by itself.
    // The last cycle answered the enumerator question: there is NOT ONE
    // 'Ignoring network' line, and the allocator logs 'Allocate ports
    // on any any' — WebRTC was handed ZERO networks rather than
    // networks it rejected. In Chromium the list does not come from
    // WebRTC at all: the NETWORK SERVICE enumerates and sends it over
    // IPC, so these are its modules.
    '--vmodule=connection=2,port=2,stun_request=2,p2p_transport_channel=2,network=3,basic_port_allocator=2,stun_port=2,' +
      'address_tracker_linux=3,network_change_notifier=3,network_change_notifier_linux=3,network_interfaces_linux=3,p2p_socket_manager=3,network_manager=3,ip_address=2',
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
      `--user-data-dir=${path.join(req.profileDir, 'cdp-profile')}`,
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
        // Only the ICE accounting, not thirteen thousand lines of
        // dbus and histograms: the log is evidence, and evidence
        // nobody can find is not evidence.
        for (const line of text.split('\n')) {
          if (
            /connection\.cc|stun_request\.cc|port\.cc|p2p_transport_channel\.cc|stun\.cc|network\.cc|basic_port_allocator\.cc|address_tracker_linux\.cc|network_change_notifier|network_interfaces|p2p_socket_manager\.cc|network_manager\.cc/.test(
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
          }
        }
        // THE PRECONDITION, pinned so it cannot regress silently.
        //
        // A Port on `Net[any:0.0.0.x/0:Wildcard:id=0]` at cost 999
        // means `BasicNetworkManager` enumerated ZERO networks and
        // fell back to wildcard ports — and a wildcard port drops
        // every inbound packet before STUN parsing, which is exactly
        // how §6.12 presented: valid, correctly credentialed packets
        // on the wire, no request answered and no response credited.
        // A real network (`Net[eth0:…:Ethernet:id=1]`, cost 10/50) is
        // what makes the Chromium rows able to work at all, so it is
        // reported either way rather than hoped for.
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
  // THE FIX for §6.12, and it is a permission, not a network.
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
  // real user does before a call. It is a HARNESS grant: no product
  // behaviour changes, no candidate is relabelled, no deadline
  // widened. Chromium only; Firefox's rows are untouched.
  if (live.engine !== 'firefox') {
    try {
      const origin = new URL(req.url).origin;
      await live.context.grantPermissions(['camera', 'microphone'], { origin });
      log(`granted camera+microphone for ${origin}`);
    } catch (e) {
      log(`grantPermissions: ${e && e.message}`);
    }
  }
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

// `chrome://webrtc-internals`, read before the browser closes.
//
// It is the engine's own record of every peer connection in this
// process: the ICE event log, the candidate pairs and their state
// transitions, and the getStats history — the account of the failing
// check that no external observer could supply. Chromium only, opened
// in its own tab so the row's page is untouched, and best-effort: a
// dump that fails must not fail a row.
function reportNetworkEnumeration() {
  // Named plainly, both ways: the fix is a topology fact and a run
  // that silently lost it would otherwise look like a new mystery.
  log(
    `[chromium-ice] PRECONDITION networks: real=${realNetworkPorts} wildcard=${wildcardPorts}` +
      (realNetworkPorts === 0
        ? ' — ZERO enumerated networks, every inbound packet is dropped before STUN (S6_REPORT.md §6.12)'
        : ''),
  );
}

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
  reportNetworkEnumeration();
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
