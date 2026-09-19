import { BrowserNode } from './browser-node.mjs';
import assert from 'node:assert/strict';
async function probe(throwFirst) {
  const closed = []; let count = 0; let parentClosed = false;
  const inner = {
    node_id_hex: () => '0000000000000001', on_event: () => {},
    open_stream: () => {
      const id = ++count;
      return { stream_id_hex: () => id.toString(16).padStart(16, '0'),
        on_message: () => {}, is_reliable: () => true,
        close: () => { closed.push(id); if (id === 1 && throwFirst) throw Error('injected close failure'); }
      };
    },
    close: () => { parentClosed = true; }
  };
  const node = new BrowserNode(inner, null, {}, null);
  const a = node.openStream({ reliability: 'reliable' });
  const b = node.openStream({ reliability: 'reliable' });
  const pa = a[Symbol.asyncIterator]().next();
  const pb = b[Symbol.asyncIterator]().next();
  let thrown = false; try { node.close(); } catch { thrown = true; }
  const aResult = await pa;
  const bResult = await Promise.race([pb, new Promise(r => setTimeout(() => r('pending'), 100))]);
  node.close();
  console.log(JSON.stringify({throwFirst, thrown, closed, parentClosed, aDone:aResult.done, bResult}));
  assert.equal(bResult.done, true, 'second iterator must end even if first injected close throws');
  assert.equal(parentClosed, true, 'native parent must still close');
}
let failed = 0;
for (const value of [false, true]) { try { await probe(value); console.log('PASS', value); } catch(e) { failed++; console.log('FAIL', value, e.message); } }
process.exitCode = failed ? 1 : 0;
