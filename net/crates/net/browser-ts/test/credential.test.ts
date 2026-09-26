// `requestCredential`: the page half of an anchor's `POST /credential`.
// The responses below are the shapes the Rust route produces
// (`sdk/src/rtc_bootstrap.rs` `post_credential`, witnessed in
// `sdk/tests/rtc_bootstrap_listener.rs`).

import { describe, expect, it } from 'vitest';

import { CredentialRequestError, requestCredential } from '../src/index.js';

type Call = { url: string; init: RequestInit };

function fakeFetch(respond: (call: Call) => Response | Promise<Response>): { fetch: typeof fetch; calls: Call[] } {
  const calls: Call[] = [];
  const fn = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const call = { url: String(input), init: init ?? {} };
    calls.push(call);
    return respond(call);
  }) as typeof fetch;
  return { fetch: fn, calls };
}

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });

async function failure(promise: Promise<unknown>): Promise<CredentialRequestError> {
  try {
    await promise;
  } catch (error) {
    expect(error).toBeInstanceOf(CredentialRequestError);
    return error as CredentialRequestError;
  }
  throw new Error('expected a CredentialRequestError');
}

describe('requestCredential', () => {
  it('posts the game to <anchor>/credential and returns what connect() takes', async () => {
    const { fetch, calls } = fakeFetch(() =>
      json(200, { credentialB64: 'net-bootstrap:abc', bootstrapUrl: 'https://anchor.example', game: 'my-game' }),
    );
    const credential = await requestCredential({ anchorUrl: 'https://anchor.example/', game: 'my-game', fetch });
    expect(credential).toEqual({ credentialB64: 'net-bootstrap:abc', bootstrapUrl: 'https://anchor.example', game: 'my-game' });
    expect(calls).toHaveLength(1);
    expect(calls[0]!.url).toBe('https://anchor.example/credential');
    expect(calls[0]!.init.method).toBe('POST');
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({ game: 'my-game' });
  });

  it.each([
    [404, 'unknown_game', 'unknown-game'],
    [429, 'rate_limited', 'rate-limited'],
    [400, 'malformed_request', 'malformed-request'],
  ] as const)('maps a %i %s refusal to %s, keeping the message', async (status, refusal, kind) => {
    const { fetch } = fakeFetch(() => json(status, { refusal, message: `anchor says ${refusal}` }));
    const error = await failure(requestCredential({ anchorUrl: 'https://a', game: 'g', fetch }));
    expect(error.kind).toBe(kind);
    expect(error.status).toBe(status);
    expect(error.message).toBe(`anchor says ${refusal}`);
  });

  it('reads a bare 404 as an anchor that issues no credentials', async () => {
    const { fetch } = fakeFetch(() => new Response('', { status: 404 }));
    const error = await failure(requestCredential({ anchorUrl: 'https://a', game: 'g', fetch }));
    expect(error.kind).toBe('unreachable');
    expect(error.message).toMatch(/--game/);
  });

  it('types a network failure as unreachable, and passes an abort through', async () => {
    const { fetch } = fakeFetch(() => {
      throw new TypeError('Failed to fetch');
    });
    expect((await failure(requestCredential({ anchorUrl: 'https://a', game: 'g', fetch }))).kind).toBe('unreachable');

    const controller = new AbortController();
    controller.abort();
    const aborting = fakeFetch(() => {
      throw new DOMException('aborted', 'AbortError');
    });
    await expect(
      requestCredential({ anchorUrl: 'https://a', game: 'g', fetch: aborting.fetch, signal: controller.signal }),
    ).rejects.toThrow(/aborted/);
  });

  it('refuses a 200 without a credential', async () => {
    const { fetch } = fakeFetch(() => json(200, { game: 'g' }));
    expect((await failure(requestCredential({ anchorUrl: 'https://a', game: 'g', fetch }))).kind).toBe('unexpected');
  });
});
