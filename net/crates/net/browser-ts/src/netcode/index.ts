/**
 * `@net-mesh/browser/netcode` — responsive movement over the lossy carrier
 * (netcode model 2): an authoritative host tick loop with lag compensation,
 * and on each player snapshot interpolation for others plus prediction and
 * reconciliation for yourself.
 *
 * Use it beside the store, not instead of it: the store holds durable,
 * validated state (inventory, score, doors); netcode carries high-rate
 * transforms that are superseded many times a second.
 */

export { hostNetcode } from './host.js';
export type { HostNetcode, HostNetcodeOptions, Rewound, TickContext, TickInput } from './host.js';
export { NetcodeError, joinNetcode } from './client.js';
export type { JoinNetcodeOptions, LocalEntity, NetcodeClient, NetcodeErrorKind, NetcodeStats } from './client.js';
export { joinShards } from './shards.js';
export { assignAction, assignmentProblems, authorizeTeams, leaseWatch, mayReassign, renewLeases, shardAssignment, teamOf } from './assignment.js';
export type { LeaseWatch, LeaseWatchOptions, MatchAssignment, ShardRecord, Squad } from './assignment.js';
export type { JoinShardsOptions, ShardAssignment, ShardStats, ShardedMatch, ShardedMatchStats } from './shards.js';
export { ClockEstimator, CLOCK_WINDOW } from './clock.js';
export type { ClockEstimate, Millis } from './clock.js';
export { SnapshotBuffer, lerpNumbers, SNAPSHOT_BUFFER } from './interpolate.js';
export type { Interpolator, TimedSnapshot } from './interpolate.js';
export type { NetcodeStream, NetcodeTransport } from './wire.js';
export { angle, bool, entityCodec, float32, int, quantized, uint } from './codec.js';
export type { EntityCodec, FieldCodec } from './codec.js';

// The netcode wire format and its helpers, exported so callers can
// build their own transports and tooling on the same frames.
export {
  snapshotFrames,
} from './host.js';
export {
  BINARY_FRAME_MAGIC,
  decodeBinarySnapshot,
  encodeBinarySnapshot,
  isBinaryFrame,
  labelHash,
} from './codec.js';
export type { BinarySnapshot } from './codec.js';
export {
  MAX_NAME_REQUESTS,
  chunkOf,
  decodeFrame,
  defaultNow,
  encodeFrame,
  eventPeer,
  MAX_NETCODE_INTEREST_KEY_LENGTH,
  MAX_NETCODE_INTEREST_KEYS,
  MAX_SNAPSHOT_CHUNKS,
  peerHex,
} from './wire.js';
export type {
  Frame,
  Now,
  WireInput,
} from './wire.js';
