/**
 * Lobbies over the local mesh: list, join by code, capacity, kick,
 * presence, and what a list refuses to believe.
 */

import { afterEach, describe, expect, it } from 'vitest';

import {
  createLobby,
  joinLobby,
  listLobbies,
  LobbyError,
  lobbyCodeFromUrl,
  normalizeLobbyCode,
  type Lobby,
} from '../src/lobby.js';
import { createLocalMesh } from '../src/local.js';
import { defineStore } from '../src/store/definition.js';

interface Room {
  readonly seats: Record<string, number>;
}
type Actions = { sit: { input: Record<string, never>; output: { readonly seat: number } } };
type Inputs = Record<string, never>;

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null) throw new Error('not a record');
  return value as Record<string, unknown>;
}

const room = defineStore<Room, Actions, Inputs>({
  id: 'lobby-test.room',
  version: 1,
  state: value => {
    const seats: Record<string, number> = {};
    for (const [peer, seat] of Object.entries(record(record(value).seats))) seats[peer] = Number(seat);
    return { seats };
  },
  empty: () => ({ seats: {} }),
  actions: { sit: { input: () => ({}), output: value => ({ seat: Number(record(value).seat) }) } },
  inputs: {},
});

const open: Lobby<Room, Actions, Inputs>[] = [];
afterEach(async () => {
  await Promise.all(open.splice(0).map(lobby => lobby.close()));
});

async function lobbyOn(mesh: ReturnType<typeof createLocalMesh>, extra: Partial<Parameters<typeof createLobby>[0]> = {}) {
  const lobby = await createLobby<Room, Actions, Inputs>({
    node: mesh.node(),
    game: 'lobby-test',
    name: 'Friday arena',
    capacity: 3,
    info: { map: 'dunes' },
    definition: room,
    initialState: { seats: {} },
    project: state => state,
    actions: {
      sit: (_input, context) => {
        const seat = Object.keys(context.getState().seats).length + 1;
        context.setState({ seats: { ...context.getState().seats, [context.peer]: seat } });
        return { seat };
      },
    },
    inputs: {},
    ...(extra as object),
  });
  open.push(lobby);
  return lobby;
}

async function settle(): Promise<void> {
  for (let turn = 0; turn < 40; turn += 1) await Promise.resolve();
}

describe('lobbies', () => {
  it('lists a public lobby with its record and the announcing host', async () => {
    const mesh = createLocalMesh();
    const lobby = await lobbyOn(mesh);
    await settle();
    const lobbies = await listLobbies({ node: mesh.node(), game: 'lobby-test' });
    expect(lobbies).toEqual([
      {
        code: lobby.code,
        name: 'Friday arena',
        players: 1,
        capacity: 3,
        store: 'lobby-test.room@1',
        info: { map: 'dunes' },
        host: lobby.host.authority,
      },
    ]);
    expect(await listLobbies({ node: mesh.node(), game: 'other-game' })).toEqual([]);
  });

  it('joins by code — typed loosely — and plays; the host sees the player arrive', async () => {
    const mesh = createLocalMesh();
    const lobby = await lobbyOn(mesh);
    const seen: (readonly string[])[] = [];
    lobby.subscribePlayers(players => seen.push(players));
    const node = mesh.node();
    const typed = `${lobby.code.slice(0, 3).toLowerCase()}-${lobby.code.slice(3)}`;
    const world = await joinLobby({ node, definition: room, game: 'lobby-test', code: typed });
    await world.ready();
    await expect(world.act('sit', {})).resolves.toEqual({ seat: 1 });
    await expect(lobby.self.act('sit', {})).resolves.toEqual({ seat: 2 });
    await settle();
    expect(lobby.players()).toEqual([lobby.host.authority, node.nodeIdHex()]);
    expect(seen.at(-1)).toEqual([lobby.host.authority, node.nodeIdHex()]);
    const [listed] = await listLobbies({ node: mesh.node(), game: 'lobby-test' });
    expect(listed?.players).toBe(2);
  });

  it('turns a player away when full, the host counted', async () => {
    const mesh = createLocalMesh();
    const lobby = await lobbyOn(mesh, { capacity: 2 });
    const first = await joinLobby({ node: mesh.node(), definition: room, game: 'lobby-test', code: lobby.code });
    await first.ready();
    const second = await joinLobby({ node: mesh.node(), definition: room, game: 'lobby-test', code: lobby.code });
    await expect(second.ready()).rejects.toMatchObject({ code: 'forbidden' });
  });

  it('kicks at once, and keeps the kicked player out', async () => {
    const mesh = createLocalMesh();
    const lobby = await lobbyOn(mesh);
    const node = mesh.node();
    const world = await joinLobby({ node, definition: room, game: 'lobby-test', code: lobby.code });
    await world.ready();
    lobby.kick(node.nodeIdHex());
    await settle();
    expect(lobby.players()).toEqual([lobby.host.authority]);
    // The unsolicited `closed` makes the replica rejoin; the rejoin is refused.
    for (let i = 0; i < 20 && world.getStatus().phase !== 'failed'; i += 1) await settle();
    expect(world.getStatus().error?.code).toBe('forbidden');
    expect(() => lobby.kick(lobby.host.authority)).toThrow(LobbyError);
  });

  it("tells onEvent a kicked player left because they were refused", async () => {
    const mesh = createLocalMesh();
    const events: unknown[] = [];
    const lobby = await lobbyOn(mesh, { onEvent: (event: unknown) => events.push(event) } as never);
    const node = mesh.node();
    const world = await joinLobby({ node, definition: room, game: 'lobby-test', code: lobby.code });
    await world.ready();
    lobby.kick(node.nodeIdHex());
    await settle();
    expect(events).toContainEqual({ type: 'leave', peer: node.nodeIdHex(), reason: 'refused' });
  });

  it('keeps an unlisted lobby out of the list, but joinable by code', async () => {
    const mesh = createLocalMesh();
    const lobby = await lobbyOn(mesh, { visibility: 'unlisted' });
    await settle();
    expect(await listLobbies({ node: mesh.node(), game: 'lobby-test' })).toEqual([]);
    const world = await joinLobby({ node: mesh.node(), definition: room, game: 'lobby-test', code: lobby.code });
    await world.ready();
  });

  it('refuses a code two nodes claim, rather than guessing which host is real', async () => {
    const mesh = createLocalMesh();
    const lobby = await lobbyOn(mesh);
    await settle();
    const [codeTag] = (await mesh.node().query('net-lobby:lobby-test')).flatMap(d =>
      d.capabilities.filter(tag => tag.includes(':code:')),
    );
    const impostor = mesh.node();
    await impostor.announce([codeTag!]);
    await expect(
      joinLobby({ node: mesh.node(), definition: room, game: 'lobby-test', code: lobby.code, timeoutMs: 0 }),
    ).rejects.toMatchObject({ code: 'ambiguous' });
  });

  it('reports a code nobody answers as not-found', async () => {
    const mesh = createLocalMesh();
    await expect(
      joinLobby({ node: mesh.node(), definition: room, game: 'lobby-test', code: 'AAAAAA', timeoutMs: 0 }),
    ).rejects.toMatchObject({ code: 'not-found' });
  });

  it('skips records that do not check out', async () => {
    const mesh = createLocalMesh();
    const liar = mesh.node();
    const bad = (json: string) =>
      `net-lobby:lobby-test:rec:${btoa(json).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')}`;
    await liar.announce(['net-lobby:lobby-test', bad('{"v":1,"c":"AAAAAA","n":"x","p":9,"m":2,"s":"a@1","i":{}}')]);
    const other = mesh.node();
    await other.announce(['net-lobby:lobby-test', bad('not json')]);
    expect(await listLobbies({ node: mesh.node(), game: 'lobby-test' })).toEqual([]);
  });

  it('withdraws the lobby on close', async () => {
    const mesh = createLocalMesh();
    const lobby = await lobbyOn(mesh);
    await settle();
    await lobby.close();
    await settle();
    expect(await listLobbies({ node: mesh.node(), game: 'lobby-test' })).toEqual([]);
  });

  it('refuses what it cannot publish', async () => {
    const mesh = createLocalMesh();
    await expect(lobbyOn(mesh, { game: 'Bad Game' })).rejects.toMatchObject({ code: 'invalid' });
    await expect(lobbyOn(mesh, { info: { blob: 'x'.repeat(300) } })).rejects.toMatchObject({ code: 'invalid' });
    await expect(lobbyOn(mesh, { capacity: 0 })).rejects.toMatchObject({ code: 'invalid' });
  });

  it('round-trips a code through a link', async () => {
    const mesh = createLocalMesh();
    const lobby = await lobbyOn(mesh);
    const link = lobby.link('https://game.example/play?x=1');
    expect(lobbyCodeFromUrl(link)).toBe(lobby.code);
    expect(lobbyCodeFromUrl('https://game.example/')).toBeNull();
    expect(normalizeLobbyCode(' k7q-p2m ')).toBe('K7QP2M');
    expect(() => normalizeLobbyCode('K7QP2O')).toThrow(LobbyError);
  });
});
