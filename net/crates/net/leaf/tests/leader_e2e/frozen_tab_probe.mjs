// Does a frozen tab let go of its Web Lock?
//
// This probe exists because D2's stale-leader paragraph says of a
// resumed tab "its lock is gone, **and** any message it emits carries
// a stale generation". Those are two mechanisms, and the leaf's
// correctness must not rest on the first one unless it is actually
// true: a document frozen by `Page.setWebLifecycleState` is not
// required by any specification to release its locks, and if it keeps
// them then "the lock is gone" is false and the generation is the only
// thing standing between a resumed tab and acting as leader.
//
// So this measures it, on the engine Stage 5 targets, with two real
// tabs and real CDP freezing — no wasm, no anchor, nothing from the
// leaf. It is the evidence behind the D2 correction in the report, and
// it is deliberately separate from `net/crates/net/tests/rtc_browser/`
// (the merged Stage 5 runner, which MergedRunner owns and which runs
// the full two-tab witness against a native anchor).
//
// Run:
//   node net/crates/net/leaf/tests/leader_e2e/frozen_tab_probe.mjs
//
// Needs `playwright-core` and a Chromium. Both are resolved from the
// merged runner's driver install, read-only, so this probe adds no
// dependency of its own:
//   PW=net/crates/net/tests/rtc_browser/driver/node_modules
//   CHROMIUM=<a Chromium or Chrome executable>

import http from 'node:http';
import path from 'node:path';
import { createRequire } from 'node:module';

const HERE = path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1'));
const DRIVER = path.resolve(HERE, '../../../tests/rtc_browser/driver');
const require = createRequire(path.join(DRIVER, 'package.json'));
const { chromium } = require('playwright-core');

const CHROMIUM =
  process.env.CHROMIUM ??
  'C:/Users/chief/AppData/Local/ms-playwright/chromium-1194/chrome-win/chrome.exe';

// `navigator.locks` and IndexedDB need a secure context. `http://127.0.0.1`
// is one ("potentially trustworthy origin"), which is why this serves
// over loopback rather than opening a `file://` page.
const PAGE = `<!doctype html>
<meta charset="utf-8">
<title>frozen tab probe</title>
<script>
const DB = 'frozen-tab-probe';
const SCOPE = 'frozen-tab-probe/lock';

// A lock held for as long as the returned promise is pending — the
// same shape the leaf's WebLock uses, because it is the only shape the
// API offers.
window.held = null;
window.takeLock = (ifAvailable) =>
  new Promise((settle) => {
    navigator.locks
      .request(SCOPE, { mode: 'exclusive', ifAvailable }, (lock) => {
        if (!lock) {
          settle({ granted: false });
          return new Promise((r) => r());
        }
        return new Promise((release) => {
          window.held = release;
          settle({ granted: true });
        });
      })
      .catch((error) => settle({ granted: false, error: String(error) }));
  });

window.releaseLock = () => {
  if (window.held) {
    window.held();
    window.held = null;
  }
};

const open = () =>
  new Promise((ok, bad) => {
    const request = indexedDB.open(DB, 1);
    request.onupgradeneeded = () => request.result.createObjectStore('leader');
    request.onsuccess = () => ok(request.result);
    request.onerror = () => bad(request.error);
  });

// Read-increment-write inside ONE readwrite transaction, exactly as
// storage::IdentityVault::next_generation does.
window.nextGeneration = async () => {
  const db = await open();
  return await new Promise((ok, bad) => {
    const tx = db.transaction('leader', 'readwrite');
    const store = tx.objectStore('leader');
    const get = store.get('generation');
    get.onsuccess = () => {
      const current = get.result === undefined ? 0 : Number(get.result);
      const next = current + 1;
      const put = store.put(String(next), 'generation');
      put.onsuccess = () => {
        // This tab's own belief, readable from the tab itself after a
        // freeze/resume cycle — the probe must not be the one
        // remembering it.
        window.myGeneration = next;
        ok(next);
      };
      put.onerror = () => bad(put.error);
    };
    get.onerror = () => bad(get.error);
  });
};

window.currentGeneration = async () => {
  const db = await open();
  return await new Promise((ok, bad) => {
    const get = db.transaction('leader', 'readonly').objectStore('leader').get('generation');
    get.onsuccess = () => ok(get.result === undefined ? 0 : Number(get.result));
    get.onerror = () => bad(get.error);
  });
};
</script>
<body>frozen tab probe</body>
`;

const server = http.createServer((_req, res) => {
  res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
  res.end(PAGE);
});
await new Promise((ok) => server.listen(0, '127.0.0.1', ok));
const url = `http://127.0.0.1:${server.address().port}/`;

const browser = await chromium.launch({
  headless: true,
  executablePath: CHROMIUM,
  args: [
    '--no-sandbox',
    '--disable-gpu',
    // Without this Chromium may refuse to freeze a page it considers
    // foreground; the probe must actually freeze to mean anything.
    '--enable-features=PageFreezingFromRenderer',
  ],
});
const context = await browser.newContext();
const findings = {};

async function lifecycle(page, state) {
  const session = await context.newCDPSession(page);
  try {
    await session.send('Page.enable');
    await session.send('Page.setWebLifecycleState', { state });
    // The command's acknowledgement is not the effect: a no-op
    // freeze used to sail straight on to a verdict below. Read the
    // lifecycle state back and report it, and nothing is concluded
    // from an unverified freeze.
    const read = await session.send('Page.getWebLifecycleState');
    return read.state === state;
  } finally {
    await session.detach().catch(() => {});
  }
}

try {
  const leader = await context.newPage();
  await leader.goto(url);
  const follower = await context.newPage();
  await follower.goto(url);

  // 1. The leader takes the lock and the generation.
  findings.leaderTookLock = await leader.evaluate(() => window.takeLock(true));
  findings.leaderGeneration = await leader.evaluate(() => window.nextGeneration());

  // 2. A second tab cannot have it. This is "two tabs share one
  //    identity without evicting each other" in its rawest form.
  findings.followerBlockedWhileLeaderLive = await follower.evaluate(() =>
    window.takeLock(true),
  );

  // 3. Freeze the leader, then ask again. THE question. The freeze
  //    is VERIFIED (the lifecycle state read back off the page) — a
  //    no-op freeze must not reach a verdict at all.
  findings.leaderFreezeVerified = await lifecycle(leader, 'frozen');
  await new Promise((r) => setTimeout(r, 500));
  findings.followerAfterLeaderFrozen = await follower.evaluate(() => window.takeLock(true));

  // 4. Resume the leader (verified the same way), and read what IT
  //    believes FROM THE TAB — not an echo of the probe's own
  //    pre-freeze reading, which was true by construction.
  findings.leaderResumeVerified = await lifecycle(leader, 'active');
  findings.resumedLeaderBelievesGeneration = await leader.evaluate(() => window.myGeneration);
  findings.generationInStorageAfterResume = await follower.evaluate(() =>
    window.currentGeneration(),
  );

  // 5. The successor has acted — in its own right and in its own
  //    step: the read-increment-write the new leader performs. The
  //    fence row below performs NO write at all, so the store move it
  //    measures is an independent writer's act, observed post-hoc —
  //    not a delta the fence row caused and then read back one line
  //    later, where the compared values agree by construction and
  //    the row can never report anything else.
  findings.successorGeneration = await follower.evaluate(() => window.nextGeneration());

  // 6. The fence — asserted strictly as what it measures: the
  //    tab-side bookkeeping the generation fence rests on, that the
  //    resumed tab's cached generation LAGS the counter in storage.
  //    Both sides are read fresh after the successor's move, and
  //    neither is the successor's increment result (which is above
  //    every prior reading by construction). It can fail on
  //    staleness: a resumed tab whose belief tracked the store — a
  //    vault that re-read the counter on resume — reads equal here
  //    and this row reports false, refuting the staleness claim.
  findings.generationInStorageNow = await follower.evaluate(() => window.currentGeneration());
  findings.resumedTabBelievesNow = await leader.evaluate(() => window.myGeneration);
  findings.resumedTabIsStale =
    findings.resumedTabBelievesNow < findings.generationInStorageNow;
  // The control, the same predicate applied to the tab that produced
  // the move: false, because a writer is current with what it just
  // wrote. This pair is what makes `resumedTabIsStale` a staleness
  // reading rather than a tautology — the predicate demonstrably
  // does report false for a tab that is not stale.
  findings.successorTabIsStale =
    findings.successorGeneration < findings.generationInStorageNow;

  findings.verdict = !findings.leaderFreezeVerified
    ? 'INCONCLUSIVE on this engine: the freeze never took effect'
    : findings.followerAfterLeaderFrozen.granted
      ? 'a frozen tab RELEASES its Web Lock on this engine'
      : 'a frozen tab KEEPS its Web Lock on this engine — the lock alone cannot fence it';

  console.log(JSON.stringify(findings, null, 2));
} finally {
  await browser.close();
  await new Promise((ok) => server.close(ok));
}
