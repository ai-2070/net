#!/usr/bin/env node
// S0b — the sharded-match topology run, in real browsers
// (docs/internal/plans/BROWSER_SHARDED_MATCH_PLAN.md).
//
// H browser shard hosts and N browser players against a real
// `net-mesh anchor serve --game`. Every player joins every host (topology
// A); hosts answer offers so pairs go direct; each host renders the merged
// world while hosting. Measured per host: netcode players held, direct
// pairs up, WIRE upload (Chrome's own RTCPeerConnection stats: DTLS, SCTP
// and Noise included) set against the netcode payload, main-thread busy
// time (CDP), tick time and frame time. Per player: connect outcomes and
// time to the first snapshot from every host.
//
// One machine (a smoke run):
//   node run.mjs coordinator --net-mesh <bin> --hosts 2 --players 8 --local-hosts 2 --local-players 8
//
// Several machines: the coordinator runs the anchor, serves the pages and
// collects the reports; every other machine runs browsers against it.
//   node run.mjs coordinator --net-mesh <bin> --public 192.168.1.10 --hosts 4 --players 127 [--local-hosts 1]
//   node run.mjs browsers --coordinator https://192.168.1.10:<port> --spki <pin> --hosts 1 [--headed]
//   node run.mjs browsers --coordinator https://192.168.1.10:<port> --spki <pin> --players 64
// The coordinator prints both commands with its port and pin filled in.
//
// Needs: the built package (`browser-ts`: `npm run build`, with the leaf
// wasm), `openssl` on PATH, and `npm install` here (playwright-core).
// Exit code: 0 the gate passed, 1 it failed, 2 the run never reached the
// measuring phase.

import { spawn, execFileSync } from 'node:child_process';
import { createHash, randomBytes, X509Certificate } from 'node:crypto';
import { createServer, request } from 'node:https';
import { createServer as createNetServer, isIP } from 'node:net';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const here = dirname(fileURLToPath(import.meta.url));
const dist = join(here, '..', '..', 'browser-ts', 'dist');
const GAME = 'shard-topology';

const [mode, ...args] = process.argv.slice(2);
const arg = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};
const num = (name, fallback) => Number(arg(name, fallback));
const flag = name => args.includes(`--${name}`);
const log = line => console.log(`[s0b] ${line}`);
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

// ── browsers: host and player pages, on any machine ─────────────────────

const BROWSER_ARGS = [
  '--no-sandbox',
  // Every page is a live foreground page: throttled timers would starve
  // the leaf's pump and the tick loop.
  '--disable-background-timer-throttling',
  '--disable-renderer-backgrounding',
  '--disable-backgrounding-occluded-windows',
];

/** GET/POST the coordinator, trusting exactly its pinned certificate. */
function coordinatorApi(base, pin, path, body) {
  return new Promise((resolve, reject) => {
    const url = new URL(`/api/${path}`, base);
    const req = request(
      url,
      { method: body === undefined ? 'GET' : 'POST', rejectUnauthorized: false },
      res => {
        const raw = res.socket.getPeerCertificate()?.raw;
        const der = raw ? new X509Certificate(raw).publicKey.export({ type: 'spki', format: 'der' }) : null;
        const seen = der ? createHash('sha256').update(der).digest('base64') : 'none';
        if (seen !== pin) {
          res.destroy();
          reject(new Error(`the coordinator certificate does not match --spki (it presented ${seen})`));
          return;
        }
        let text = '';
        res.setEncoding('utf8').on('data', chunk => (text += chunk)).on('end', () => {
          try {
            resolve(JSON.parse(text));
          } catch (error) {
            reject(error);
          }
        });
      },
    );
    req.on('error', reject);
    req.end(body === undefined ? undefined : JSON.stringify(body));
  });
}

async function runBrowsers({ base, pin, hosts, players, chrome, headed, staggerMs = 0 }) {
  const browser = await chromium.launch({
    headless: !headed,
    ...(chrome ? { executablePath: chrome } : {}),
    args: [...BROWSER_ARGS, `--ignore-certificate-errors-spki-list=${pin}`],
  });
  log(`browser ${browser.version()}: ${hosts} host page(s), ${players} player page(s)`);
  const pages = [];
  const open = async role => {
    // One context per page: its own storage, so its own identity.
    const context = await browser.newContext({ viewport: { width: 640, height: 720 } });
    const page = await context.newPage();
    page.on('pageerror', error => log(`[${role}] pageerror: ${error.message}`));
    page.on('console', message => {
      if (message.type() === 'error') log(`[${role}] ${message.text()}`);
    });
    await page.goto(`${base}/?role=${role}`);
    const entry = { role, page, cdp: null, last: null };
    if (role === 'host') {
      entry.cdp = await context.newCDPSession(page);
      await entry.cdp.send('Performance.enable');
    }
    pages.push(entry);
  };
  for (let i = 0; i < hosts; i += 1) await open('host');
  for (let i = 0; i < players; i += 1) {
    await open('player');
    if (staggerMs > 0) await sleep(staggerMs);
  }

  // Main-thread busy time for host pages, handed to the page's own report.
  const sampler = setInterval(async () => {
    for (const entry of pages) {
      if (entry.cdp === null) continue;
      try {
        const { metrics } = await entry.cdp.send('Performance.getMetrics');
        const value = name => metrics.find(m => m.name === name)?.value ?? 0;
        const now = { at: Date.now(), task: value('TaskDuration'), script: value('ScriptDuration') };
        if (entry.last !== null) {
          const wall = (now.at - entry.last.at) / 1000;
          const cdp = { busy: (now.task - entry.last.task) / wall, script: (now.script - entry.last.script) / wall };
          await entry.page.evaluate(value => {
            globalThis.__cdp = value;
          }, cdp);
        }
        entry.last = now;
      } catch {
        // A page that closed or navigated: its report says so.
      }
    }
  }, 1000);

  for (;;) {
    await sleep(2000);
    let state;
    try {
      state = await coordinatorApi(base, pin, 'state');
    } catch (error) {
      log(`coordinator unreachable: ${error.message}`);
      continue;
    }
    if (state.phase === 'done') break;
  }
  clearInterval(sampler);
  for (const entry of pages) {
    const status = await entry.page.evaluate(() => globalThis.__match ?? null).catch(() => null);
    if (status?.phase !== 'ready') {
      const diag = await entry.page.evaluate(() => globalThis.__diag?.() ?? null).catch(error => String(error));
      log(`[${entry.role}] not ready: ${JSON.stringify(status)}
    diag: ${JSON.stringify(diag)}`);
      // Both ends of each pair that never delivered: this page's view of
      // the host, and every host page's view of this player.
      if (entry.role === 'player' && status?.node) {
        for (const other of pages) {
          if (other.role !== 'host') continue;
          const hostView = await other.page.evaluate(id => globalThis.__peer?.(id) ?? null, status.node).catch(error => String(error));
          const hostId = await other.page.evaluate(() => globalThis.__match?.node ?? null).catch(() => null);
          const playerView = hostId ? await entry.page.evaluate(id => globalThis.__peer?.(id) ?? null, hostId).catch(error => String(error)) : null;
          log(`    pair ${status.node} ↔ host ${hostId}: player sees ${JSON.stringify(playerView)}; host sees ${JSON.stringify(hostView)}`);
        }
      }
    }
  }
  await browser.close();
}

if (mode === 'browsers') {
  const base = arg('coordinator');
  const pin = arg('spki');
  if (!base || !pin) throw new Error('browsers: pass --coordinator <https url> and --spki <pin> (the coordinator prints both)');
  await runBrowsers({ base, pin, hosts: num('hosts', 0), players: num('players', 0), chrome: arg('chrome'), headed: flag('headed'), staggerMs: num('stagger-ms', 0) });
  process.exit(0);
}

if (mode !== 'coordinator') {
  throw new Error('usage: node run.mjs coordinator --net-mesh <bin> … | node run.mjs browsers --coordinator <url> --spki <pin> …');
}

// ── coordinator ─────────────────────────────────────────────────────────

const netMesh = arg('net-mesh');
if (!netMesh) throw new Error('pass --net-mesh <path to the net-mesh binary> (the same release as the package)');
if (!existsSync(join(dist, 'index.bundle.js')) || !existsSync(join(dist, 'net_leaf_bg.wasm'))) {
  throw new Error(`build the package first: ${dist} has no bundle or leaf wasm`);
}
const config = {
  game: GAME,
  hosts: num('hosts', 2),
  players: num('players', 8),
  npcs: num('npcs', 0),
  codec: arg('codec', 'compact'),
  tickRate: num('tick-rate', 20),
  visible: num('visible', 30),
  burnMs: num('burn-ms', 4),
  announceMs: num('announce-ms', 2000),
  relayed: flag('relayed'),
};
const measureSecs = num('measure-secs', 30);
const joinTimeoutSecs = num('join-timeout-secs', 300);
const budgetMbit = num('budget-mbit', 10);
const targetFps = num('target-fps', 60);
const publicName = arg('public', 'localhost');
const bindIp = arg('bind-ip', publicName === 'localhost' ? '127.0.0.1' : '0.0.0.0');
const outFile = arg('out', join(here, 'work', 'results.json'));
const localHosts = num('local-hosts', 0);
const perAddressLimit = num('per-address-limit', 10_000);
const localPlayers = num('local-players', 0);

async function freePort() {
  return new Promise((resolve, reject) => {
    const server = createNetServer();
    server.listen(0, bindIp, () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
    server.on('error', reject);
  });
}

// Secrets and a certificate valid for every name the browsers will use.
const work = join(here, 'work');
rmSync(work, { recursive: true, force: true });
mkdirSync(work, { recursive: true });
const pskFile = join(work, 'psk.hex');
writeFileSync(pskFile, randomBytes(32).toString('hex'));
const issuerFile = join(work, 'issuer.toml');
execFileSync(netMesh, ['identity', 'generate', '--out', issuerFile], { stdio: 'ignore' });
const certFile = join(work, 'cert.pem');
const keyFile = join(work, 'key.pem');
const san = ['DNS:localhost', 'IP:127.0.0.1', ...(publicName === 'localhost' ? [] : [isIP(publicName) ? `IP:${publicName}` : `DNS:${publicName}`])];
execFileSync('openssl', [
  'req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:prime256v1', '-nodes',
  '-keyout', keyFile, '-out', certFile, '-days', '2', '-subj', '/CN=shard-topology',
  '-addext', `subjectAltName=${san.join(',')}`,
], { stdio: 'ignore' });
const spki = new X509Certificate(readFileSync(certFile)).publicKey.export({ type: 'spki', format: 'der' });
const spkiPin = createHash('sha256').update(spki).digest('base64');

// ── state, reports, verdict ─────────────────────────────────────────────

const registered = { host: [], player: [] };
const reports = []; // every report, in arrival order
const latest = new Map(); // `${role}:${index}` → last report
const failedLogged = new Set();
let phase = 'joining';
let measureFrom = null;
let anchorStats = null;

function readyCount() {
  let hosts = 0;
  let players = 0;
  for (const report of latest.values()) {
    if (report.phase !== 'ready') continue;
    if (report.role === 'host') hosts += 1;
    else players += 1;
  }
  return { hosts, players };
}

const mean = values => (values.length === 0 ? null : values.reduce((a, b) => a + b, 0) / values.length);
const quantile = (values, q) => {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * q))];
};
const mbit = bytes => (bytes * 8) / 1e6;
const fmt = (value, digits = 2) => (value === null || value === undefined ? '—' : Number(value).toFixed(digits));

function verdict() {
  const window = reports.filter(r => measureFrom !== null && r.at >= measureFrom && r.wire);
  const byHost = new Map();
  const byPlayer = new Map();
  for (const r of window) (r.role === 'host' ? byHost : byPlayer).set(r.index, [...((r.role === 'host' ? byHost : byPlayer).get(r.index) ?? []), r]);
  const expectedPeers = config.players + config.hosts - 1;
  const hosts = [...byHost].sort(([a], [b]) => a - b).map(([index, rs]) => {
    const last = rs.at(-1);
    const up = rs.map(r => mbit(r.wire.anchorSent + r.wire.directSent));
    const payload = rs.map(r => mbit(r.payloadBytes ?? 0));
    const relayedUp = rs.map(r => mbit(r.wire.anchorSent));
    const frameP50 = quantile(rs.map(r => r.frame?.p50).filter(v => v !== null && v !== undefined), 0.5);
    return {
      index,
      node: last.node,
      players: last.players,
      directUp: last.wire.directUp,
      upMbit: mean(up),
      upMbitMax: Math.max(...up),
      relayedUpMbit: mean(relayedUp),
      payloadMbit: mean(payload),
      overhead: mean(payload) > 0 ? mean(up) / mean(payload) : null,
      tickP95: quantile(rs.map(r => r.tick?.p95).filter(v => v !== null && v !== undefined), 0.5),
      frameP50,
      frameP95: quantile(rs.map(r => r.frame?.p95).filter(v => v !== null && v !== undefined), 0.5),
      fps: frameP50 ? 1000 / frameP50 : null,
      busy: mean(rs.map(r => r.cdp?.busy).filter(v => typeof v === 'number')),
      heapMb: last.heapMb,
      ice: last.ice,
      accepts: last.outcomes?.accept ?? {},
      dropped: last.dropped,
    };
  });
  const playerRows = [...byPlayer].map(([index, rs]) => ({ index, last: rs.at(-1), down: mean(rs.map(r => mbit(r.wire.anchorReceived + r.wire.directReceived))) }));
  const joinMs = playerRows.flatMap(p => Object.values(p.last.joinedAt ?? {}));
  const connectMs = playerRows.map(p => p.last.connectMs).filter(v => typeof v === 'number');
  const errors = playerRows.flatMap(p => p.last.outcomes?.errors ?? []);
  const connects = {};
  for (const p of playerRows) for (const [outcome, n] of Object.entries(p.last.outcomes?.connect ?? {})) connects[outcome] = (connects[outcome] ?? 0) + n;
  const checks = [];
  const check = (name, ok, detail) => checks.push({ name, ok, detail });
  check('every host reported in the window', hosts.length === config.hosts, `${hosts.length}/${config.hosts}`);
  check('every player reported in the window', playerRows.length === config.players, `${playerRows.length}/${config.players}`);
  for (const h of hosts) {
    check(`host ${h.index} holds every netcode peer`, h.players === expectedPeers, `${h.players}/${expectedPeers}`);
    if (!config.relayed) check(`host ${h.index} has every pair direct`, h.directUp >= expectedPeers, `${h.directUp}/${expectedPeers}`);
    check(`host ${h.index} upload within ${budgetMbit} Mbit/s`, h.upMbit !== null && h.upMbit <= budgetMbit, `${fmt(h.upMbit)} Mbit/s mean, ${fmt(h.upMbitMax)} max`);
    check(`host ${h.index} renders at ≥ 90% of ${targetFps} fps`, h.fps !== null && h.fps >= 0.9 * targetFps, `${fmt(h.fps, 1)} fps median`);
  }
  return {
    pass: checks.every(c => c.ok),
    checks,
    hosts,
    players: {
      count: playerRows.length,
      downMbitMean: mean(playerRows.map(p => p.down)),
      joinMsP50: quantile(joinMs, 0.5),
      joinMsP95: quantile(joinMs, 0.95),
      joinMsMax: joinMs.length ? Math.max(...joinMs) : null,
      connectOutcomes: connects,
      connectMsP50: quantile(connectMs, 0.5),
      connectMsMax: connectMs.length ? Math.max(...connectMs) : null,
      errors: errors.slice(0, 20),
    },
  };
}

function printVerdict(result) {
  console.log('\nhost | players direct | up Mbit/s mean  max | via anchor | payload | overhead | tick p95 | frame p50 p95 (fps) | busy | heap MB');
  for (const h of result.hosts) {
    console.log(
      [
        String(h.index).padStart(4),
        `${String(h.players).padStart(7)} ${String(h.directUp).padStart(6)}`,
        `${fmt(h.upMbit).padStart(14)} ${fmt(h.upMbitMax).padStart(5)}`,
        fmt(h.relayedUpMbit).padStart(10),
        fmt(h.payloadMbit).padStart(7),
        `${fmt(h.overhead)}x`.padStart(8),
        `${fmt(h.tickP95)} ms`.padStart(8),
        `${fmt(h.frameP50, 1)} ${fmt(h.frameP95, 1)} (${fmt(h.fps, 0)})`.padStart(19),
        fmt(h.busy).padStart(4),
        fmt(h.heapMb, 0).padStart(7),
      ].join(' | '),
    );
  }
  const p = result.players;
  console.log(
    `\nplayers: ${p.count}, anchor connect p50 ${fmt(p.connectMsP50, 0)} ms, max ${fmt(p.connectMsMax, 0)}; down ${fmt(p.downMbitMean)} Mbit/s mean; first snapshot per host p50 ${fmt(p.joinMsP50, 0)} ms, p95 ${fmt(p.joinMsP95, 0)}, max ${fmt(p.joinMsMax, 0)}; connect outcomes ${JSON.stringify(p.connectOutcomes)}`,
  );
  for (const e of p.errors) console.log(`  connectPeer error, host ${e.host}, after ${e.ms} ms: ${e.message}`);
  console.log('');
  for (const c of result.checks) console.log(`${c.ok ? 'PASS' : 'FAIL'} ${c.name} — ${c.detail}`);
}

// ── the page + API server ───────────────────────────────────────────────

const types = { '.html': 'text/html', '.mjs': 'text/javascript', '.js': 'text/javascript', '.wasm': 'application/wasm' };
const readBody = req =>
  new Promise((resolve, reject) => {
    let text = '';
    req.setEncoding('utf8').on('data', chunk => (text += chunk)).on('end', () => resolve(text)).on('error', reject);
  });
const json = (res, status, value) => {
  res.writeHead(status, { 'content-type': 'application/json' });
  res.end(JSON.stringify(value));
};
let anchorUrl = null;
const pages = createServer({ cert: readFileSync(certFile), key: readFileSync(keyFile) }, async (req, res) => {
  const path = decodeURIComponent(new URL(req.url, 'https://x').pathname);
  if (path.startsWith('/api/')) {
    const what = path.slice(5);
    if (what === 'config' && req.method === 'GET') return json(res, 200, { ...config, anchorUrl });
    if (what === 'state' && req.method === 'GET') return json(res, 200, { phase, ready: readyCount(), registered: { hosts: registered.host.length, players: registered.player.length } });
    if (what === 'hosts' && req.method === 'GET') return json(res, 200, { hosts: registered.host.map((node, index) => ({ node, index })) });
    if (req.method === 'POST' && (what === 'register' || what === 'report')) {
      let body;
      try {
        body = JSON.parse(await readBody(req));
      } catch {
        return json(res, 400, { error: 'not JSON' });
      }
      if (body.role !== 'host' && body.role !== 'player') return json(res, 400, { error: 'role' });
      if (what === 'register') {
        const list = registered[body.role];
        const known = list.indexOf(body.node);
        if (known >= 0) return json(res, 200, { index: known });
        const cap = body.role === 'host' ? config.hosts : config.players;
        if (list.length >= cap) return json(res, 409, { error: `already ${cap} ${body.role}s` });
        list.push(body.node);
        log(`${body.role} ${list.length - 1} registered: ${body.node}`);
        return json(res, 200, { index: list.length - 1 });
      }
      reports.push(body);
      latest.set(`${body.role}:${body.index}`, body);
      // Once per page: a failed page keeps reporting, without its error.
      const key = `${body.role}:${body.index}`;
      if (body.phase === 'failed' && body.error && !failedLogged.has(key)) {
        failedLogged.add(key);
        log(`${body.role} ${body.index} FAILED: ${body.error}`);
      }
      return json(res, 200, { phase });
    }
    return json(res, 404, { error: 'unknown' });
  }
  const [root, rel] = path.startsWith('/pkg/') ? [dist, path.slice(5)] : [join(here, 'page'), path.slice(1) || 'index.html'];
  const file = normalize(join(root, rel));
  if (!file.startsWith(root) || !existsSync(file)) {
    res.writeHead(404).end();
    return;
  }
  res.writeHead(200, { 'content-type': types[extname(file)] ?? 'application/octet-stream' });
  res.end(readFileSync(file));
});
const pagePort = num('page-port', 0);
await new Promise(resolve => pages.listen(pagePort, bindIp, resolve));
const origin = `https://${publicName}:${pages.address().port}`;

// ── the anchor: the real CLI ────────────────────────────────────────────

const listenPort = await freePort();
anchorUrl = `https://${publicName}:${listenPort}`;
const anchor = spawn(netMesh, [
  'anchor', 'serve',
  '--bind', `${bindIp}:0`,
  '--psk-file', pskFile,
  '--listen', `${bindIp}:${listenPort}`,
  '--url', anchorUrl,
  '--rtc-bind', `${arg('rtc-ip', bindIp === '0.0.0.0' ? publicName : bindIp)}:0`,
  '--tls-cert', certFile,
  '--tls-key', keyFile,
  '--allow-origin', origin,
  '--issuer-identity', issuerFile,
  '--insecure-permissions',
  '--game', GAME,
  '--game-stats-secs', '5',
  // A lab run puts every player tab of a machine behind ONE address. The
  // anchor charges offers per visitor and caps an address at 120 a minute,
  // which a machine of 64 tabs re-dialling during joins can exceed; raised
  // here for the test, and reported in the results.
  '--offers-per-minute', String(perAddressLimit),
  '--credentials-per-minute', String(perAddressLimit),
  '--output', 'ndjson',
], { stdio: ['ignore', 'pipe', 'pipe'] });
let report = null;
let tail = '';
anchor.stdout.setEncoding('utf8').on('data', chunk => {
  tail += chunk;
  const lines = tail.split('\n');
  tail = lines.pop();
  for (const line of lines) {
    if (!line.trim()) continue;
    try {
      const value = JSON.parse(line);
      if (value.game_stats) anchorStats = value.game_stats;
      else if (value.credential_endpoint !== undefined || value.node) report = value;
    } catch {
      log(`anchor: ${line}`);
    }
  }
});
anchor.stderr.setEncoding('utf8').on('data', chunk => process.stderr.write(`[anchor] ${chunk}`));

const deadline = Date.now() + 30_000;
while (!report && Date.now() < deadline) await sleep(100);
if (!report) throw new Error('the anchor never reported that it was serving');
log(`anchor ${report.node} at ${anchorUrl}; pages at ${origin}`);
log(`config ${JSON.stringify(config)}; measure ${measureSecs}s; budget ${budgetMbit} Mbit/s; target ${targetFps} fps`);
log(`other machines:\n  node run.mjs browsers --coordinator ${origin} --spki ${spkiPin} --hosts <k>\n  node run.mjs browsers --coordinator ${origin} --spki ${spkiPin} --players <k>`);

const local =
  localHosts + localPlayers > 0
    ? runBrowsers({ base: origin, pin: spkiPin, hosts: localHosts, players: localPlayers, chrome: arg('chrome'), headed: flag('headed'), staggerMs: num('stagger-ms', 0) }).catch(error => log(`local browsers: ${error.message}`))
    : null;

// Join → measure → verdict.
const joinDeadline = Date.now() + joinTimeoutSecs * 1000;
let exitCode = 2;
for (;;) {
  await sleep(1000);
  const ready = readyCount();
  if (phase === 'joining') {
    if (ready.hosts === config.hosts && ready.players === config.players) {
      // One report interval after everyone is ready, so the first window
      // reading is not the join burst.
      measureFrom = Date.now() + 2000;
      phase = 'measuring';
      log(`all ${config.hosts} hosts and ${config.players} players ready; measuring for ${measureSecs}s`);
    } else if (Date.now() > joinDeadline) {
      log(`join timed out: ${JSON.stringify(ready)} of ${config.hosts} hosts / ${config.players} players ready`);
      break;
    }
  } else if (Date.now() >= measureFrom + measureSecs * 1000) {
    const result = verdict();
    printVerdict(result);
    // A host missing from the window: say what it last reported, and when.
    for (let h = 0; h < config.hosts; h += 1) {
      if (result.hosts.some(row => row.index === h)) continue;
      const mine = reports.filter(r => r.role === 'host' && r.index === h);
      const last = mine.at(-1);
      log(`host ${h}: ${mine.length} report(s) in all; last ${last ? `${Math.round((Date.now() - last.at) / 1000)}s ago, phase ${last.phase}, wire ${last.wire ? 'yes' : 'no'}, error ${last.error ?? 'none'}` : 'never'}`);
    }
    mkdirSync(dirname(outFile), { recursive: true });
    writeFileSync(outFile, JSON.stringify({ config, measureSecs, budgetMbit, targetFps, perAddressLimit, at: new Date().toISOString(), anchorStats, ...result }, null, 2));
    log(`results: ${outFile}`);
    exitCode = result.pass ? 0 : 1;
    break;
  }
}
phase = 'done';
await sleep(3000); // let the browsers runners see `done`
await local;
anchor.kill();
pages.close();
process.exit(exitCode);
