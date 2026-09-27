/**
 * `@net-mesh/browser/world` — large worlds across region hosts (browser
 * plan §9). So far: at-most-once entity handoff between regions.
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
export { onForwardedAction, pruneActs } from './border.js';
export type { ActRecord, ActionResult, BorderAction, ForwardedAction } from './border.js';
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
