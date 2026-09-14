/**
 * The one-consumer async queue behind every async iterable in this
 * package (`node.events()`, `stream`).
 *
 * Items that arrive while the consumer is not awaiting are buffered in
 * arrival order; buffering stops the moment the loop is left (`break`,
 * `return`, or an exception), which is what calls the `detach` hook, so
 * a page that stops consuming stops paying. A page that starts a loop
 * and then never awaits it will grow the buffer — leave the loop
 * instead of abandoning it.
 */
export class AsyncQueue<T> implements AsyncIterableIterator<T> {
  private readonly buffered: T[] = [];
  private waiting: ((result: IteratorResult<T>) => void) | null = null;
  private ended = false;

  /** @param detach called once when the consumer stops iterating. */
  constructor(private readonly detach: () => void = () => {}) {}

  /** Hand an item to the consumer, or buffer it. */
  push(item: T): void {
    if (this.ended) return;
    const waiting = this.waiting;
    if (waiting) {
      this.waiting = null;
      waiting({ value: item, done: false });
      return;
    }
    this.buffered.push(item);
  }

  /** End the iteration once the buffer drains. */
  end(): void {
    if (this.ended) return;
    this.ended = true;
    const waiting = this.waiting;
    if (waiting) {
      this.waiting = null;
      waiting({ value: undefined, done: true });
    }
  }

  next(): Promise<IteratorResult<T>> {
    const buffered = this.buffered.shift();
    if (buffered !== undefined) return Promise.resolve({ value: buffered, done: false });
    if (this.ended) return Promise.resolve({ value: undefined, done: true });
    const { promise, resolve } = Promise.withResolvers<IteratorResult<T>>();
    this.waiting = resolve;
    return promise;
  }

  return(): Promise<IteratorResult<T>> {
    this.detach();
    this.end();
    return Promise.resolve({ value: undefined, done: true });
  }

  [Symbol.asyncIterator](): AsyncIterableIterator<T> {
    return this;
  }
}
