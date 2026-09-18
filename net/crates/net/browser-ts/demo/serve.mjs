/**
 * A static server for the demo, scoped to this package.
 *
 * Exists because the demo is ES modules plus an importmap: `file://`
 * cannot load either. No dependencies, no configuration.
 */
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join, normalize, resolve } from 'node:path';

const root = resolve(import.meta.dirname, '..');
const port = Number(process.env.PORT ?? 8173);
const types = new Map([
  ['.html', 'text/html; charset=utf-8'],
  ['.js', 'text/javascript; charset=utf-8'],
  ['.mjs', 'text/javascript; charset=utf-8'],
  ['.wasm', 'application/wasm'],
  ['.json', 'application/json'],
  ['.map', 'application/json'],
]);

createServer(async (request, response) => {
  const url = new URL(request.url ?? '/', 'http://localhost');
  const path = url.pathname === '/' ? '/demo/index.html' : url.pathname;
  const file = join(root, normalize(path).replace(/^(\.\.[/\\])+/, ''));
  if (!file.startsWith(root)) {
    response.writeHead(403).end('outside the package');
    return;
  }
  try {
    const body = await readFile(file);
    response.writeHead(200, {
      'content-type': types.get(extname(file)) ?? 'application/octet-stream',
      'cache-control': 'no-store',
      // The leaf's identity is scoped to an origin; a demo served
      // cross-origin would be a different node.
      'cross-origin-opener-policy': 'same-origin',
    });
    response.end(body);
  } catch {
    response.writeHead(404).end('not found');
  }
}).listen(port, () => {
  process.stdout.write(`demo: http://localhost:${String(port)}/\n`);
});
