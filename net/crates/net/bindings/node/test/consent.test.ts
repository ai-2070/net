/**
 * Consent + pin-store binding tests (`MCP_BRIDGE_SDK_PLAN.md` P2).
 *
 * Build the addon first:
 *   napi build --platform --features net,cortex,compute,groups,meshos,deck,meshdb,aggregator,tool,consent,mcp
 *
 * The pin store is the machine-shared consent file the `net mcp pin` CLI
 * and a running `net mcp serve` shim use. The load-bearing assertions are
 * the P2 acceptance criteria: concurrent locked mutations lose nothing,
 * and the on-disk format is byte-compatible with the Rust core's — the
 * binding never opens the file itself.
 */

import { describe, it, expect, beforeEach, afterEach } from "vitest";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

import {
  CapabilityId,
  ConsentPolicy,
  PinStore,
  credentialRequiresConsent,
} from "../index";

let dir: string;
beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), "net-pins-"));
});
afterEach(() => {
  fs.rmSync(dir, { recursive: true, force: true });
});
const storePath = () => path.join(dir, "pins.json");

// ---------------------------------------------------------------------------
// CapabilityId
// ---------------------------------------------------------------------------

describe("CapabilityId", () => {
  it("parses on the first slash", () => {
    const cid = CapabilityId.parse("homelab/github.create_issue");
    expect(cid.provider).toBe("homelab");
    expect(cid.capability).toBe("github.create_issue");
    expect(cid.display()).toBe("homelab/github.create_issue");
    // The capability half may itself contain `/`.
    const nested = CapabilityId.parse("homelab/svc/sub");
    expect(nested.provider).toBe("homelab");
    expect(nested.capability).toBe("svc/sub");
  });

  it("rejects missing or empty halves", () => {
    for (const bad of ["bareword", "/cap", "prov/"]) {
      expect(() => CapabilityId.parse(bad)).toThrow(/consent:/);
    }
  });

  it("canonicalizes provider spellings", () => {
    // A node id typed as hex or with whitespace keys the SAME records as
    // the decimal form discovery emits.
    for (const spelling of ["0x2a/echo", "0X2A/echo", " 42/echo", "42 /echo"]) {
      const cid = CapabilityId.parse(spelling);
      expect(cid.display()).toBe("42/echo");
    }
    expect(new CapabilityId("0x2a", "echo").display()).toBe("42/echo");
  });
});

// ---------------------------------------------------------------------------
// Credential-status trust boundary + consent gate
// ---------------------------------------------------------------------------

describe("consent gate", () => {
  it("never trusts a wire status — even 'none' is gated", () => {
    for (const status of ["credentialed", "external_api", "unknown", "none", "", "bogus"]) {
      expect(credentialRequiresConsent(status)).toBe(true);
    }
  });

  it("gates everything until admitted", () => {
    const policy = new ConsentPolicy();
    expect(policy.decide("b/echo", "none")).toBe("requires_approval");
    expect(policy.requiresApproval("b/echo", "credentialed")).toBe(true);

    policy.allow("b/echo");
    expect(policy.decide("b/echo", "credentialed")).toBe("allowed");
    expect(policy.requiresApproval("b/other", "credentialed")).toBe(true);

    policy.pin("b/slack.post");
    expect(policy.isPinned("b/slack.post")).toBe(true);
    expect(policy.decide("b/slack.post", "external_api")).toBe("allowed");
    expect(policy.pinned()).toEqual(["b/slack.post"]);
    policy.unpin("b/slack.post");
    expect(policy.requiresApproval("b/slack.post", "external_api")).toBe(true);
  });

  it("keys on canonical identity", () => {
    // A pin under the hex spelling admits the decimal spelling —
    // canonicalization runs in the Rust core, not here.
    const policy = new ConsentPolicy();
    policy.pin("0x2a/echo");
    expect(policy.decide("42/echo", "credentialed")).toBe("allowed");
  });
});

// ---------------------------------------------------------------------------
// PinStore — the machine-shared, lock-protocol store
// ---------------------------------------------------------------------------

describe("PinStore", () => {
  it("reads a missing store as empty", async () => {
    const store = new PinStore(storePath());
    expect(await store.approved()).toEqual([]);
    expect(await store.pending()).toEqual([]);
    expect(await store.list()).toEqual([]);
    expect(await store.state("b/echo")).toBeNull();
  });

  it("request is pending-only and never upgrades", async () => {
    const store = new PinStore(storePath());
    expect(await store.request("b/echo")).toBe("pending");
    expect(await store.isApproved("b/echo")).toBe(false);
    expect(await store.pending()).toEqual(["b/echo"]);

    expect(await store.approve("b/echo")).toBe(true);
    expect(await store.request("b/echo")).toBe("approved"); // untouched, reported
    expect(await store.isApproved("b/echo")).toBe(true);

    expect(await store.reject("b/echo")).toBe(true);
    expect(await store.reject("b/echo")).toBe(false); // absent -> no-op
    expect(await store.state("b/echo")).toBeNull();
  });

  it("is shared between handles on the same path", async () => {
    const a = new PinStore(storePath());
    const b = new PinStore(storePath());
    await a.approve("b/secret");
    expect(await b.isApproved("b/secret")).toBe(true);
    expect(await b.list()).toEqual([{ capId: "b/secret", state: "approved" }]);
  });

  it("throws on a corrupt store instead of resetting", async () => {
    fs.writeFileSync(storePath(), "{ not valid json");
    const store = new PinStore(storePath());
    await expect(store.list()).rejects.toThrow(/pins:/);
    await expect(store.approve("b/echo")).rejects.toThrow(/pins:/);
  });

  it("round-trips the exact on-disk format the Rust core writes", async () => {
    // A file in the shape `net mcp pin` writes is readable here...
    fs.writeFileSync(
      storePath(),
      JSON.stringify({
        pins: [
          { cap_id: "42/echo", state: "approved" },
          { cap_id: "42/spicy", state: "pending" },
        ],
      })
    );
    const store = new PinStore(storePath());
    expect(await store.isApproved("42/echo")).toBe(true);
    expect(await store.pending()).toEqual(["42/spicy"]);

    // ...and a mutation here persists that same shape (same impl), so the
    // CLI/shim read it back.
    await store.approve("42/spicy");
    const onDisk = JSON.parse(fs.readFileSync(storePath(), "utf8"));
    const pairs = new Set(onDisk.pins.map((p: any) => `${p.cap_id}:${p.state}`));
    expect(pairs).toEqual(new Set(["42/echo:approved", "42/spicy:approved"]));
  });

  it("loses nothing under concurrent locked mutations", async () => {
    // P2 acceptance: concurrent access, no corruption. Each mutation runs
    // under the Rust core's cross-process advisory lock on a napi worker
    // thread, so many in-flight approves must not lose one to a
    // stale-snapshot race.
    const N = 60;
    const store = new PinStore(storePath());
    const results = await Promise.all(
      Array.from({ length: N }, (_, i) => store.approve(`node/tool${i}`))
    );
    expect(results.every((r) => r === true)).toBe(true);
    const approved = await new PinStore(storePath()).approved();
    expect(approved.length).toBe(N);
    for (let i = 0; i < N; i++) {
      expect(approved).toContain(`node/tool${i}`);
    }
  });
});
