/**
 * Consumer-compile probe for paid A2A (NODE_A2A_PAID_ADMISSION_PLAN.md WS-E):
 * the documented `@net-mesh/core` route — `NetMesh`, `PaymentProvider`,
 * `CapabilityGateway`, the lossless readers, and the typed errors after
 * `classifyError` — written as a real consumer would, against the generated
 * native declarations with `skipLibCheck: false`. Compiled, never executed;
 * the live behaviour is `test/a2a_paid.test.ts`.
 *
 * Everything is exported so the probe reads as real consumer code and no
 * linter drops an "unused" pin.
 */

import {
  a2aDocument,
  a2aU64,
  CapabilityGateway,
  NetMesh,
  PaymentProvider,
} from '../../index'
import type {
  A2aPreflightArgs,
  A2aServeHandle,
  A2aServicePolicyJs,
  ServeA2aConfiguredOptions,
  TaskBriefJs,
} from '../../index'
import {
  A2aInvalidArgumentError,
  classifyError,
  JournalOwnedElsewhereError,
  PaymentRefusedError,
} from '../../errors'

export const summarize: A2aServicePolicyJs = {
  revision: 'r1',
  pricingTerms: undefined,
  bounds: {
    maxPromptBytes: 1024n,
    maxContextRefs: 8n,
    maxTags: 8n,
    maxTagBytes: 64n,
    maxInFlight: 4n,
  },
  reservationTtlSecs: 600n,
  reservationRetentionSecs: 604800n,
  retentionSecs: 3600n,
  description: 'summarize a document',
}

export const options: ServeA2aConfiguredOptions = { principal: 'session_peer', handlerTimeoutMs: 0 }

export async function serveProvider(mesh: NetMesh, statePath: string): Promise<A2aServeHandle> {
  const provider = new PaymentProvider(mesh, statePath, undefined, 'https://facilitator.example.com')
  const terms: string = await provider.pricingTerms(`${mesh.nodeId()}/net.a2a.task/summarize`, '[]')
  const handle = await provider.serveA2aConfigured(
    async (brief: TaskBriefJs): Promise<string> => `blob://${brief.service ?? 'free'}/${brief.taskId}`,
    { summarize: { ...summarize, pricingTerms: terms } },
    'state/a2a-journal.json',
    options,
    async (args: A2aPreflightArgs): Promise<string | null> =>
      args.briefJson.length > 0 ? null : 'empty brief',
  )
  const queue: string = await provider.a2aUnresolved()
  if (queue !== '[]') {
    await provider.a2aResolve(
      a2aDocument(queue, '/0/owner'),
      'task-1',
      '{"state":"failed","error":"refunded"}',
      a2aU64(queue, '/0/generation'),
    )
  }
  return handle
}

export async function buy(mesh: NetMesh, providerNode: bigint): Promise<string> {
  const gateway = new CapabilityGateway(
    mesh,
    undefined,
    'state/spend-policy.json',
    'production',
    undefined,
    undefined,
    undefined,
    undefined,
    undefined,
    undefined,
    undefined,
    'state/a2a-purchases.json',
  )
  const env: string = await gateway.prepareTask(providerNode, 'summarize', 'the filings', [], [], 'task-1')
  const prepared: string = a2aDocument(env, '/prepared')
  const expires: bigint = a2aU64(env, '/quote/expires_at_ns')
  void expires
  const bought: string = await gateway.purchaseTask(prepared)
  const sent: string = await gateway.submitTask(prepared)
  const rows: string = await gateway.a2aAttempts()
  await gateway.a2aResolveAttempt(
    'task-1',
    '{"resolution":"closed","outcome":"refunded"}',
    a2aU64(rows, '/0/key/provider_node'),
  )
  // A raw resubmission, and the refusal it can produce, classified.
  try {
    return await mesh.submitTaskPaid(prepared, a2aDocument(bought, '/proof'))
  } catch (e) {
    const typed = classifyError(e)
    if (typed instanceof PaymentRefusedError) return typed.schematic ?? typed.message
    if (typed instanceof JournalOwnedElsewhereError || typed instanceof A2aInvalidArgumentError) {
      throw typed
    }
    return sent
  } finally {
    gateway.close()
  }
}

export async function describe(mesh: NetMesh, providerNode: bigint): Promise<string> {
  mesh.setA2aOrgCaller(null)
  return mesh.describeA2a(providerNode)
}
