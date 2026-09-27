/**
 * The bones of a player inventory: stacks of items, kept in the store's
 * state, changed only by the host.
 *
 * An inventory is plain JSON — item id → count — so it rides in any
 * store document, diffs like the rest of it, and needs no protocol of
 * its own. These are pure functions: call them inside an action
 * handler or the `onEvent` hook and `setState` the result. A change
 * that breaks the rules throws, which inside a handler refuses the
 * action and discards its writes.
 *
 * ```ts
 * // state: { inventories: Record<peer, Inventory>, … }
 * onEvent: (event, context) => {
 *   if (event.type !== 'join') return;
 *   const inventories = { ...context.getState().inventories };
 *   inventories[event.peer] ??= addItems(emptyInventory(), 'potion', 3, rules);
 *   context.setState({ inventories });
 * },
 * actions: {
 *   drink: (_input, context) => {
 *     const inventories = { ...context.getState().inventories };
 *     inventories[context.peer] = removeItems(inventoryOf(inventories, context.peer), 'potion', 1);
 *     context.setState({ inventories });
 *     return { left: countItems(inventories[context.peer], 'potion') };
 *   },
 * },
 * projectFor: (state, { peer }) => ({ ...state, inventories: onlyOwn(state.inventories, peer) }),
 * ```
 *
 * **Deliberately not here yet:** trading between players (it needs both
 * sides' consent and one atomic transfer), equipment slots, item
 * metadata. Item ids are yours; what an item *is* stays game data.
 */

/** Item id → how many. Every count is a whole number ≥ 1; absent means none. */
export type Inventory = Readonly<Record<string, number>>;

/** Limits an inventory keeps. Every limit is optional. */
export interface InventoryRules {
  /** How many different items it may hold at once. */
  readonly maxKinds?: number;
  /** The most of one item: one number for all, or per item id (missing ids unlimited). */
  readonly maxCount?: number | Readonly<Record<string, number>>;
}

/** Why an inventory change was refused. */
export type InventoryErrorCode = 'insufficient' | 'full' | 'invalid';

export class InventoryError extends Error {
  constructor(
    readonly code: InventoryErrorCode,
    message: string,
  ) {
    super(message);
    this.name = 'InventoryError';
  }
}

/** Longest item id accepted. Ids are keys in a document every player may receive. */
export const MAX_ITEM_ID_CHARS = 64;

const EMPTY: Inventory = Object.freeze({});

/** An inventory with nothing in it. */
export function emptyInventory(): Inventory {
  return EMPTY;
}

function checkItem(item: string): void {
  if (typeof item !== 'string' || item.length === 0 || item.length > MAX_ITEM_ID_CHARS || item === '__proto__') {
    throw new InventoryError('invalid', `an item id is 1-${MAX_ITEM_ID_CHARS} characters, got ${JSON.stringify(item)}`);
  }
}

function checkCount(count: number): void {
  if (!Number.isSafeInteger(count) || count < 1) {
    throw new InventoryError('invalid', `a count is a whole number of at least 1, got ${String(count)}`);
  }
}

function limitFor(item: string, rules: InventoryRules | undefined): number {
  const max = rules?.maxCount;
  if (max === undefined) return Number.MAX_SAFE_INTEGER;
  if (typeof max === 'number') return max;
  return Object.prototype.hasOwnProperty.call(max, item) ? max[item]! : Number.MAX_SAFE_INTEGER;
}

/** How many of `item` it holds. */
export function countItems(inventory: Inventory | undefined, item: string): number {
  if (inventory === undefined || !Object.prototype.hasOwnProperty.call(inventory, item)) return 0;
  return inventory[item] ?? 0;
}

/** Whether it holds at least `count` of `item`. */
export function hasItems(inventory: Inventory | undefined, item: string, count = 1): boolean {
  return countItems(inventory, item) >= count;
}

/**
 * Add `count` of `item`. Throws `full` when that would break a rule;
 * nothing is partly added.
 */
export function addItems(inventory: Inventory, item: string, count: number, rules?: InventoryRules): Inventory {
  checkItem(item);
  checkCount(count);
  const had = countItems(inventory, item);
  const next = had + count;
  if (!Number.isSafeInteger(next) || next > limitFor(item, rules)) {
    throw new InventoryError('full', `no room for ${String(count)} more ${item}`);
  }
  if (had === 0 && rules?.maxKinds !== undefined && Object.keys(inventory).length >= rules.maxKinds) {
    throw new InventoryError('full', `no room for another kind of item (${String(rules.maxKinds)} at most)`);
  }
  return Object.freeze({ ...inventory, [item]: next });
}

/**
 * Remove `count` of `item`. Throws `insufficient` when it holds fewer;
 * nothing is partly removed. An item that reaches zero is gone.
 */
export function removeItems(inventory: Inventory, item: string, count: number): Inventory {
  checkItem(item);
  checkCount(count);
  const had = countItems(inventory, item);
  if (had < count) throw new InventoryError('insufficient', `has ${String(had)} ${item}, needs ${String(count)}`);
  const next: Record<string, number> = { ...inventory };
  if (had === count) delete next[item];
  else next[item] = had - count;
  return Object.freeze(next);
}

/**
 * A validator for an inventory inside a store definition's `state`:
 * accepts only whole positive counts under valid item ids, and the
 * rules if given. Throws otherwise, as a definition validator should.
 */
export function parseInventory(value: unknown, rules?: InventoryRules): Inventory {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new InventoryError('invalid', 'an inventory is an object of item id → count');
  }
  let out: Inventory = EMPTY;
  for (const [item, count] of Object.entries(value)) {
    if (typeof count !== 'number') throw new InventoryError('invalid', `the count of ${item} is not a number`);
    out = addItems(out, item, count, rules);
  }
  return out;
}

/** The inventory kept for `peer` in a map of them, or an empty one. */
export function inventoryOf(inventories: Readonly<Record<string, Inventory>>, peer: string): Inventory {
  return Object.prototype.hasOwnProperty.call(inventories, peer) ? inventories[peer]! : EMPTY;
}

/**
 * For `projectFor`: the viewer's own entry of a per-player map, and no
 * one else's. Hidden entries are absent — not zeroed — so another
 * player's inventory is never in the frames sent to you.
 */
export function onlyOwn<T>(byPeer: Readonly<Record<string, T>>, peer: string): Record<string, T> {
  return Object.prototype.hasOwnProperty.call(byPeer, peer) ? { [peer]: byPeer[peer]! } : {};
}
