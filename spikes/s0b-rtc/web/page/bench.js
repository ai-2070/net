// S0c — double-AEAD cost measurement.
//
// Loaded by app.js when the page is opened with `?bench=1`. Reuses the
// S0b signalling, driver and session machinery; only the measurement
// loops are new.
//
// Three configurations per workload:
//   A  S0a PacketBuilder (ChaCha20-Poly1305 seal) -> dc.send   [the design]
//   B  same real 68-byte NetHeader, payload copied, no AEAD -> dc.send
//      [the DTLS-exporter shortcut, approximated]
//   C  S0a PacketBuilder -> discard                            [encoder alone]
//
// Every packet carries its 4-byte sequence at the head of its payload so
// the native ack can be matched back to the send timestamp.

const TAG_BENCH_SYNC = 0x10;
const TAG_BENCH_SYNC_ACK = 0x11;
const TAG_BENCH_A = 0x12;
const TAG_BENCH_B = 0x13;
const TAG_BENCH_ACK = 0x14;
const TAG_BENCH_RX_START = 0x15;
const TAG_BENCH_RX_A = 0x16;
const TAG_BENCH_RX_B = 0x17;
const TAG_BENCH_RX_DONE = 0x18;

export function makeBench(deps) {
  const { log, post, sleep, tagged, establish, getConfig } = deps;

  const sleepUntil = async (t) => {
    const d = t - performance.now();
    if (d > 1) await sleep(d);
    while (performance.now() < t) {
      /* final sub-ms spin so pacing is not setTimeout-quantised */
    }
  };

  function pct(xs, p) {
    if (!xs.length) return NaN;
    const s = [...xs].sort((a, b) => a - b);
    const i = Math.min(s.length - 1, Math.max(0, Math.ceil((p / 100) * s.length) - 1));
    return s[i];
  }

  const f = (x, n = 1) => (Number.isFinite(x) ? x.toFixed(n) : 'n/a');

  async function stats(path) {
    const text = await (await fetch(path)).text();
    const out = {};
    for (const line of text.trim().split('\n')) {
      const [k, v] = line.split('=');
      out[k] = Number(v);
    }
    return out;
  }

  /// Chrome clamps `performance.now()` to 100 µs for a page that is
  /// not cross-origin isolated, so a single packet build (single-digit
  /// µs) is unmeasurable directly. Time batches of `k` builds instead
  /// and report the per-build mean of each batch; resolution becomes
  /// 100/k µs.
  function buildBatches(ep, payload, config, k = 200, m = 100) {
    const out = [];
    for (let i = 0; i < m; i++) {
      const t0 = performance.now();
      for (let j = 0; j < k; j++) {
        if (config === 'B') ep.build_plain(payload);
        else ep.build_packet(payload);
      }
      out.push(((performance.now() - t0) * 1000) / k); // µs per build
    }
    return out;
  }

  /// Estimate `nativeMicros -> performance.now()` offset with 20
  /// round trips, keeping the sample with the smallest RTT (the least
  /// asymmetric one). Returns { offsetMs, minRttMs }.
  async function clockSync(dc, handlers) {
    const samples = [];
    for (let i = 0; i < 20; i++) {
      const seq = i;
      const t0 = performance.now();
      const got = new Promise((res) => {
        handlers.sync = (nativeUs) => res(nativeUs);
      });
      const body = new Uint8Array(4);
      new DataView(body.buffer).setUint32(0, seq, true);
      dc.send(tagged(TAG_BENCH_SYNC, body));
      const nativeUs = await got;
      const t3 = performance.now();
      samples.push({ rtt: t3 - t0, offset: nativeUs / 1000 - (t0 + t3) / 2 });
      await sleep(5);
    }
    samples.sort((a, b) => a.rtt - b.rtt);
    return { offsetMs: samples[0].offset, minRttMs: samples[0].rtt };
  }

  /// Install one bench-wide message handler on the channel.
  function attach(dc) {
    const handlers = { sync: null, ack: null, rx: null, done: null };
    dc.binaryType = 'arraybuffer';
    dc.onmessage = (ev) => {
      const b = new Uint8Array(ev.data);
      const dv = new DataView(ev.data);
      switch (b[0]) {
        case TAG_BENCH_SYNC_ACK:
          if (handlers.sync) handlers.sync(Number(dv.getBigUint64(5, true)));
          break;
        case TAG_BENCH_ACK:
          if (handlers.ack) handlers.ack(dv.getUint32(1, true), Number(dv.getBigUint64(5, true)));
          break;
        case TAG_BENCH_RX_A:
        case TAG_BENCH_RX_B:
          if (handlers.rx) handlers.rx(b[0], b.subarray(1));
          break;
        case TAG_BENCH_RX_DONE:
          if (handlers.done) handlers.done(dv.getUint32(1, true));
          break;
        default:
          break;
      }
    };
    return handlers;
  }

  async function teardown(s) {
    await post('/close/' + s.sid);
    s.pc.close();
    await sleep(200);
  }

  // -------------------------------------------------------------------
  // Send-direction cell: browser -> native
  // -------------------------------------------------------------------

  async function sendCell(cfg, { run, workload, config, size, rateHz, durationMs }) {
    const s = await establish(cfg, 'answerer', { quiet: true });
    const handlers = attach(s.dc);
    await post('/bench-reset');

    let offsetMs = 0;
    let minRttMs = NaN;
    if (config !== 'C') {
      ({ offsetMs, minRttMs } = await clockSync(s.dc, handlers));
    }

    const n = Math.round((durationMs / 1000) * rateHz);
    const payload = new Uint8Array(size);
    payload.fill(0x5a);
    const view = new DataView(payload.buffer);

    // Per-build distribution, measured in batches (see buildBatches).
    const buildUs = buildBatches(s.ep, payload, config);
    const build = []; // ms, per packet (quantised; used only for the sum)
    const oneWay = []; // ms, browser-send -> native-decrypt
    const sendAt = new Float64Array(n);
    let acks = 0;
    handlers.ack = (seq, nativeUs) => {
      if (seq >= n) return;
      acks++;
      oneWay.push(nativeUs / 1000 - offsetMs - sendAt[seq]);
    };

    let mainThreadMs = 0;
    const tag = config === 'B' ? TAG_BENCH_B : TAG_BENCH_A;
    const t0 = performance.now();
    for (let i = 0; i < n; i++) {
      await sleepUntil(t0 + (i * 1000) / rateHz);
      view.setUint32(0, i, true);
      const a = performance.now();
      const pkt = config === 'B' ? s.ep.build_plain(payload) : s.ep.build_packet(payload);
      const b = performance.now();
      if (config !== 'C') {
        sendAt[i] = b;
        s.dc.send(tagged(tag, pkt));
      }
      const c = performance.now();
      build.push(b - a);
      mainThreadMs += c - a;
    }
    const wallMs = performance.now() - t0;
    await sleep(800); // let the last acks land

    const bs = await stats('/bench-stats');
    const ps = await stats('/probe-stats');
    const nativeBytes = config === 'B' ? bs.b_payload_bytes : bs.a_payload_bytes;
    const nativePkts = config === 'B' ? bs.b_packets : bs.a_packets;
    const nativeNs = config === 'B' ? bs.b_parse_ns : bs.a_decrypt_ns;
    const mb = (size * n) / 1048576;

    await log(
      `S0C ${workload} run=${run} cfg=${config} n=${n} size=${size} rate=${rateHz} ` +
        `wall_s=${f(wallMs / 1000, 2)} ` +
        `build_us_med=${f(pct(buildUs, 50), 2)} p95=${f(pct(buildUs, 95), 2)} ` +
        `p99=${f(pct(buildUs, 99), 2)} ` +
        `lat_ms_med=${f(pct(oneWay, 50), 3)} p95=${f(pct(oneWay, 95), 3)} ` +
        `p99=${f(pct(oneWay, 99), 3)} acks=${acks} min_rtt_ms=${f(minRttMs, 3)} ` +
        `mt_ms_per_s=${f(mainThreadMs / (wallMs / 1000), 3)} mt_ms_per_mb=${f(mainThreadMs / mb, 2)} ` +
        `native_pkts=${nativePkts} native_bytes=${nativeBytes} ` +
        `native_failed=${config === 'B' ? bs.b_failed : bs.a_failed} ` +
        `native_thruput_MBps=${f(nativeBytes / 1048576 / (bs.window_us / 1e6), 3)} ` +
        `native_cost_us_per_pkt=${f(nativeNs / 1000 / Math.max(1, nativePkts), 2)} ` +
        `admission_refusals=${ps.admission_refusals} write_false=${ps.write_false} ` +
        `max_buffered=${ps.max_buffered}`,
    );
    await teardown(s);
  }

  /// Unpaced send: how much can the browser actually push through the
  /// channel with config A vs B? The paced bulk cell answers "does
  /// 1 MB/s cost much"; this answers "is 1 MB/s even the ceiling".
  /// Backs off on `dc.bufferedAmount` so the SCTP queue is not the
  /// thing being measured.
  async function saturateCell(cfg, { run, config, size, durationMs }) {
    const s = await establish(cfg, 'answerer', { quiet: true });
    attach(s.dc);
    await post('/bench-reset');

    const payload = new Uint8Array(size);
    payload.fill(0x5a);
    const view = new DataView(payload.buffer);
    const tag = config === 'B' ? TAG_BENCH_B : TAG_BENCH_A;
    let sent = 0;
    let mainThreadMs = 0;
    let backoffs = 0;
    const t0 = performance.now();
    while (performance.now() - t0 < durationMs) {
      if (s.dc.bufferedAmount > 1000000) {
        backoffs++;
        await sleep(2);
        continue;
      }
      const a = performance.now();
      view.setUint32(0, sent & 0xffffffff, true);
      const pkt = config === 'B' ? s.ep.build_plain(payload) : s.ep.build_packet(payload);
      s.dc.send(tagged(tag, pkt));
      mainThreadMs += performance.now() - a;
      sent++;
      if ((sent & 0x3f) === 0) await sleep(0); // let the event loop breathe
    }
    const wallMs = performance.now() - t0;
    await sleep(1500);
    const bs = await stats('/bench-stats');
    const ps = await stats('/probe-stats');
    const bytes = config === 'B' ? bs.b_payload_bytes : bs.a_payload_bytes;
    const mb = bytes / 1048576;
    await log(
      `S0C saturate run=${run} cfg=${config} size=${size} wall_s=${f(wallMs / 1000, 2)} ` +
        `sent=${sent} native_pkts=${config === 'B' ? bs.b_packets : bs.a_packets} ` +
        `native_MBps=${f(mb / (bs.window_us / 1e6), 2)} browser_offered_MBps=${f((sent * size) / 1048576 / (wallMs / 1000), 2)} ` +
        `mt_ms_per_s=${f(mainThreadMs / (wallMs / 1000), 1)} mt_ms_per_mb=${f(mainThreadMs / Math.max(mb, 1e-9), 2)} ` +
        `backoffs=${backoffs} admission_refusals=${ps.admission_refusals} write_false=${ps.write_false} ` +
        `max_buffered=${ps.max_buffered}`,
    );
    await teardown(s);
  }

  // -------------------------------------------------------------------
  // Receive-direction cell: native -> browser
  // -------------------------------------------------------------------

  async function recvCell(cfg, { run, config, size, rateHz, durationMs }) {
    const s = await establish(cfg, 'answerer', { quiet: true });
    const handlers = attach(s.dc);
    await post('/bench-reset');

    const cost = [];
    let bytes = 0;
    let packets = 0;
    let failures = 0;
    let cpuMs = 0;
    handlers.rx = (_tag, body) => {
      const a = performance.now();
      try {
        const out = config === 'B' ? s.ep.parse_plain(body) : s.ep.open_packet(body);
        const b = performance.now();
        cost.push(b - a);
        cpuMs += b - a;
        bytes += config === 'B' ? out : out.length;
        packets++;
      } catch (e) {
        failures++;
      }
    };
    const finished = new Promise((res) => {
      handlers.done = (sent) => res(sent);
    });

    const body = new Uint8Array(13);
    const dv = new DataView(body.buffer);
    body[0] = config === 'B' ? 0x42 : 0x41; // 'B' / 'A'
    dv.setUint32(1, size, true);
    dv.setUint32(5, rateHz, true);
    dv.setUint32(9, durationMs, true);
    const t0 = performance.now();
    s.dc.send(tagged(TAG_BENCH_RX_START, body));
    const sent = await finished;
    await sleep(500);
    const wallMs = performance.now() - t0;
    const mb = bytes / 1048576;

    await log(
      `S0C rx run=${run} cfg=${config} size=${size} rate=${rateHz} ` +
        `wall_s=${f(wallMs / 1000, 2)} native_sent=${sent} received=${packets} failures=${failures} ` +
        `bytes=${bytes} thruput_MBps=${f(mb / (wallMs / 1000), 3)} ` +
        `cost_us_med=${f(pct(cost, 50) * 1000, 2)} p95=${f(pct(cost, 95) * 1000, 2)} ` +
        `p99=${f(pct(cost, 99) * 1000, 2)} ` +
        `cpu_ms_per_mb=${f(cpuMs / Math.max(mb, 1e-9), 2)} cpu_ms_total=${f(cpuMs, 1)}`,
    );
    await teardown(s);
  }

  // -------------------------------------------------------------------

  async function run() {
    // `?dur=<ms>&runs=<n>` shorten the matrix for a smoke test; the
    // reported figures always come from the defaults (30 s, 3 runs).
    const qs = new URLSearchParams(location.search);
    const durationMs = Number(qs.get('dur') || 30000);
    const runs = Number(qs.get('runs') || 3);
    const cfg = await getConfig();
    await log(`S0C INFO userAgent=${navigator.userAgent}`);
    await log(
      `S0C INFO hardwareConcurrency=${navigator.hardwareConcurrency} ` +
        `deviceMemory=${navigator.deviceMemory ?? 'n/a'}`,
    );

    for (let r = 1; r <= runs; r++) {
      // Workload 1 — 60 Hz x 1 KiB for 30 s.
      for (const config of ['A', 'B', 'C']) {
        await sendCell(cfg, {
          run: r,
          workload: '60hz',
          config,
          size: 1024,
          rateHz: 60,
          durationMs,
        });
      }
      // Workload 2 — 1 MB/s bulk for 30 s (8 KiB x 128/s).
      for (const config of ['A', 'B']) {
        await sendCell(cfg, {
          run: r,
          workload: 'bulk',
          config,
          size: 8000,
          rateHz: 131,
          durationMs,
        });
      }
      // Unpaced ceiling, 10 s each — the bulk threshold needs a
      // number that is not pacing-limited.
      for (const config of ['A', 'B']) {
        await saturateCell(cfg, { run: r, config, size: 8000, durationMs: 10000 });
      }
      // Receive direction — native -> browser at 1 MB/s.
      for (const config of ['A', 'B']) {
        await recvCell(cfg, { run: r, config, size: 8000, rateHz: 131, durationMs });
      }
    }
  }

  return { run };
}
