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
