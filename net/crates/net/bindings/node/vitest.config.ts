import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    include: ["test/**/*.test.ts"],
    testTimeout: 30000,
    hookTimeout: 30000,
    sequence: {
      concurrent: false,
    },
    fileParallelism: false,
  },
  resolve: {
    // `.ts` BEFORE `.js`, which is the reverse of Vite's default order.
    //
    // Seven of the hand-written sources here ship as compiled CJS beside
    // themselves — `npm run build:ts` emits `tool.js` next to `tool.ts`,
    // and the same for `errors`, `mesh_rpc`, `meshdb`, `org`, `subnet`
    // and `aggregator`. Every test imports them extensionless
    // (`from '../tool'`), so on the default order the suite resolved the
    // COMPILED artifact and the `.ts` beside it was never loaded.
    //
    // That makes a stale artifact indistinguishable from a passing
    // change. Edit `tool.ts`, run `npm test` without re-running
    // `build:ts`, and vitest reports green against the previous build —
    // it does not report that it ignored your edit, because from its
    // point of view nothing is wrong. This cost a real debugging session:
    // a control run swapped `tool.ts` back to its unfixed version and the
    // suite stayed green, which reads as "the test does not witness the
    // bug" when the truth was "the test never saw the file".
    //
    // Preferring `.ts` makes the source the thing under test, which is
    // what a unit suite is for. `build:ts` still runs in CI and still has
    // to succeed — it produces what consumers `require()` — but the
    // package's shipped shape is proven by the external-consumer gate,
    // not by shadowing the sources here. `index.js` is unaffected: it is
    // the napi loader and has no `.ts` beside it to be shadowed by.
    extensions: [".ts", ".mts", ".mjs", ".js", ".jsx", ".tsx", ".json"],
  },
});
