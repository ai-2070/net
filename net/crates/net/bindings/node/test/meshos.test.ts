// Tests for the MeshOS daemon-author SDK (Phase 3 slice 1).
//
// Requires the binding to have been built with the `meshos` Cargo
// feature: `npm run build:debug` (which enables it by default per
// `package.json:scripts.build:debug`). The describe.skipIf below
// noops the whole suite if MeshOS symbols are absent.

import { describe, expect, it } from 'vitest';

let symbols: Record<string, unknown> = {};
try {
  // eslint-disable-next-line @typescript-eslint/no-require-imports
  symbols = require('../index');
} catch {
  symbols = {};
}

const hasMeshOs =
  typeof symbols.MeshOsDaemonSdk === 'function' &&
  typeof symbols.MeshOsDaemonHandle === 'function' &&
  typeof symbols.Identity === 'function';

const d = hasMeshOs ? describe : describe.skip;

interface MeshOsSdkErrorLike extends Error {
  // The napi side embeds the envelope in the message; the TS shim
  // parses the kind. For direct napi error consumption we read the
  // envelope manually.
}

function parseKind(err: unknown): string | null {
  if (!(err instanceof Error)) return null;
  const m = err.message.match(/<<meshos-sdk-kind:([^>]+)>>/);
  return m ? m[1] : null;
}

d('MeshOS daemon-author SDK (Phase 3 slice 1)', () => {
  const {
    MeshOsDaemonSdk,
    Identity,
  } = symbols as {
    MeshOsDaemonSdk: any;
    Identity: any;
  };

  // -------------------------------------------------------------------------
  // Lifecycle
  // -------------------------------------------------------------------------

  it('start + shutdown with defaults', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    await sdk.shutdown();
  });

  it('rejects bad config with `invalid_config` kind', async () => {
    await expect(MeshOsDaemonSdk.start({ thisNode: -1n })).rejects.toThrow();
    try {
      await MeshOsDaemonSdk.start({ thisNode: -1n });
    } catch (e) {
      expect(parseKind(e)).toBe('invalid_config');
    }
  });

  it('double shutdown surfaces already_shutdown', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    await sdk.shutdown();
    try {
      await sdk.shutdown();
      throw new Error('expected throw');
    } catch (e) {
      expect(parseKind(e)).toBe('already_shutdown');
    }
  });

  // -------------------------------------------------------------------------
  // Registration + metadata
  // -------------------------------------------------------------------------

  it('register a daemon and read its identity off the handle', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const identity = Identity.generate();
      const daemon = {
        name: 'echo',
        process: (_event: unknown) => [Buffer.from('out')],
      };
      const handle = await sdk.registerDaemon(daemon, identity);
      try {
        expect(handle.daemonId).toBe(identity.originHash);
        expect(handle.daemonName).toBe('echo');
      } finally {
        await handle.gracefulShutdown(10n);
      }
    } finally {
      await sdk.shutdown();
    }
  });

  it('metadata view carries Active maintenance state and the configured node id', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const handle = await sdk.registerDaemon(
        { name: 'echo', process: () => [] },
        Identity.generate(),
      );
      try {
        const md = await handle.metadata();
        expect(md.daemonName).toBe('echo');
        expect(md.maintenanceState.kind).toBe('Active');
        // Substrate `runtime_this_node()` is a placeholder
        // returning `0` today; pin to keep the test honest.
        expect(md.nodeId).toBe(0n);
      } finally {
        await handle.gracefulShutdown(10n);
      }
    } finally {
      await sdk.shutdown();
    }
  });

  // -------------------------------------------------------------------------
  // Control events
  // -------------------------------------------------------------------------

  it('tryNextControl returns null on an empty channel', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const handle = await sdk.registerDaemon(
        { name: 'echo', process: () => [] },
        Identity.generate(),
      );
      try {
        expect(await handle.tryNextControl()).toBeNull();
      } finally {
        await handle.gracefulShutdown(10n);
      }
    } finally {
      await sdk.shutdown();
    }
  });

  it('nextControl with timeout returns null on quiet channel', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const handle = await sdk.registerDaemon(
        { name: 'echo', process: () => [] },
        Identity.generate(),
      );
      try {
        const ev = await handle.nextControl(100n);
        expect(ev).toBeNull();
      } finally {
        await handle.gracefulShutdown(10n);
      }
    } finally {
      await sdk.shutdown();
    }
  });

  // -------------------------------------------------------------------------
  // publish_log
  // -------------------------------------------------------------------------

  it('publishLog accepts every level', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const handle = await sdk.registerDaemon(
        { name: 'echo', process: () => [] },
        Identity.generate(),
      );
      try {
        for (const lvl of ['trace', 'debug', 'info', 'warn', 'error']) {
          await handle.publishLog(lvl, `from ${lvl}`);
        }
      } finally {
        await handle.gracefulShutdown(10n);
      }
    } finally {
      await sdk.shutdown();
    }
  });

  it('publishLog rejects invalid level with `invalid_log_level` kind', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const handle = await sdk.registerDaemon(
        { name: 'echo', process: () => [] },
        Identity.generate(),
      );
      try {
        try {
          await handle.publishLog('verbose', 'nope');
          throw new Error('expected throw');
        } catch (e) {
          expect(parseKind(e)).toBe('invalid_log_level');
        }
      } finally {
        await handle.gracefulShutdown(10n);
      }
    } finally {
      await sdk.shutdown();
    }
  });

  // -------------------------------------------------------------------------
  // graceful shutdown
  // -------------------------------------------------------------------------

  it('gracefulShutdown completes; subsequent handle methods throw already_shutdown', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const handle = await sdk.registerDaemon(
        { name: 'echo', process: () => [] },
        Identity.generate(),
      );
      await handle.gracefulShutdown(10n);
      try {
        await handle.publishLog('info', 'after shutdown');
        throw new Error('expected throw');
      } catch (e) {
        expect(parseKind(e)).toBe('already_shutdown');
      }
    } finally {
      await sdk.shutdown();
    }
  });

  it('publishCapabilities stub does not throw', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const handle = await sdk.registerDaemon(
        { name: 'echo', process: () => [] },
        Identity.generate(),
      );
      try {
        await handle.publishCapabilities();
        await handle.publishCapabilities(null);
      } finally {
        await handle.gracefulShutdown(10n);
      }
    } finally {
      await sdk.shutdown();
    }
  });

  // -------------------------------------------------------------------------
  // Daemon validation
  // -------------------------------------------------------------------------

  it('registerDaemon rejects a daemon without a `name` property', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      try {
        await sdk.registerDaemon({ process: () => [] } as any, Identity.generate());
        throw new Error('expected throw');
      } catch (e) {
        expect(parseKind(e)).toBe('invalid_daemon');
      }
    } finally {
      await sdk.shutdown();
    }
  });

  it('registerDaemon rejects a daemon without a `process` method', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      try {
        await sdk.registerDaemon({ name: 'no-process' } as any, Identity.generate());
        throw new Error('expected throw');
      } catch (e) {
        expect(parseKind(e)).toBe('invalid_daemon');
      }
    } finally {
      await sdk.shutdown();
    }
  });

  // -------------------------------------------------------------------------
  // Drop-without-shutdown still cleans up (Rust-side Drop impl)
  // -------------------------------------------------------------------------

  it('re-registering the same identity after handle drop succeeds', async () => {
    const sdk = await MeshOsDaemonSdk.start();
    try {
      const identity = Identity.generate();
      // Register once and drop the handle reference. The napi
      // pyclass-equivalent Drop runs once GC reclaims the wrapper;
      // for the test we explicitly graceful-shutdown to force
      // deregistration, then re-register cleanly.
      let handle = await sdk.registerDaemon(
        { name: 'echo', process: () => [] },
        identity,
      );
      await handle.gracefulShutdown(10n);
      // Substrate `register` succeeds because the slot is free.
      handle = await sdk.registerDaemon(
        { name: 'echo', process: () => [] },
        identity,
      );
      await handle.gracefulShutdown(10n);
    } finally {
      await sdk.shutdown();
    }
  });
});
