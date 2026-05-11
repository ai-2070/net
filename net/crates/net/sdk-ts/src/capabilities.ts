/**
 * Capability declarations — hardware / software / models / tools /
 * tags / resource limits — and the filters that query them.
 *
 * Construct a {@link CapabilitySet} from whatever your node actually
 * runs, hand it to {@link MeshNode.announceCapabilities}, and the
 * mesh pushes it to every directly-connected peer. Peers keep the
 * latest announcement from each node in their local capability
 * index; {@link MeshNode.findNodes} queries that index.
 *
 * @example
 * ```ts
 * import { MeshNode } from '@ai2070/net-sdk';
 *
 * await node.announceCapabilities({
 *   hardware: {
 *     cpuCores: 16,
 *     memoryGb: 64,
 *     gpu: { vendor: 'nvidia', model: 'RTX 4090', vramGb: 24 },
 *   },
 *   tags: ['gpu', 'inference'],
 *   models: [{ modelId: 'llama-3.1-70b', family: 'llama' }],
 * });
 *
 * const peers = node.findNodes({ requireTags: ['gpu'], minVramMb: 16_384 });
 * ```
 *
 * Multi-hop propagation is deferred; today peers more than one hop
 * away are not visible.
 */

// ----------------------------------------------------------------------------
// GPU / Accelerator / Hardware
// ----------------------------------------------------------------------------

/**
 * GPU vendor. Case-insensitive on input (`'NVIDIA'`, `'nvidia'`,
 * `'Nvidia'` all normalize to `'nvidia'`). Unknown / misspelled
 * vendors collapse to `'unknown'`.
 */
export type GpuVendor =
  | 'nvidia'
  | 'amd'
  | 'intel'
  | 'apple'
  | 'qualcomm'
  | 'unknown';

export interface GpuInfo {
  vendor?: GpuVendor;
  model: string;
  vramGb: number;
  computeUnits?: number;
  tensorCores?: number;
  /** FP16 TFLOPS × 10 (integer) — e.g. 825 for 82.5 TFLOPS. */
  fp16TflopsX10?: number;
}

export type AcceleratorKind =
  | 'tpu'
  | 'npu'
  | 'fpga'
  | 'asic'
  | 'dsp'
  | 'unknown';

export interface Accelerator {
  kind: AcceleratorKind;
  model: string;
  memoryGb?: number;
  /** TOPS × 10 (integer). */
  topsX10?: number;
}

export interface Hardware {
  cpuCores?: number;
  cpuThreads?: number;
  memoryGb?: number;
  gpu?: GpuInfo;
  additionalGpus?: GpuInfo[];
  /** Storage in GB. BigInt to carry multi-TB values without loss. */
  storageGb?: bigint;
  networkMbps?: number;
  accelerators?: Accelerator[];
}

// ----------------------------------------------------------------------------
// Software
// ----------------------------------------------------------------------------

/** `[runtime_name, version]` pair used by runtimes/frameworks/drivers. */
export type SoftwarePair = [string, string];

export interface Software {
  os?: string;
  osVersion?: string;
  runtimes?: SoftwarePair[];
  frameworks?: SoftwarePair[];
  cudaVersion?: string;
  drivers?: SoftwarePair[];
}

// ----------------------------------------------------------------------------
// Models / Tools
// ----------------------------------------------------------------------------

export type Modality =
  | 'text'
  | 'image'
  | 'audio'
  | 'video'
  | 'code'
  | 'embedding'
  | 'tool-use';

export interface ModelCapability {
  modelId: string;
  family?: string;
  /**
   * Parameter count, billions × 10 (70 B ⇒ 700). Integer-encoded to
   * avoid float precision loss; the core uses the same layout.
   */
  parametersBX10?: number;
  contextLength?: number;
  quantization?: string;
  modalities?: Modality[];
  tokensPerSec?: number;
  loaded?: boolean;
}

export interface ToolCapability {
  toolId: string;
  name?: string;
  version?: string;
  /** JSON-Schema string. */
  inputSchema?: string;
  /** JSON-Schema string. */
  outputSchema?: string;
  requires?: string[];
  estimatedTimeMs?: number;
  stateless?: boolean;
}

// ----------------------------------------------------------------------------
// Resource limits
// ----------------------------------------------------------------------------

export interface CapabilityLimits {
  maxConcurrentRequests?: number;
  maxTokensPerRequest?: number;
  rateLimitRpm?: number;
  maxBatchSize?: number;
  maxInputBytes?: number;
  maxOutputBytes?: number;
}

// ----------------------------------------------------------------------------
// Top-level set + filter
// ----------------------------------------------------------------------------

export interface CapabilitySet {
  hardware?: Hardware;
  software?: Software;
  models?: ModelCapability[];
  tools?: ToolCapability[];
  tags?: string[];
  limits?: CapabilityLimits;
}

export interface CapabilityFilter {
  requireTags?: string[];
  requireModels?: string[];
  requireTools?: string[];
  minMemoryMb?: number;
  requireGpu?: boolean;
  gpuVendor?: GpuVendor;
  minVramMb?: number;
  minContextLength?: number;
  requireModalities?: Modality[];
}

// ----------------------------------------------------------------------------
// Conversion helpers — bridge TS interfaces ↔ NAPI POJOs. These are
// exported so the mesh wrapper can consume them without TS having to
// import from @ai2070/net directly.
// ----------------------------------------------------------------------------

interface NapiGpuInfo {
  vendor?: string;
  model: string;
  vramGb: number;
  computeUnits?: number;
  tensorCores?: number;
  fp16TflopsX10?: number;
}

interface NapiAccelerator {
  kind: string;
  model: string;
  memoryGb?: number;
  topsX10?: number;
}

interface NapiHardware {
  cpuCores?: number;
  cpuThreads?: number;
  memoryGb?: number;
  gpu?: NapiGpuInfo;
  additionalGpus?: NapiGpuInfo[];
  storageGb?: bigint;
  networkMbps?: number;
  accelerators?: NapiAccelerator[];
}

interface NapiSoftware {
  os?: string;
  osVersion?: string;
  runtimes?: string[][];
  frameworks?: string[][];
  cudaVersion?: string;
  drivers?: string[][];
}

interface NapiModel {
  modelId: string;
  family?: string;
  parametersBX10?: number;
  contextLength?: number;
  quantization?: string;
  modalities?: string[];
  tokensPerSec?: number;
  loaded?: boolean;
}

interface NapiTool {
  toolId: string;
  name?: string;
  version?: string;
  inputSchema?: string;
  outputSchema?: string;
  requires?: string[];
  estimatedTimeMs?: number;
  stateless?: boolean;
}

interface NapiLimits {
  maxConcurrentRequests?: number;
  maxTokensPerRequest?: number;
  rateLimitRpm?: number;
  maxBatchSize?: number;
  maxInputBytes?: number;
  maxOutputBytes?: number;
}

/** Shape that napi-rs expects for `announceCapabilities`. */
export interface NapiCapabilitySet {
  hardware?: NapiHardware;
  software?: NapiSoftware;
  models?: NapiModel[];
  tools?: NapiTool[];
  tags?: string[];
  limits?: NapiLimits;
}

/** Shape that napi-rs expects for `findNodes`. */
export interface NapiCapabilityFilter {
  requireTags?: string[];
  requireModels?: string[];
  requireTools?: string[];
  minMemoryMb?: number;
  requireGpu?: boolean;
  gpuVendor?: string;
  minVramMb?: number;
  minContextLength?: number;
  requireModalities?: string[];
}

function gpuToNapi(g: GpuInfo): NapiGpuInfo {
  return {
    vendor: g.vendor,
    model: g.model,
    vramGb: g.vramGb,
    computeUnits: g.computeUnits,
    tensorCores: g.tensorCores,
    fp16TflopsX10: g.fp16TflopsX10,
  };
}

function acceleratorToNapi(a: Accelerator): NapiAccelerator {
  return {
    kind: a.kind,
    model: a.model,
    memoryGb: a.memoryGb,
    topsX10: a.topsX10,
  };
}

function hardwareToNapi(h: Hardware): NapiHardware {
  return {
    cpuCores: h.cpuCores,
    cpuThreads: h.cpuThreads,
    memoryGb: h.memoryGb,
    gpu: h.gpu ? gpuToNapi(h.gpu) : undefined,
    additionalGpus: h.additionalGpus?.map(gpuToNapi),
    storageGb: h.storageGb,
    networkMbps: h.networkMbps,
    accelerators: h.accelerators?.map(acceleratorToNapi),
  };
}

function pairToArray(p: SoftwarePair): string[] {
  return [p[0], p[1]];
}

function softwareToNapi(s: Software): NapiSoftware {
  return {
    os: s.os,
    osVersion: s.osVersion,
    runtimes: s.runtimes?.map(pairToArray),
    frameworks: s.frameworks?.map(pairToArray),
    cudaVersion: s.cudaVersion,
    drivers: s.drivers?.map(pairToArray),
  };
}

function modelToNapi(m: ModelCapability): NapiModel {
  return {
    modelId: m.modelId,
    family: m.family,
    parametersBX10: m.parametersBX10,
    contextLength: m.contextLength,
    quantization: m.quantization,
    modalities: m.modalities as string[] | undefined,
    tokensPerSec: m.tokensPerSec,
    loaded: m.loaded,
  };
}

function toolToNapi(t: ToolCapability): NapiTool {
  return {
    toolId: t.toolId,
    name: t.name,
    version: t.version,
    inputSchema: t.inputSchema,
    outputSchema: t.outputSchema,
    requires: t.requires,
    estimatedTimeMs: t.estimatedTimeMs,
    stateless: t.stateless,
  };
}

export function capabilitySetToNapi(caps: CapabilitySet): NapiCapabilitySet {
  return {
    hardware: caps.hardware ? hardwareToNapi(caps.hardware) : undefined,
    software: caps.software ? softwareToNapi(caps.software) : undefined,
    models: caps.models?.map(modelToNapi),
    tools: caps.tools?.map(toolToNapi),
    tags: caps.tags,
    limits: caps.limits,
  };
}

export function capabilityFilterToNapi(f: CapabilityFilter): NapiCapabilityFilter {
  return {
    requireTags: f.requireTags,
    requireModels: f.requireModels,
    requireTools: f.requireTools,
    minMemoryMb: f.minMemoryMb,
    requireGpu: f.requireGpu,
    gpuVendor: f.gpuVendor,
    minVramMb: f.minVramMb,
    minContextLength: f.minContextLength,
    requireModalities: f.requireModalities as string[] | undefined,
  };
}

// =====================================================
// Scope filter (reserved-tag discovery filter)
// =====================================================

/**
 * Caller's intent for narrowing peer discovery by reserved
 * `scope:*` tags. See {@link MeshNode.findNodesScoped}.
 *
 * Tag-based scope is a query-time concern — the wire format is
 * untouched. Untagged peers resolve to `Global` and stay visible
 * under most filters by design (matches the v1-permissive
 * default). Peers tagged `scope:subnet-local` only show up under
 * `sameSubnet`.
 */
export type ScopeFilter =
  | { kind: 'any' }
  | { kind: 'globalOnly' }
  | { kind: 'sameSubnet' }
  | { kind: 'tenant'; tenant: string }
  | { kind: 'tenants'; tenants: string[] }
  | { kind: 'region'; region: string }
  | { kind: 'regions'; regions: string[] };

/** Shape that napi-rs expects for `findNodesScoped`. */
export interface NapiScopeFilter {
  kind: string;
  tenant?: string;
  tenants?: string[];
  region?: string;
  regions?: string[];
}

export function scopeFilterToNapi(s: ScopeFilter): NapiScopeFilter {
  switch (s.kind) {
    case 'any':
    case 'globalOnly':
    case 'sameSubnet':
      return { kind: s.kind };
    case 'tenant':
      return { kind: 'tenant', tenant: s.tenant };
    case 'tenants':
      return { kind: 'tenants', tenants: s.tenants };
    case 'region':
      return { kind: 'region', region: s.region };
    case 'regions':
      return { kind: 'regions', regions: s.regions };
  }
}

// =====================================================
// Reserved scope tag helpers
// =====================================================

/** Reserved tag prefix for tenant-scoped announcements. */
export const SCOPE_TENANT_PREFIX = 'scope:tenant:';
/** Reserved tag prefix for region-scoped announcements. */
export const SCOPE_REGION_PREFIX = 'scope:region:';
/** Reserved tag marking an announcement subnet-local. */
export const SCOPE_SUBNET_LOCAL = 'scope:subnet-local';

/**
 * Append a `scope:tenant:<id>` tag to a tag list. Idempotent —
 * safe to call repeatedly with the same id. Empty `tenantId` is a
 * no-op.
 */
export function withTenantScope(
  tags: string[] | undefined,
  tenantId: string,
): string[] {
  if (!tenantId) return tags ?? [];
  const tag = `${SCOPE_TENANT_PREFIX}${tenantId}`;
  const list = tags ?? [];
  return list.includes(tag) ? list : [...list, tag];
}

/** Append a `scope:region:<name>` tag to a tag list. Idempotent. */
export function withRegionScope(
  tags: string[] | undefined,
  region: string,
): string[] {
  if (!region) return tags ?? [];
  const tag = `${SCOPE_REGION_PREFIX}${region}`;
  const list = tags ?? [];
  return list.includes(tag) ? list : [...list, tag];
}

/**
 * Append the `scope:subnet-local` tag to a tag list. Idempotent.
 * Strictest form wins on the resolver — when this tag is present,
 * tenant/region tags on the same set are ignored by
 * `CapabilityScope`.
 */
export function withSubnetLocalScope(tags: string[] | undefined): string[] {
  const list = tags ?? [];
  return list.includes(SCOPE_SUBNET_LOCAL)
    ? list
    : [...list, SCOPE_SUBNET_LOCAL];
}
