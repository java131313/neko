import { TerminalIcon } from '@phosphor-icons/react'
import type { Plugin } from '@/plugins/types'
import { Webshell } from './Webshell'

// a terminal inside the container: neko-webshell bridges WebSocket <-> PTY
// (one shell per connection, running as the container user; `pi` is on PATH)
export const webshell: Plugin = {
  id: 'webshell',
  ns: 'shell',
  tab: { id: 'shell', icon: TerminalIcon, label: 'shell:tab', component: Webshell },
}
