# eve + Nitro — Deployment Target and Transport Findings (2026-09-17)

Point-in-time findings on `vercel/eve` and the Nitro server framework it is
built on, gathered while answering three questions:

1. Is eve designed for serverless functions?
2. What is eve's *real* deployment target?
3. Does Nitro have UDP, WebRTC, or just TCP?

Answers, in order: **no (as a design model, though Vercel output is
function-shaped)**, **a Nitro application (`Nitro + Workflows`) with two host
outputs**, and **TCP only**.

Everything below was checked against the sources listed under
[Provenance](#provenance), not against blog posts or memory. Where the docs and
the source disagree, the source wins and the disagreement is recorded. This is
a third-party survey, not a plan against this repository's code — nothing here
proposes a change to Net.

---

## 1. eve is a durable runtime, not a serverless-function design

`packages/eve/README.md` states the framing directly: *"eve is a filesystem-first
framework for durable backend agents on Vercel"*, and *"eve is built to be
durable. The runtime is Nitro + Workflows."*

Its documented runtime surface (`docs/README.md`, "The public mental model") is
a persistent orchestrator, not a request handler:

- a stable HTTP message route and optional channel webhook routes
- a **reconnectable** session stream
- durable session state across turns
- a per-agent sandbox with a shared runtime workspace
- workflow primitives (`start()`, `resumeHook()`, `createHook()`, `getWritable()`)

Sessions carry a durable deadline (30 days by default,
`limits.sessionTimeoutMs`), and stream events are recorded *before* a step
completes so a consumer can reconnect from a cursor
(`docs/concepts/sessions-runs-and-streaming.md`).

### Verdict on the serverless question

- **Wrong as a design model.** Stateless, request-scoped invocation breaks the
  run/callback and session-persistence contract. `docs/guides/deployment/self-hosting.md`
  is explicit about the failure mode: *"A proxy restricted to `/eve/` lets a
  session start, but the run stalls when its callback can't reach eve."* The
  proxy must forward both `/eve/` and `/.well-known/workflow/`.
- **Not wrong at the artifact level.** On Vercel the output *is* Vercel
  Functions (see §2). The functions are the compute substrate; the Workflow
  world is the durability target.

---

## 2. The real deployment target: a Nitro application

`eve build` compiles the agent under `.eve/` and then runs a **Nitro** build
(`packages/eve/src/internal/nitro/host/build-application.ts`, which imports
`build`, `prepare`, `prerender`, `copyPublicAssets` from `nitro/builder`).
Nitro is pinned at `3.0.260903-beta` in `packages/eve/package.json`.

Two host outputs, selected by environment:

| Strategy | Build output | Workflows | Sandbox | Chosen when |
|---|---|---|---|---|
| Vercel | `.vercel/output` | Vercel Workflow | Vercel Sandbox | Vercel operates the runtime services |
| Self-host | `.output/` Node server | Local or custom Workflow world | Docker, microsandbox, or custom | You run your own Node/container infra |

(Table from `docs/guides/deployment/overview.md`.)

### 2.1 Vercel target — concrete artifacts

`createEveVercelOptions()` in
`packages/eve/src/internal/nitro/host/vercel-build-output-config.ts` returns
Nitro's **Vercel preset** options: Build Output API **version 3**,
`framework.slug = "eve"` (this is why CI sets
`VERCEL_USE_EXPERIMENTAL_FRAMEWORKS=1`), and a `functionRules` entry.

| Artifact under `.vercel/output` | What it is |
|---|---|
| `functions/eve/__server.func` | the shared server function — `EVE_SHARED_SERVER_FUNCTION_PATH` in `src/internal/workflow-bundle/eve-service-route-output.ts`. Nitro dedupes every route function through `__server.func`; eve copies it once into `eve/` and repoints the `/eve/v1/**` aliases at it (`normalizeEveVercelFunctionOutput`) |
| `functions/.well-known/workflow/v1/flow.func` | the **queue-triggered workflow function** — route `EVE_WORKFLOW_FLOW_ROUTE_PATH = "/.well-known/workflow/v1/flow"`. Rebuilt from `__server.func` by `materializeVercelWorkflowFunctionOutput` so traced `.nf3` symlinks stay relative |
| `config.json` | routes (non-eve functions pruned) and crons (`normalizeVercelServiceCrons`) |
| services graph | `framework: "eve"`, `outputDirectory: ".vercel/output"`, `experimentalServices` / `experimentalServicesV2` — what `eve/vercel`'s `withEve` and `eve/next` contribute |

The flow function carries its own rules:

```ts
functionRules: {
  [EVE_WORKFLOW_FLOW_ROUTE_PATH]: {
    maxDuration: "max",
    experimentalTriggers: [createEveWorkflowQueueTrigger(input.agentName)],
    environment: { WORKFLOW_PRECONDITION_GUARD: "1" },
  },
}
```

Comment in that file: *"makes Nitro emit a dedicated `flow.func` from the same
build output, carrying the agent's queue trigger, an extended execution window,
and the environment the deployed workflow runtime needs. Every other function
setting (runtime, memory, streaming) is inherited from the base server function
config."*

Deploy path: `eve deploy` → `vercel deploy --prod`; CI does `eve build` under
`VERCEL=1` then `vc deploy --prebuilt`
(`.github/workflows/e2e-vercel.yml`). Backing services: **Vercel Workflow**
(durable run persistence and resume, "optimistic replay preconditions"), **Vercel
Sandbox**, **Vercel Cron**.

### 2.2 Self-host target

Nitro **Node server** preset → `.output/`, run as a long-lived process:

```bash
eve build && PORT=3000 eve start --host 0.0.0.0
```

`docs/guides/deployment/self-hosting.md`: run it *"under the same process manager
or container platform you use for other Node web services."* Default workflow
world is file-backed under `.eve/.workflow-data` and **must be mounted on
persistent storage** so runs survive process replacement; alternatively select
an installed Workflow world package (`experimental.workflow.world`), built
against the same `@workflow/*` line (current line `5.0.0-beta`).

### 2.3 Not the app deployment target

`packages/eve/Dockerfile` is labeled *"Sandbox base image for eve agents"*
(Ubuntu 26.04, Node 24, pnpm, `vercel-sandbox` user). That is the per-agent
sandbox image — not how eve itself deploys.

---

## 3. Nitro transport surface: TCP only

Nitro is an HTTP server framework (h3 over Web-standard `Request`/`Response`).
Every transport it offers is HTTP-over-TCP, plus WebSocket (HTTP Upgrade, also
TCP over a single connection).

| Transport | Support | Evidence |
|---|---|---|
| HTTP/1.1 | default | `node_server` preset → `node .output/server/index.mjs`, "Listening on http://localhost:3000" |
| HTTP/2 | not a preset feature; supply your own listener | `node_middleware` preset exports a middleware fn for a custom `node:http`/`node:http2` server |
| HTTPS | `NITRO_SSL_CERT` + `NITRO_SSL_KEY`, "intended for testing only; in production, run behind a reverse proxy that terminates SSL" | `nitro.build/deploy/runtimes/node` |
| UNIX socket | `NITRO_UNIX_SOCKET` (IPC, not network) | same page |
| WebSocket | opt-in `features: { websocket: true }`, `defineWebSocketHandler`, pub/sub, powered by CrossWS | `nitro.build/docs/websocket` |
| SSE | recommended over WS for one-way server→client: "They use plain HTTP and reconnect automatically" | same page |

### Absent

- **No UDP.** No datagram listener, no `dgram`/`Bun.udpSocket`/`Deno.listenDatagram`
  binding, no preset that exposes one. The handler model is one HTTP request →
  one response, or a WS peer.
- **No HTTP/3 / QUIC.** QUIC is UDP-based and gated behind Node's
  `--experimental-quic`. Not a Nitro preset. Options are proxy termination
  (nginx, Caddy, Cloudflare edge) forwarding HTTP/1.1 or /2 to the Nitro
  process, or a custom preset over `node:quic`.
- **No WebRTC.** No ICE/STUN/TURN, no DTLS-SRTP, no SCTP data channels. Nitro
  can serve *signaling* routes (plain HTTP); the peer connection itself must
  come from a separate stack (`werift`, `node-webrtc`, mediasoup, an SFU).

### Two ways UDP can appear despite the above

1. **Platform edges.** Cloudflare and Vercel may serve HTTP/3 to clients while
   the Nitro code still sees an ordinary `Request`. Terminal protocol at the
   edge is not the framework's transport.
2. **Raw sockets in-process.** `node:dgram` can be opened inside the Node
   process as a side channel — outside Nitro's abstraction, and unavailable on
   serverless/edge presets where no persistent process exists.

### Relevance to eve

eve's durable session stream is **NDJSON over plain HTTP**
(`GET /eve/v1/session/<sessionId>/stream`, one JSON event per line, with
`startIndex` cursors, `x-eve-stream-tail-index`, and `x-eve-stream-version`
negotiation). It never opts into Nitro's WebSocket feature. The whole agent
protocol — messages, controls, streams, cancellation, health — is TCP HTTP.

---

## 4. Residual uncertainty

- Whether Vercel's Build Output lands `eve/__server.func` as a classic
  serverless function or as a Vercel *service* runtime was not settled from the
  source. The docs consistently say "service" (`docs/guides/deployment/vercel.mdx`:
  "Vercel runs the web service, workflows, sandboxes, schedules"); the CI
  workflow's comment ("the deployed function could not resolve a team-scoped
  template") says "function". Both are consistent with Build Output v3
  `functions/` entries deployed under the experimental services model. Marked
  as unresolved rather than inferred.
- Nitro's full preset list was not enumerated. A preset for an exotic platform
  could in principle expose a non-HTTP listener; nothing in the presets
  consulted suggested one.
- Eve is in beta/preview, subject to Vercel beta terms; APIs and behavior may
  change before GA. Treat artifact names and paths as point-in-time.

---

## Provenance

| Source | Revision / date |
|---|---|
| `vercel/eve` (`main`) | `29af29d5ebe69ca74b0eb76191bf464d8d7ea56c`, committed 2026-09-17T02:15:37Z |
| eve docs read | `README.md`, `docs/README.md`, `docs/concepts/project-structure.mdx`, `docs/concepts/sessions-runs-and-streaming.md`, `docs/guides/deployment/{overview.md,vercel.mdx,self-hosting.md}` |
| eve source read | `packages/eve/README.md`, `package.json`, `Dockerfile`, `.github/workflows/e2e-vercel.yml`, `src/cli/commands/build.ts`, `src/cli/vercel-service-output.ts`, `src/internal/nitro/host/{build-application.ts,vercel-build-output-config.ts}`, `src/internal/workflow-bundle/{eve-service-route-output.ts,vercel-workflow-output.ts}`, `src/internal/vercel/vercel-services-config.ts` |
| Nitro docs read | `nitro.build/docs`, `nitro.build/docs/websocket`, `nitro.build/deploy/runtimes/node` |
| Nitro version in scope | `3.0.260903-beta` (eve's pinned dependency) |

Not verified against source: Nitro's HTTP/3 / QUIC stance rests on its published
docs and preset pages only; the `nitrojs/nitro` source tree was not read.
