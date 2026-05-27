// AI tool-calling surface for the Net mesh SDK.
//
// Re-exports the canonical implementation from `@net-mesh/core/tool`
// so users can `import { serveTool, callTool, openai, ... } from
// '@net-mesh/sdk/tool'` without reaching into the raw napi binding.
//
// Why re-export rather than wrap: the underlying tool layer is
// pure-TypeScript on top of the typed `MeshRpc` surface (see
// `bindings/node/tool.ts`). There is no SDK-specific ergonomics
// to layer on top — the napi-side code already handles JSON
// codec + capability auto-install + format translators. A pure
// re-export keeps the SDK and the binding in lockstep without
// duplicating a single line of policy.
//
// Layout mirrors the binding's exports — types, descriptor
// builder, register / invoke, discovery, format translators
// (openai / anthropic / mcp / gemini), and the
// `tool.metadata.fetch` helpers. The four provider translators
// are namespaced objects (`openai.toOpenaiTool(...)`,
// `anthropic.lowerAnthropicToolUse(...)`, etc.) to match the
// reference implementation pinned by T-1 + T-2 cross-language
// golden vectors.

export type {
  ToolDescriptor,
  ToolEvent,
  ToolEventStart,
  ToolEventProgress,
  ToolEventDelta,
  ToolEventResult,
  ToolEventError,
  ToolOptions,
  ToolHandler,
  ToolServeHandle,
  StreamingToolHandler,
  ToolListChange,
  WatchToolsOptions,
  ToolMetadataResponse,
  ToolCallSpec,
} from '@net-mesh/core/tool'

export {
  isTerminalEvent,
  descriptorFrom,
  serveTool,
  serveToolStreaming,
  callTool,
  callToolStreaming,
  listTools,
  watchTools,
  addToolCapabilitiesToAnnounce,
  fetchToolMetadata,
  ToolCallParseError,
  TOOL_METADATA_FETCH_SERVICE,
  openai,
  anthropic,
  mcp,
  gemini,
} from '@net-mesh/core/tool'
