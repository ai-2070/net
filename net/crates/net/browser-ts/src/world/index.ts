/**
 * `@net-mesh/browser/world` — large worlds across region hosts: regions and
 * their directory, a player's merged view (`joinWorld`), at-most-once entity
 * handoff between region hosts, cross-border actions, and ghosting.
 */

export {
  beginHandoff,
  locate,
  onHandoffOffer,
  onHandoffReply,
  pruneHandled,
  regionState,
  reofferHandoff,
  retryHandoffs,
} from './handoff.js';
export type {
  Handled,
  HandoffEvent,
  HandoffId,
  HandoffMessage,
  HandoffOffer,
  HandoffReply,
  HandoffStep,
  HandoffTiming,
  Outgoing,
  RegionState,
} from './handoff.js';
export { BorderActionError, handoffLink, parseHandoffLedger, regionHandoffs, storeRegion } from './link.js';
export { ghostTargets, onForwardedAction, pruneActs } from './border.js';
export type { ActRecord, ActionResult, BorderAction, ForwardedAction, GhostFrame } from './border.js';
export type {
  HandoffLedger,
  HandoffLink,
  HandoffLinkOptions,
  HandoffTransport,
  LinkBody,
  RegionHandoffs,
  RegionHandoffsOptions,
} from './link.js';
export {
  REGION_ANNOUNCE_MS,
  announceRegions,
  joinWorld,
  regionDirectory,
  regionOf,
  regionTag,
  regionsAround,
} from './regions.js';
export type {
  JoinWorldOptions,
  RegionDirectory,
  RegionDirectoryOptions,
  RegionLookup,
  WorldNode,
  WorldRegion,
  WorldView,
} from './regions.js';
