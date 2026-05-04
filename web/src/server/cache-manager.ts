/* eslint-disable unicorn/no-await-expression-member */
import "server-only";

import type { Cache } from "cache-manager";
import { createCache } from "cache-manager";
import { KeyvCacheableMemory } from "cacheable";
import Keyv from "keyv";
import v8 from "node:v8";
import { constants } from "node:zlib";
import globals from "@/lib/globals";

export const fastBrotliOptions = {
  params: {
    [constants.BROTLI_PARAM_QUALITY]: 1,
    [constants.BROTLI_PARAM_MODE]: constants.BROTLI_MODE_GENERIC,
  },
};

function createCacheManager(): Cache {
  // const { compress, decompress } = createCompress();

  return createCache({
    ttl: globals.defaultTtl,
    nonBlocking: true,
    stores: [
      //  High performance in-memory cache with LRU and TTL
      new Keyv({
        store: new KeyvCacheableMemory({ ttl: globals.defaultTtl }),
        serialize: (x) => v8.serialize(x).toString("base64"),
        // eslint-disable-next-line @typescript-eslint/no-unsafe-return
        deserialize: (s) => v8.deserialize(Buffer.from(s, "base64")),
      }),
    ],
  });
}

const globalForCacheManager = globalThis as unknown as {
  cacheManager: Keyv;
};

export const cacheManager =
  globalForCacheManager.cacheManager || createCacheManager();

if (process.env.NODE_ENV !== "production")
  globalForCacheManager.cacheManager = cacheManager;
