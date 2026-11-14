// FallbackInput: the transport's own channel while it is ready, the
// websocket otherwise (data channel still handshaking / ICE blocked).
import { test } from 'vitest'
import assert from 'node:assert/strict'
import type { InputChannel } from '../transport.ts'

const { FallbackInput } = await import('../index.ts')

const recorder = (name: string) => {
  const calls: string[] = []
  const channel: InputChannel & { ready(): boolean } = {
    ready: () => true,
    move: (x, y) => calls.push(`${name}.move ${x} ${y}`),
    scroll: (x, y, c) => calls.push(`${name}.scroll ${x} ${y} ${c}`),
    button: (c, d) => calls.push(`${name}.button ${c} ${d}`),
    key: (k, d) => calls.push(`${name}.key ${k} ${d}`),
    touch: (p, id, x, y, pr) => calls.push(`${name}.touch ${p} ${id} ${x} ${y} ${pr}`),
  }
  return { calls, channel }
}

test('ready primary carries everything; unready primary falls back per call', () => {
  const a = recorder('dc')
  const b = recorder('ws')
  const input = new FallbackInput(a.channel, b.channel)

  // primary ready → everything on the primary
  input.key(0x61, true)
  input.button(1, false)
  assert.deepEqual(a.calls, ['dc.key 97 true', 'dc.button 1 false'])
  assert.deepEqual(b.calls, [])

  // primary goes down → subsequent calls land on the fallback
  a.channel.ready = () => false
  input.key(0x61, false)
  input.move(10, 20)
  input.scroll(1, -1, true)
  input.touch('begin', 3, 5, 7, 9)
  assert.deepEqual(a.calls, ['dc.key 97 true', 'dc.button 1 false'])
  assert.deepEqual(b.calls, ['ws.key 97 false', 'ws.move 10 20', 'ws.scroll 1 -1 true', 'ws.touch begin 3 5 7 9'])

  // primary recovers → back to the primary
  a.channel.ready = () => true
  input.key(0xff0d, true)
  assert.deepEqual(a.calls.at(-1), 'dc.key 65293 true')
  assert.equal(b.calls.length, 4)
})

test('a primary without ready() is always used (backwards compatible)', () => {
  const calls: string[] = []
  const legacy: InputChannel = {
    key: (k, d) => calls.push(`legacy.key ${k} ${d}`),
    move: () => {},
    scroll: () => {},
    button: () => {},
    touch: () => {},
  }
  const b = recorder('ws')
  const input = new FallbackInput(legacy, b.channel)
  input.key(1, true)
  assert.deepEqual(calls, ['legacy.key 1 true'])
  assert.deepEqual(b.calls, [])
})
