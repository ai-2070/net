// TypeScript layer for AI tool calling on net.
//
// Wraps the existing `TypedMeshRpc` napi surface with the `tool()` /
// `callTool()` ergonomic helpers + format translators that lower
// `ToolDescriptor`s to OpenAI / Anthropic / MCP / Gemini tool shapes
// and parse provider tool-call replies back into nRPC dispatches.
//
// This is the Wave 3 / B-1 + B-4 starting point. v1 covers unary
// register + invoke + format conversion. Streaming (B-2) and
// discovery (B-3 list_tools / watch_tools) follow once the
// underlying napi surface exposes them; today the only available
// streaming primitive is direct-addressed (`callStreaming(nodeId,
// ...)`), so capability-routed streaming has to wait on a
// `callServiceStreaming` TS wrapper or a `findServiceNodes` +
// direct-call composition (TODO).
//
// Plan: see
// `crates/net/docs/plans/NRPC_AI_TOOL_CALLING_AND_AGENT_DX.md`,
// slices B-1 / B-2 / B-4. Mirror of the Rust SDK's
// `net_sdk::tool` + `net_sdk::tool::formats` modules — cross-
// language tests (T-1) will pin byte equality across both.

import type { CallOptions, ServeHandle, TypedMeshRpc } from './mesh_rpc'
import type { CapabilitySetJs, ToolJs } from './index'

// ============================================================================
// Wire types — mirror of the Rust `ToolDescriptor` + `ToolEvent`.
// ============================================================================

/**
 * Discovery shape for an AI tool, as advertised on the capability
 * fold. One row per `(tool_id, version)`; `nodeCount` is filled by
 * the aggregating walk (`list_tools` once it lands).
 *
 * Schemas are stored as JSON-encoded strings (matching the Rust
 * substrate's wire shape). Use `JSON.parse(desc.inputSchema)` to
 * get the parsed JSON Schema object — most consumers want this
 * for lowering into a provider tool definition.
 *
 * Wire-compatible 1:1 with `net::adapter::net::cortex::tool::ToolDescriptor`.
 */
export interface ToolDescriptor {
  /** nRPC service name. Same string `callTool(...)` takes. */
  toolId: string
  /** Human-readable name. Defaults to `toolId` if unset. */
  name: string
  /** Tool version (semver-ish). Defaults to `"1.0.0"`. */
  version: string
  /** Human-readable description; LLMs read this to decide when to call. */
  description?: string
  /** JSON-encoded JSON Schema (draft 2020-12) for the request body. */
  inputSchema?: string
  /** JSON-encoded JSON Schema (draft 2020-12) for the response body. */
  outputSchema?: string
  /** Required capabilities / dependencies (free-form strings). */
  requires: string[]
  /** Soft latency hint (ms); `0` = no estimate. */
  estimatedTimeMs: number
  /** True if the tool is a pure function (no session state). */
  stateless: boolean
  /** True if the tool is server-streaming (uses `serveToolStreaming`). */
  streaming: boolean
  /** Free-form host-attached tags (e.g. `["web", "research"]`). */
  tags: string[]
  /** How many nodes currently serve this `(toolId, version)`. */
  nodeCount: number
}

/**
 * One envelope on a streaming tool. Discriminated by `type`.
 *
 * Wire-compatible 1:1 with `net::adapter::net::cortex::tool::ToolEvent`
 * (JSON tag form, `{"type": "start", …}` shape).
 *
 * Every stream ends with exactly one terminal event
 * (`type: "result"` or `type: "error"`). Handlers that forget emit
 * a synthesized `{type: "error", code: "missing_terminal", …}` from
 * the Rust SDK's streaming wrapper.
 */
export type ToolEvent =
  | ToolEventStart
  | ToolEventProgress
  | ToolEventDelta
  | ToolEventResult
  | ToolEventError

export interface ToolEventStart {
  type: 'start'
  toolId: string
  callId?: number
  metadata?: unknown
}

export interface ToolEventProgress {
  type: 'progress'
  pct?: number
  message?: string
}

export interface ToolEventDelta {
  type: 'delta'
  data: unknown
}

export interface ToolEventResult {
  type: 'result'
  data: unknown
}

export interface ToolEventError {
  type: 'error'
  code: string
  message: string
  details?: unknown
}

/** True if `event` is a terminal envelope (`result` or `error`). */
export function isTerminalEvent(event: ToolEvent): boolean {
  return event.type === 'result' || event.type === 'error'
}

// ============================================================================
// Descriptor construction
// ============================================================================

/**
 * Options for `tool({...})` and `serveTool(rpc, options, handler)`.
 * Mirror of the Rust `ToolMetadataBuilder` shape — caller supplies
 * the fields that don't derive from a type signature in JS (no
 * compile-time type system to introspect, unlike `schemars` in Rust).
 *
 * `inputSchema` / `outputSchema` are JSON-Schema-as-object (caller
 * uses `zod-to-json-schema`, `pydantic`, or hand-rolls); we serialize
 * to a string before stashing on the descriptor.
 */
export interface ToolOptions {
  /** nRPC service name + tool identifier. Required. */
  name: string
  /** Human-readable description. Strongly recommended. */
  description?: string
  /** Version. Defaults to `"1.0.0"`. */
  version?: string
  /** JSON Schema object for the request. */
  inputSchema?: object
  /** JSON Schema object for the response. */
  outputSchema?: object
  /** Required capabilities / dependencies. */
  requires?: string[]
  /** Soft latency hint (ms). */
  estimatedTimeMs?: number
  /** Pure-function flag. Default `true`. */
  stateless?: boolean
  /** Free-form tags. */
  tags?: string[]
}

/** Construct a [`ToolDescriptor`] from a `ToolOptions` literal. */
export function descriptorFrom(options: ToolOptions): ToolDescriptor {
  return {
    toolId: options.name,
    name: options.name,
    version: options.version ?? '1.0.0',
    description: options.description,
    inputSchema: options.inputSchema ? JSON.stringify(options.inputSchema) : undefined,
    outputSchema: options.outputSchema ? JSON.stringify(options.outputSchema) : undefined,
    requires: options.requires ?? [],
    estimatedTimeMs: options.estimatedTimeMs ?? 0,
    stateless: options.stateless ?? true,
    streaming: false,
    tags: options.tags ?? [],
    nodeCount: 0,
  }
}

// ============================================================================
// Register / invoke
// ============================================================================

/**
 * Handler signature for `serveTool` — receives a decoded request,
 * returns a decoded response (or a Promise of one).
 */
export type ToolHandler<Req = unknown, Resp = unknown> = (
  req: Req,
) => Resp | Promise<Resp>

/**
 * Handle returned by `serveTool`. Calling `.close()` deregisters the
 * underlying nRPC handler (mirror of the Rust `ToolServeHandle`'s
 * Drop semantics). Calling `.close()` twice is idempotent; the
 * second call is a no-op.
 *
 * NOTE: v1 does NOT yet integrate with the substrate-side
 * `tool_registry`, so the `ai-tool:<toolId>` capability tag must be
 * added to the caller's announce explicitly. See
 * [`addToolCapabilitiesToAnnounce`] for the convention. Once the
 * napi surface exposes `tool_registry()` insert/remove (a Wave 3
 * follow-up), this handle will atomically reverse both the
 * registry insert and the handler registration on `.close()`.
 */
export interface ToolServeHandle {
  /** The descriptor under which the tool was registered. */
  readonly descriptor: ToolDescriptor
  /** Deregister the handler. Idempotent. */
  close(): void
}

/**
 * Register an AI tool against `rpc`. The handler is registered as
 * an nRPC service at `descriptor.toolId` with JSON codec (same as
 * the Rust SDK's `Mesh::serve_tool`).
 *
 * The caller is responsible for announcing the tool to peers — use
 * [`addToolCapabilitiesToAnnounce`] on the `CapabilitySetJs` you
 * pass to `mesh.announceCapabilities(...)` so the
 * `ai-tool:<toolId>` tag + the `ToolJs` entry land on the wire.
 *
 * Wave 3 follow-up: once the napi surface exposes
 * `tool_registry()`, this helper will atomically insert there too,
 * making the announce-time merge automatic (matching the Rust
 * SDK's contract).
 */
export function serveTool<Req = unknown, Resp = unknown>(
  rpc: TypedMeshRpc,
  options: ToolOptions,
  handler: ToolHandler<Req, Resp>,
): ToolServeHandle {
  const descriptor = descriptorFrom(options)
  const inner: ServeHandle = rpc.serve<Req, Resp>(descriptor.toolId, handler)
  let closed = false
  return {
    descriptor,
    close() {
      if (closed) return
      closed = true
      inner.close()
    },
  }
}

/**
 * Capability-routed unary tool invocation. Encodes `req` as JSON
 * (the codec every AI provider consumes for tool input/output),
 * dispatches via `rpc.callService(toolId, req, opts)`.
 *
 * Throws `NoRouteError` if no host advertises `nrpc:<toolId>` in
 * the local capability fold; bubbles handler errors as
 * `RpcServerError` with the typed-handler status code.
 */
export async function callTool<Req = unknown, Resp = unknown>(
  rpc: TypedMeshRpc,
  toolId: string,
  req: Req,
  opts?: CallOptions,
): Promise<Resp> {
  return rpc.callService<Req, Resp>(toolId, req, opts)
}

/**
 * Merge tool descriptors into a `CapabilitySetJs` so the next
 * `mesh.announceCapabilities(caps)` carries:
 *
 * - `ai-tool:<toolId>` tag — peer fold's tag-prefix lookup hits.
 * - A `ToolJs` entry — peer's `list_tools` walk sees the
 *   tool's tag-encoded fields.
 *
 * Caller still owns the `caps` object — pass it through
 * `mesh.announceCapabilities(caps)` to publish. Returns the same
 * object for chaining.
 *
 * This is a v1 convenience; once the napi surface exposes
 * `tool_registry()`, the announce-time merge happens
 * automatically and this helper becomes optional.
 */
export function addToolCapabilitiesToAnnounce(
  caps: CapabilitySetJs,
  descriptors: ToolDescriptor[],
): CapabilitySetJs {
  if (descriptors.length === 0) return caps
  const tags = new Set(caps.tags ?? [])
  const tools: ToolJs[] = [...(caps.tools ?? [])]
  for (const desc of descriptors) {
    tags.add(`ai-tool:${desc.toolId}`)
    tools.push({
      toolId: desc.toolId,
      name: desc.name,
      version: desc.version,
      inputSchema: desc.inputSchema,
      outputSchema: desc.outputSchema,
      requires: desc.requires,
      estimatedTimeMs: desc.estimatedTimeMs,
      stateless: desc.stateless,
    })
  }
  caps.tags = Array.from(tags)
  caps.tools = tools
  return caps
}

// ============================================================================
// Format translators — mirror of `net_sdk::tool::formats`
// ============================================================================
//
// Each provider submodule exports two directions:
//
// 1. `to<Provider>Tool(desc) -> object` — descriptor → provider's
//    tool-definition shape for the `tools` array on the provider's
//    HTTP request.
// 2. `lower<Provider>ToolCall(reply) -> ToolCallSpec` — provider's
//    reply → ToolCallSpec the caller hands to `callTool`.
//
// All translators short-circuit a missing `inputSchema` to an
// empty-object schema (`{type: "object", properties: {}}`) since
// providers' strict-mode validators reject null parameter schemas.

/** Canonical hand-off between a provider adapter and `callTool`. */
export interface ToolCallSpec {
  /** nRPC tool_id to invoke. */
  name: string
  /** JSON-encoded arguments to pass to `callTool` (caller parses). */
  argumentsJson: string
  /** Provider-supplied call id when present (for reply correlation). */
  providerCallId?: string
}

/** Thrown when a provider's tool-call reply doesn't match its spec. */
export class ToolCallParseError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'ToolCallParseError'
  }
}

function inputSchemaValue(desc: ToolDescriptor): object {
  if (!desc.inputSchema) return { type: 'object', properties: {} }
  try {
    return JSON.parse(desc.inputSchema) as object
  } catch {
    // Schema string was malformed (shouldn't happen for descriptors
    // built via `descriptorFrom`). Empty-object fallback keeps
    // provider validators happy.
    return { type: 'object', properties: {} }
  }
}

/** OpenAI Chat Completions / Responses API `tools` array. */
export const openai = {
  /**
   * Lower a descriptor to an OpenAI tool definition. Shape:
   * ```
   * { type: "function", function: { name, description, parameters, strict } }
   * ```
   * `strict` is true when the descriptor carried an `inputSchema`.
   */
  toOpenaiTool(desc: ToolDescriptor): object {
    return {
      type: 'function',
      function: {
        name: desc.toolId,
        description: desc.description ?? '',
        parameters: inputSchemaValue(desc),
        strict: desc.inputSchema !== undefined,
      },
    }
  },

  /**
   * Parse one OpenAI `tool_calls[]` entry into a `ToolCallSpec`.
   * OpenAI's `function.arguments` is a JSON-encoded STRING; this
   * helper validates it parses up front so malformed payloads fail
   * fast instead of riding through `callTool`.
   */
  lowerOpenaiToolCall(call: Record<string, unknown>): ToolCallSpec {
    const fn = call['function'] as Record<string, unknown> | undefined
    if (!fn) throw new ToolCallParseError('tool-call reply missing field `function`')
    const name = fn['name']
    if (typeof name !== 'string') {
      throw new ToolCallParseError('tool-call reply field `function.name` must be a string')
    }
    const argumentsField = fn['arguments']
    if (typeof argumentsField !== 'string') {
      throw new ToolCallParseError(
        'tool-call reply field `function.arguments` must be a JSON-encoded string',
      )
    }
    try {
      JSON.parse(argumentsField)
    } catch (e) {
      throw new ToolCallParseError(
        `tool-call arguments were not valid JSON: ${(e as Error).message}`,
      )
    }
    const id = call['id']
    return {
      name,
      argumentsJson: argumentsField,
      providerCallId: typeof id === 'string' ? id : undefined,
    }
  },
}

/** Anthropic Messages API `tools` array + `tool_use` content blocks. */
export const anthropic = {
  /**
   * Lower a descriptor to an Anthropic tool definition. Shape:
   * ```
   * { name, description, input_schema }
   * ```
   * No tool-level `strict` flag — Anthropic relies on schema-
   * validated tool input as the default.
   */
  toAnthropicTool(desc: ToolDescriptor): object {
    return {
      name: desc.toolId,
      description: desc.description ?? '',
      input_schema: inputSchemaValue(desc),
    }
  },

  /**
   * Parse one Anthropic `tool_use` content block into a
   * `ToolCallSpec`. `input` is already a parsed object (not a
   * string like OpenAI); re-serializes once to preserve the
   * `argumentsJson: string` invariant.
   */
  lowerAnthropicToolUse(block: Record<string, unknown>): ToolCallSpec {
    const name = block['name']
    if (typeof name !== 'string') {
      throw new ToolCallParseError('tool_use block field `name` must be a string')
    }
    if (!('input' in block)) {
      throw new ToolCallParseError('tool_use block missing field `input`')
    }
    const argumentsJson = JSON.stringify(block['input'])
    const id = block['id']
    return {
      name,
      argumentsJson,
      providerCallId: typeof id === 'string' ? id : undefined,
    }
  },
}

/** Model Context Protocol `tools/list` + `tools/call`. */
export const mcp = {
  /** Lower a descriptor to an MCP tool definition. Shape: `{ name, description, inputSchema }` (camelCase). */
  toMcpTool(desc: ToolDescriptor): object {
    return {
      name: desc.toolId,
      description: desc.description ?? '',
      inputSchema: inputSchemaValue(desc),
    }
  },

  /**
   * Parse an MCP `tools/call` request's `params` into a
   * `ToolCallSpec`. `providerCallId` is left `undefined` — MCP's
   * JSON-RPC `id` lives one envelope layer up, threaded
   * independently.
   */
  lowerMcpToolsCall(params: Record<string, unknown>): ToolCallSpec {
    const name = params['name']
    if (typeof name !== 'string') {
      throw new ToolCallParseError('tools/call params field `name` must be a string')
    }
    if (!('arguments' in params)) {
      throw new ToolCallParseError('tools/call params missing field `arguments`')
    }
    return {
      name,
      argumentsJson: JSON.stringify(params['arguments']),
      providerCallId: undefined,
    }
  },
}

/** Gemini `generateContent` function-calling shape. */
export const gemini = {
  /**
   * Lower a descriptor to one Gemini `FunctionDeclaration`. Shape:
   * ```
   * { name, description, parameters }
   * ```
   * Caller wraps these into the outer
   * `tools: [{ function_declarations: [ … ] }]` array.
   */
  toGeminiFunctionDeclaration(desc: ToolDescriptor): object {
    return {
      name: desc.toolId,
      description: desc.description ?? '',
      parameters: inputSchemaValue(desc),
    }
  },

  /**
   * Parse one Gemini `functionCall` part into a `ToolCallSpec`.
   * Gemini has no per-call id; the spec leaves `providerCallId`
   * `undefined` (multi-call sequences are positional).
   */
  lowerGeminiFunctionCall(call: Record<string, unknown>): ToolCallSpec {
    const name = call['name']
    if (typeof name !== 'string') {
      throw new ToolCallParseError('functionCall field `name` must be a string')
    }
    if (!('args' in call)) {
      throw new ToolCallParseError('functionCall missing field `args`')
    }
    return {
      name,
      argumentsJson: JSON.stringify(call['args']),
      providerCallId: undefined,
    }
  },
}
