// Input multiplexer: prefer the transport's own channel (the WebRTC data
// channel) and fall back to the websocket while it is not ready. Without
// this, a dead data channel — ICE blocked, SCTP still handshaking — meant
// every keystroke and click was silently dropped (DataChannelInput.send
// returns early when the channel is not open), the classic symptom being
// "keyboard does nothing but the video plays".

import type { InputChannel } from '../transport.ts'

export class FallbackInput implements InputChannel {
  private readonly primary: InputChannel
  private readonly fallback: InputChannel

  constructor(primary: InputChannel, fallback: InputChannel) {
    this.primary = primary
    this.fallback = fallback
  }

  private channel(): InputChannel {
    return this.primary.ready ? (this.primary.ready() ? this.primary : this.fallback) : this.primary
  }

  move(x: number, y: number) {
    this.channel().move(x, y)
  }

  scroll(deltaX: number, deltaY: number, controlKey: boolean) {
    this.channel().scroll(deltaX, deltaY, controlKey)
  }

  button(code: number, down: boolean) {
    this.channel().button(code, down)
  }

  key(keysym: number, down: boolean) {
    this.channel().key(keysym, down)
  }

  touch(phase: 'begin' | 'update' | 'end', id: number, x: number, y: number, pressure: number) {
    this.channel().touch(phase, id, x, y, pressure)
  }
}
