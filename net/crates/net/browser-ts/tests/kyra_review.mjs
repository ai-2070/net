import fs from 'node:fs';
import vm from 'node:vm';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
// ADAPTATION 2 (`streamResult`'s wrapper, below): the wrapper the
// reviewer wrote reports no peer, and 6256f970b made that
// FAIL-CLOSED on purpose — a stream whose frames cannot be attributed
// is refused at construction rather than falling back to id-only
// admission, which was the cross-peer admixture on every proxied
// handle. The wrapper here now names a peer and an incarnation. The
// property these two probes assert — what the byte callback hands to
// the iterator — is untouched by that, and the refusal itself is
// witnessed in `abi_real_package.mjs`
// (`real_package_refuses_a_built_in_stream_that_spells_no_peer`) and
// in `test/store/...`/`leaf/tests/establishment_identity.rs`.
//
// ADAPTATION 1: the reviewer's absolute worktree path,
// `C:/Users/chief/orca/workspaces/net/kyra-stage5-9f8bde0c7`, cannot
// resolve in CI or in this repository. Derived from this file's own
// location instead — it sits at
// <repo>/net/crates/net/browser-ts/tests/. Every assertion below is
// verbatim.
const repo = fileURLToPath(new URL('../../../../../', import.meta.url)).replace(/[\\/]+$/, '');
const records = [];
async function probe(name, fn) {
  try { await fn(); records.push({ name, pass: true }); }
  catch (e) { records.push({ name, pass: false, error: e.message }); }
}
const { LeafStream } = await import(pathToFileURL(repo + '/net/crates/net/browser-ts/dist/stream.js'));
async function streamResult(payload) {
  let emit;
  const stream = new LeafStream({
    send() {}, close() {}, is_reliable() { return true; }, stream_id_hex() { return '9'; },
    on_message(cb) { emit = cb; },
    // See ADAPTATION 2 at the top of this file.
    peer_node_hex() { return '000000000000000a'; },
    incarnation() { return '1'; },
  });
  const next = stream[Symbol.asyncIterator]().next();
  emit(payload);
  const result = await next;
  stream.close();
  return result.value;
}
await probe('typescript_byte_callback_control', async () => {
  const data = await streamResult(new Uint8Array([1, 2]));
  if (!(data instanceof Uint8Array) || data[0] !== 1 || data[1] !== 2) throw new Error('byte control failed');
});
await probe('typescript_real_rust_callback_shape', async () => {
  // Rust wasm.rs:597-610 and leader_session.rs:1309-1321 emit JSON.
  // This is a boundary-shape probe, not real browser execution.
  // ADAPTATION 2 again: the event carries `peer_node` now. A stream
  // that knows its peer drops a frame that does not name it — "some
  // peer's bytes under my id" is the thing that had to stop being
  // deliverable. `000000000000000a` is decimal 10.
  const data = await streamResult(
    '{"type":"stream_data","stream_id":"9","peer_node":"10","seq":"1","payload":"AQI="}',
  );
  if (!(data instanceof Uint8Array) || data[0] !== 1 || data[1] !== 2) throw new Error(`promised bytes, received ${typeof data}`);
});
const source = fs.readFileSync(repo + '/net/crates/net/tests/rtc_browser/driver/driver.mjs', 'utf8');
const helper = source.slice(source.indexOf('function seedFirefoxProfile('), source.indexOf('// Chromium\'s flags'));
const tls = source.slice(source.indexOf('async function opTlsProbe('), source.indexOf('\nconst OPS ='));
if (!helper.trim().endsWith('}') || !tls.trim().endsWith('}')) throw new Error('exact-source extraction boundaries changed');
let launches = 0;
const context = vm.createContext({
  fs: { mkdirSync() {}, existsSync() { return true; }, mkdtempSync() { return '/simulated/profile'; }, rmSync() {} },
  path, os: { tmpdir() { return '/simulated'; } }, execFileSync() {}, log() {},
  ENGINES: { firefox: { async launchPersistentContext() { launches++; return { async newPage() { return { async goto() { return { status() { return 200; } }; } }; }, async close() {} }; } } }
});
vm.runInContext(helper + '\n' + tls, context);
await probe('firefox_unseeded_launch_control', async () => {
  await vm.runInContext('opTlsProbe({engine:"firefox",url:"https://test.invalid"})', context);
  if (launches !== 1) throw new Error('launch control failed');
});
await probe('firefox_successfully_seeded_profile_reaches_launch', async () => {
  await vm.runInContext('opTlsProbe({engine:"firefox",url:"https://test.invalid",caPemPath:"simulated.pem",caNickname:"test"})', context);
  if (launches !== 2) throw new Error('seeded profile did not reach launch');
});
console.log(JSON.stringify({ evidence: 'compiled TS boundary plus exact-source JS functions; mocked filesystem/process/browser I/O; no certificate store changes', records, launches }, null, 2));
process.exitCode = records.some(x => !x.pass) ? 1 : 0;
