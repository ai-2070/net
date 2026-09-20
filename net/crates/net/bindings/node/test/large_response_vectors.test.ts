// SPDX-License-Identifier: MIT OR Apache-2.0
// Independent byte-layout check, not a native-mesh interoperability test.
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { expect, it } from 'vitest'

it('pins the shared unary response fragment layout', () => {
  const fixture = JSON.parse(readFileSync(resolve(__dirname,
    '../../../tests/cross_lang_nrpc/golden_vectors_large_response.json'), 'utf8'))
  expect(fixture.request_flag).toBe(1 << 6)
  expect(fixture.header_name).toBe('nrpc-response-fragment-v1')
  expect(fixture.max_response_bytes / fixture.chunk_bytes).toBe(fixture.max_fragments)
  const encoded = Buffer.alloc(7 + fixture.response.body_length, fixture.response.body_byte)
  encoded.writeUInt16LE(fixture.response.status, 0)
  encoded.writeUInt8(0, 2)
  encoded.writeUInt32LE(fixture.response.body_length, 3)
  expect(encoded.length).toBe(fixture.encoded_response_bytes)
  expect(fixture.fragments.length).toBe(Math.ceil(encoded.length / fixture.chunk_bytes))
  const assembled: Buffer[] = []
  for (const piece of fixture.fragments) {
    const header = Buffer.alloc(6)
    header.writeUInt32LE(encoded.length, 0)
    header.writeUInt16LE(piece.index, 4)
    expect(header.toString('hex')).toBe(piece.header_value_hex)
    const body = encoded.subarray(piece.index * fixture.chunk_bytes,
      (piece.index + 1) * fixture.chunk_bytes)
    expect(body.length).toBe(piece.body_bytes)
    assembled.push(body)
  }
  expect(Buffer.concat(assembled)).toEqual(encoded)
})
