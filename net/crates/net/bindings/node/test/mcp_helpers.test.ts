/**
 * MCP bridge pure-helper tests (`MCP_BRIDGE_SDK_PLAN.md` P2).
 *
 * Build the addon first:
 *   napi build --platform --features ...,consent,mcp
 *
 * The helpers are the bridge's one Rust implementation — these tests pin
 * the classification parity vectors (same inputs -> same status in every
 * binding) and the secret-negative rule (no env value ever crosses back).
 */

import { describe, it, expect } from "vitest";

import { classifyMcpServer, lowerMcpTool } from "../index";

const SECRET = "ghp_this-value-must-never-cross";

describe("classifyMcpServer", () => {
  it("matches the cross-binding parity vectors", () => {
    // Same inputs -> same status/tags in every binding.
    expect(
      classifyMcpServer("npx", ["-y", "some-server"], [{ key: "GITHUB_TOKEN", value: SECRET }])
    ).toBe("credentialed");
    expect(
      classifyMcpServer("npx", ["-y", "@modelcontextprotocol/server-github"], [])
    ).toBe("external_api");
    // Unsure => spicy: gated exactly like credentialed.
    expect(classifyMcpServer("uvx", ["mcp-server-time"], [{ key: "TZ", value: "UTC" }])).toBe(
      "unknown"
    );
  });

  it("requires force to downgrade, and upgrades freely", () => {
    expect(() => classifyMcpServer("uvx", ["t"], [], "no-credentials")).toThrow(/force/);
    expect(classifyMcpServer("uvx", ["t"], [], "no-credentials", true)).toBe("none");
    expect(classifyMcpServer("uvx", ["t"], [], "credentialed")).toBe("credentialed");
    expect(() => classifyMcpServer("uvx", ["t"], [], "bogus")).toThrow(/credentialOverride/);
  });
});

describe("lowerMcpTool", () => {
  it("produces the descriptor + bridge metadata", () => {
    const tool = {
      name: "echo",
      description: "echo it back",
      inputSchema: { type: "object", properties: { message: { type: "string" } } },
    };
    const lowered = lowerMcpTool(JSON.stringify(tool), "2.0.0", "credentialed", "provider_local");
    expect(lowered.toolId).toBe("echo");
    expect(lowered.mcpName).toBe("echo");
    expect(lowered.bridgeMetadata["tool::echo::compat_tier"]).toBe("mcp_bridge");
    expect(lowered.bridgeMetadata["tool::echo::credential_status"]).toBe("credentialed");
    const desc = JSON.parse(lowered.descriptor);
    expect(desc.tool_id).toBe("echo");
    expect(JSON.stringify(desc.input_schema)).toContain("message");
  });

  it("sanitizes non-channel-safe names", () => {
    // A camelCase name is bridged under a sanitized id; the original name
    // rides along as mcpName for the eventual tools/call.
    const lowered = lowerMcpTool(
      JSON.stringify({ name: "getCaps", inputSchema: { type: "object" } }),
      "1.0.0",
      "none"
    );
    expect(lowered.mcpName).toBe("getCaps");
    expect(lowered.toolId).not.toBe("getCaps");
    expect(lowered.toolId.startsWith("getcaps")).toBe(true);
  });

  it("rejects wire-style garbage in the trusted-local status field", () => {
    // `credentialStatus` is trusted LOCAL input (the classifier's own
    // label) — an unknown label is an error, never silently gated/guessed.
    const tool = JSON.stringify({ name: "echo", inputSchema: { type: "object" } });
    expect(() => lowerMcpTool(tool, "1.0.0", "totally-fine-trust-me")).toThrow(
      /credentialStatus/
    );
    expect(() => lowerMcpTool(tool, "1.0.0", "none", "anything")).toThrow(/substitutability/);
  });

  it("never lets an env value cross back (secret-negative)", () => {
    const status = classifyMcpServer("npx", ["srv"], [{ key: "API_KEY", value: SECRET }]);
    expect(status).toBe("credentialed");
    expect(status).not.toContain(SECRET);

    const lowered = lowerMcpTool(
      JSON.stringify({ name: "srv.call", description: "calls things", inputSchema: { type: "object" } }),
      "1.0.0",
      status
    );
    expect(JSON.stringify(lowered)).not.toContain(SECRET);
  });
});
