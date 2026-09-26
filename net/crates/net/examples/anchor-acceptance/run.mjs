#!/usr/bin/env node
// Anchor acceptance run (browser plan P0, release step 2).
//
// Two real browser players join a lobby through a real
// `net-mesh anchor serve --game`, each with an anonymous credential the
// anchor issued — no hand-minted secret, no demo host. Plus two refusals
// only a real browser can show: a game the anchor does not admit, and a
// third identity presenting another player's credential.
//
//   node run.mjs --net-mesh <path to net-mesh binary> [--chrome <path>] [--headed]
//
// Needs: the built package (`browser-ts`: `npm run build`, which copies the
// leaf wasm next to the bundle), `openssl` on PATH, and `npm install` here
// (playwright-core). Exit code 0 only when every check passes.

import { spawn, execFileSync } from 'node:child_process';
import { createHash, randomBytes, X509Certificate } from 'node:crypto';
import { createServer } from 'node:http';
import { createServer as createNetServer } from 'node:net';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const here = dirname(fileURLToPath(import.meta.url));
const dist = join(here, '..', '..', 'browser-ts', 'dist');
const work = join(here, 'work');
const GAME = 'acceptance';

const args = process.argv.slice(2);
const arg = name => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : undefined;
};
const netMesh = arg('--net-mesh');
const chrome = arg('--chrome');
const headed = args.includes('--headed');
if (!netMesh) throw new Error('pass --net-mesh <path to the net-mesh binary>');
if (!existsSync(join(dist, 'index.bundle.js')) || !existsSync(join(dist, 'net_leaf_bg.wasm'))) {
  throw new Error(`build the package first: ${dist} has no bundle or leaf wasm`);
}

const log = line => console.log(`[acceptance] ${line}`);
const checks = [];
const check = (name, ok, detail) => {
  checks.push({ name, ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ` — ${detail}` : ''}`);
};

async function freePort() {
  return new Promise((resolve, reject) => {
    const server = createNetServer();
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
    server.on('error', reject);
  });
}

// --- secrets and a certificate -------------------------------------------
rmSync(work, { recursive: true, force: true });
mkdirSync(work, { recursive: true });
const pskFile = join(work, 'psk.hex');
writeFileSync(pskFile, randomBytes(32).toString('hex'));
const issuerFile = join(work, 'issuer.toml');
execFileSync(netMesh, ['identity', 'generate', '--out', issuerFile], { stdio: 'ignore' });
const certFile = join(work, 'cert.pem');
const keyFile = join(work, 'key.pem');
execFileSync('openssl', [
  'req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:prime256v1', '-nodes',
  '-keyout', keyFile, '-out', certFile, '-days', '2', '-subj', '/CN=localhost',
  '-addext', 'subjectAltName=DNS:localhost,IP:127.0.0.1',
], { stdio: 'ignore' });
// The browser trusts THIS certificate's key only, by SPKI pin; no trust
// store is touched and verification stays on for everything else.
const spki = new X509Certificate(readFileSync(certFile)).publicKey.export({ type: 'spki', format: 'der' });
const spkiPin = createHash('sha256').update(spki).digest('base64');

// --- the page server --------------------------------------------------------
const types = { '.html': 'text/html', '.mjs': 'text/javascript', '.js': 'text/javascript', '.wasm': 'application/wasm' };
const pages = createServer((req, res) => {
  const path = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  const [root, rel] = path.startsWith('/pkg/') ? [dist, path.slice(5)] : [join(here, 'page'), path.slice(1) || 'index.html'];
  const file = normalize(join(root, rel));
  if (!file.startsWith(root) || !existsSync(file)) {
    res.writeHead(404).end();
    return;
  }
  res.writeHead(200, { 'content-type': types[extname(file)] ?? 'application/octet-stream' });
  res.end(readFileSync(file));
});
await new Promise(resolve => pages.listen(0, '127.0.0.1', resolve));
const origin = `http://localhost:${pages.address().port}`;

// --- the anchor: the real CLI --------------------------------------------
const listenPort = await freePort();
const anchorUrl = `https://localhost:${listenPort}`;
const anchor = spawn(netMesh, [
  'anchor', 'serve',
  '--bind', '127.0.0.1:0',
  '--psk-file', pskFile,
  '--listen', `127.0.0.1:${listenPort}`,
  '--url', anchorUrl,
  '--rtc-bind', '127.0.0.1:0',
  '--tls-cert', certFile,
  '--tls-key', keyFile,
  '--allow-origin', origin,
  '--issuer-identity', issuerFile,
  '--insecure-permissions',
  '--game', GAME,
  '--game-stats-secs', '1',
  '--output', 'ndjson',
], { stdio: ['ignore', 'pipe', 'pipe'] });
let report = null;
let stats = null;
let stdoutTail = '';
anchor.stdout.setEncoding('utf8').on('data', chunk => {
  stdoutTail += chunk;
  const lines = stdoutTail.split('\n');
  stdoutTail = lines.pop();
  for (const line of lines) {
    if (!line.trim()) continue;
    try {
      const value = JSON.parse(line);
      if (value.game_stats) stats = value.game_stats;
      else if (value.credential_endpoint !== undefined || value.node) report = value;
    } catch {
      log(`anchor: ${line}`);
    }
  }
});
anchor.stderr.setEncoding('utf8').on('data', chunk => process.stderr.write(`[anchor] ${chunk}`));
const anchorExited = new Promise(resolve => anchor.on('exit', code => resolve(code)));

let browser;
async function cleanup() {
  await browser?.close().catch(() => {});
  anchor.kill();
  pages.close();
}

try {
  const deadline = Date.now() + 30_000;
  while (!report && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 100));
  if (!report) throw new Error('the anchor never reported that it was serving');
  log(`anchor ${report.node} at ${anchorUrl}; games ${JSON.stringify(report.games)}`);
  check('the anchor reports its game and credential endpoint',
    JSON.stringify(report.games) === JSON.stringify([GAME]) && report.credential_endpoint === `${anchorUrl}/credential`,
    `games=${JSON.stringify(report.games)} endpoint=${report.credential_endpoint}`);

  browser = await chromium.launch({
    headless: !headed,
    ...(chrome ? { executablePath: chrome } : {}),
    // Every player is a live, foreground tab: a backgrounded page's timers
    // are throttled, and the leaf answers a peer's relayed handshake from
    // a timer-driven pump (the demo's driver passes the same three).
    args: [
      '--no-sandbox',
      '--disable-background-timer-throttling',
      '--disable-renderer-backgrounding',
      '--disable-backgrounding-occluded-windows',
      `--ignore-certificate-errors-spki-list=${spkiPin}`,
    ],
  });
  log(`browser ${browser.version()}`);

  // One isolated context per player: separate storage, separate identity.
  async function player(name, query, timeoutMs = 180_000, reuseContext = undefined) {
    const context = reuseContext ?? (await browser.newContext());
    const page = await context.newPage();
    page.on('console', m => log(`[${name}] ${m.type()}: ${m.text()}`));
    page.on('pageerror', e => log(`[${name}] pageerror: ${e.message}`));
    const url = `${origin}/?${new URLSearchParams({ anchor: anchorUrl, game: GAME, ...query })}`;
    await page.goto(url);
    const handle = {
      page,
      context,
      result: async () => page.evaluate(() => globalThis.__acceptance ?? null),
      finished: async () => {
        const until = Date.now() + timeoutMs;
        for (;;) {
          const result = await handle.result();
          if (result?.done) return result;
          if (Date.now() > until) return { ...(result ?? {}), done: false, timedOut: true };
          await new Promise(resolve => setTimeout(resolve, 500));
        }
      },
    };
    return handle;
  }

  const host = await player('host', { role: 'host' });
  const join = await player('join', { role: 'join' });
  const [hostResult, joinResult] = await Promise.all([host.finished(), join.finished()]);
  const stepsOf = r => (r.steps ?? []).map(s => (s.name === 'error' || s.name.startsWith('peer') ? `${s.name}(${JSON.stringify(s)})` : s.name)).join(' → ');
  check('the host fetched a credential, enrolled and opened a lobby',
    hostResult.ok && hostResult.steps.some(s => s.name === 'enrolled'), stepsOf(hostResult));
  check('a second player found it in the list, joined and was seated',
    joinResult.ok && joinResult.steps.some(s => s.name === 'sat'), stepsOf(joinResult));

  const unknown = await (await player('unknown', { role: 'unknown-game' })).finished();
  check('a game the anchor does not admit is refused typed', unknown.ok,
    JSON.stringify(unknown.steps?.find(s => s.name === 'refused') ?? unknown));

  const reuse = await (await player('reuse', {
    role: 'reuse',
    credential: hostResult.credentialB64 ?? '',
    bootstrap: hostResult.bootstrapUrl ?? '',
  }, 60_000)).finished();
  check("a third identity presenting another player's credential is not enrolled", reuse.ok, stepsOf(reuse));

  // A reload: the joiner's page closes and reopens in the SAME browser
  // context (same storage, so the same remembered player) with the
  // credential it was first given. The invite is bound to this device,
  // so it enrolls again — as the same node.
  await join.page.close();
  const back = await (await player('return', {
    role: 'return',
    credential: joinResult.credentialB64 ?? '',
    bootstrap: joinResult.bootstrapUrl ?? '',
  }, 90_000, join.context)).finished();
  check('a reloaded player comes back as the same node and re-enrolls with its credential',
    back.ok && !!joinResult.node && back.node === joinResult.node,
    `before=${joinResult.node} after=${back.node} ${stepsOf(back)}`);

  // The anchor's own counters, from its --game-stats-secs line.
  await new Promise(resolve => setTimeout(resolve, 1500));
  const game = stats?.find(s => s.game === GAME);
  check('the anchor counted two credentials, three enrollments (one a return) and the refused reuse',
    !!game && game.credentials_issued === 2 && game.enrollments_admitted >= 3 && game.enrollments_refused >= 1,
    JSON.stringify(game));
} catch (error) {
  check('the run completed', false, String(error?.stack ?? error));
} finally {
  await cleanup();
  await Promise.race([anchorExited, new Promise(resolve => setTimeout(resolve, 3000))]);
}

const failed = checks.filter(c => !c.ok).length;
console.log(`\n${checks.length - failed}/${checks.length} checks passed`);
process.exit(failed === 0 ? 0 : 1);
