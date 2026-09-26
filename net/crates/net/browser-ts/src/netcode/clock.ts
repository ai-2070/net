/**
 * Clock sync: one timeline for the host and its players.
 *
 * NTP-style. A player stamps a ping with its own clock (`t0`); the host
 * stamps receipt (`t1`) and reply (`t2`) with its clock; the player stamps
 * arrival (`t3`). Then
 *
 * ```text
 * rtt    = (t3 - t0) - (t2 - t1)
 * offset = ((t1 - t0) + (t2 - t3)) / 2      // host clock − player clock
 * ```
 *
 * One sample is noisy — queueing on either path skews it — so the
 * estimator keeps a window and takes the offset from the **lowest-RTT**
 * samples, which are the ones least distorted by queueing. Jitter is the
 * mean absolute deviation of RTT across the window. Pure arithmetic; the
 * transport and timers live in the netcode client.
 */

/** A clock reading in milliseconds (e.g. `performance.now()`). */
export type Millis = number;

/** What the estimator currently believes. */
export interface ClockEstimate {
  /** Host clock minus this clock, in ms. `hostNow = now + offset`. */
  readonly offsetMs: number;
  /** Smoothed round-trip time (median of the window), in ms. */
  readonly rttMs: number;
  /** Mean absolute deviation of round-trip time, in ms. */
  readonly jitterMs: number;
  /** Samples the estimate rests on. */
  readonly samples: number;
}

interface Sample {
  readonly rtt: number;
  readonly offset: number;
}

/** Samples kept. */
export const CLOCK_WINDOW = 16;
/** Of the window, how many lowest-RTT samples the offset averages. */
const BEST = 4;

/** Accumulates ping/pong samples into a clock estimate. */
export class ClockEstimator {
  readonly #window: Sample[] = [];

  /**
   * Add one exchange. Samples with a negative or non-finite RTT (clock
   * weirdness, a reply to a ping from before a reload) are ignored.
   * Returns whether the sample was used.
   */
  add(t0: Millis, t1: Millis, t2: Millis, t3: Millis): boolean {
    const rtt = t3 - t0 - (t2 - t1);
    const offset = (t1 - t0 + (t2 - t3)) / 2;
    if (!Number.isFinite(rtt) || !Number.isFinite(offset) || rtt < 0) return false;
    this.#window.push({ rtt, offset });
    if (this.#window.length > CLOCK_WINDOW) this.#window.shift();
    return true;
  }

  /** The current estimate, or `null` before the first sample. */
  estimate(): ClockEstimate | null {
    const window = this.#window;
    if (window.length === 0) return null;
    const byRtt = [...window].sort((a, b) => a.rtt - b.rtt);
    const best = byRtt.slice(0, Math.min(BEST, byRtt.length));
    const offsetMs = best.reduce((sum, s) => sum + s.offset, 0) / best.length;
    const rttMs = byRtt[Math.floor((byRtt.length - 1) / 2)]!.rtt;
    const mean = window.reduce((sum, s) => sum + s.rtt, 0) / window.length;
    const jitterMs = window.reduce((sum, s) => sum + Math.abs(s.rtt - mean), 0) / window.length;
    return { offsetMs, rttMs, jitterMs, samples: window.length };
  }
}
