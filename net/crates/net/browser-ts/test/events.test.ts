/**
 * The event surface: parsing the leaf's JSON without losing anything,
 * and fanning it out without losing anyone.
 */

import { describe, expect, it, vi } from 'vitest';

import { EventHub, parseEvent, toBase64, type LeafEvent } from '../src/events.js';

const PAYLOAD = new Uint8Array([0xde, 0xad, 0xbe, 0xef, 0x00, 0xff]);

describe('parseEvent', () => {
  it('parses connected, including the rtc_addr the probe needs', () => {
    const event = parseEvent(
      '{"type":"connected","node_id_hex":"beefcafe00000001","peer_node":"7","rtc_addr":"203.0.113.9:4433"}',
    );
    expect(event).toEqual({
      type: 'connected',
      nodeIdHex: 'beefcafe00000001',
      peerNode: '7',
      rtcAddr: '203.0.113.9:4433',
    });
  });

  it('reads a null rtc_addr as absent rather than as the string "null"', () => {
    const event = parseEvent('{"type":"connected","node_id_hex":"aa","peer_node":"7","rtc_addr":null}');
    expect(event.type === 'connected' && event.rtcAddr).toBeNull();
  });

  it('decodes base64 payloads to bytes', () => {
    const event = parseEvent(
      `{"type":"channel_message","channel_hash":"12","origin_hash":"34","payload":"${toBase64(PAYLOAD)}"}`,
    );
    expect(event.type === 'channel_message' && event.payload).toEqual(PAYLOAD);
  });

  it('keeps a u64 id exact even when the leaf emits it as a JSON number', () => {
    // 18446744073709551615 parses to 18446744073709552000 as a JS
    // number: a page filtering on this hash would match the wrong
    // channel. The value must survive as typed.
    const event = parseEvent(
      '{"type":"channel_message","channel_hash":18446744073709551615,"origin_hash":9007199254740993,"payload":""}',
    );
    expect(event.type === 'channel_message' && event.channelHash).toBe('18446744073709551615');
    expect(event.type === 'channel_message' && event.originHash).toBe('9007199254740993');
  });

  it('keeps a quoted u64 exactly as sent', () => {
    const event = parseEvent('{"type":"stream_data","stream_id":"18446744073709551615","seq":3,"payload":""}');
    expect(event.type === 'stream_data' && event.streamId).toBe('18446744073709551615');
    expect(event.type === 'stream_data' && event.seq).toBe(3);
  });

  it('re-types the leaf error an rtc_failure carries', () => {
    const event = parseEvent(
      '{"type":"rtc_failure","error":"rtc: ICE did not connect inside the deadline (this does not establish that UDP is blocked)"}',
    );
    expect(event.type === 'rtc_failure' && event.error.kind).toBe('ice-timeout');
  });

  it('parses an announcement, capabilities and verification included', () => {
    const event = parseEvent(
      '{"type":"announcement","node_id":"18446744073709551615","capabilities":["transcribe","summarise"],"rtc_addr":null,"verified":true}',
    );
    expect(event).toEqual({
      type: 'announcement',
      nodeId: '18446744073709551615',
      capabilities: ['transcribe', 'summarise'],
      rtcAddr: null,
      verified: true,
    });
  });

  it('passes an unknown tag through instead of dropping it', () => {
    const event = parseEvent('{"type":"leader_fenced","generation":"4"}');
    expect(event.type).toBe('unknown');
    expect(event.type === 'unknown' && event.tag).toBe('leader_fenced');
    expect(event.type === 'unknown' && event.raw).toEqual({ type: 'leader_fenced', generation: '4' });
  });

  it('never throws on malformed input — it runs inside a wasm callback', () => {
    const event = parseEvent('{"type":');
    expect(event).toEqual({ type: 'unknown', tag: null, raw: '{"type":' });
  });
});

describe('EventHub', () => {
  it('delivers to typed listeners, any-listeners and iterators at once', async () => {
    const hub = new EventHub();
    const typed: LeafEvent[] = [];
    const all: LeafEvent[] = [];
    hub.on('disconnected', (event) => typed.push(event));
    hub.onAny((event) => all.push(event));
    const iterator = hub.events();

    hub.deliver('{"type":"disconnected","reason":"anchor went away"}');
    hub.deliver('{"type":"dropped","reason":"oversize","subprotocol":2560}');

    expect(typed.map((event) => event.type)).toEqual(['disconnected']);
    expect(all.map((event) => event.type)).toEqual(['disconnected', 'dropped']);
    const first = await iterator.next();
    expect(first.value?.type).toBe('disconnected');
    const second = await iterator.next();
    expect(second.value?.type).toBe('dropped');
    await iterator.return?.();
  });

  it('stops delivering once a listener unsubscribes', () => {
    const hub = new EventHub();
    const seen: string[] = [];
    const cancel = hub.on('disconnected', (event) => seen.push(event.reason));
    hub.deliver('{"type":"disconnected","reason":"first"}');
    cancel();
    hub.deliver('{"type":"disconnected","reason":"second"}');
    expect(seen).toEqual(['first']);
  });

  it('buffers for an iterator that is not awaiting yet', async () => {
    const hub = new EventHub();
    const iterator = hub.events();
    hub.deliver('{"type":"disconnected","reason":"one"}');
    hub.deliver('{"type":"disconnected","reason":"two"}');
    const first = await iterator.next();
    const second = await iterator.next();
    expect([first.value, second.value].map((event) => event?.type)).toEqual(['disconnected', 'disconnected']);
  });

  it('stops buffering for an iterator the consumer has left', async () => {
    const hub = new EventHub();
    const iterator = hub.events();
    await iterator.return?.();
    hub.deliver('{"type":"disconnected","reason":"after the loop"}');
    await expect(iterator.next()).resolves.toEqual({ value: undefined, done: true });
  });

  it('survives a throwing listener and still serves the others', () => {
    const hub = new EventHub();
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    const seen: string[] = [];
    hub.on('disconnected', () => {
      throw new Error('listener bug');
    });
    hub.on('disconnected', (event) => seen.push(event.reason));

    expect(() => hub.deliver('{"type":"disconnected","reason":"still delivered"}')).not.toThrow();
    expect(seen).toEqual(['still delivered']);
    expect(consoleError).toHaveBeenCalled();
    consoleError.mockRestore();
  });

  it('ends iterators and drops listeners on close', async () => {
    const hub = new EventHub();
    const seen: LeafEvent[] = [];
    hub.onAny((event) => seen.push(event));
    const iterator = hub.events();
    hub.close();

    hub.deliver('{"type":"disconnected","reason":"after close"}');
    expect(seen).toEqual([]);
    await expect(iterator.next()).resolves.toEqual({ value: undefined, done: true });
  });
});
