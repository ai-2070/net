import fs from 'node:fs';
import vm from 'node:vm';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
const repo = 'C:/Users/chief/orca/workspaces/net/kyra-stage5-9f8bde0c7';
const records = [];
async function probe(name, fn) {
  try { await fn(); records.push({ name, pass: true }); }
  catch (e) { records.push({ name, pass: false, error: e.message }); }
}
const { LeafStream } = await import(pathToFileURL(repo + '/net/crates/net/browser-ts/dist/stream.js'));
async function streamResult(payload) {
  let emit;
  const stream = new LeafStream({ send() {}, close() {}, is_reliable() { return true; }, stream_id_hex() { return '9'; }, on_message(cb) { emit = cb; } });
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
  const data = await streamResult('{"type":"stream_data","stream_id":"9","seq":"1","payload":"AQI="}');
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
