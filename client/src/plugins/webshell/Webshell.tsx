import { useEffect, useRef } from 'react'
import { t } from '@/i18n'

// the webshell port published by the container (-p 7681:7681); same host as the page
const url = () => {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:'
  return `${proto}//${location.hostname}:7681/ws`
}

export function Webshell() {
  const host = useRef<HTMLDivElement>(null)

  useEffect(() => {
    // xterm needs a browser: imported lazily so this module stays importable in the node tests
    let dead = false
    let teardown: (() => void) | undefined
    void Promise.all([import('@xterm/xterm'), import('@xterm/addon-fit'), import('@xterm/xterm/css/xterm.css')]).then(
      ([{ Terminal }, { FitAddon }]) => {
        if (dead || !host.current) return
        const term = new Terminal({
          cursorBlink: true,
          fontSize: 14,
          fontFamily: 'Menlo, Consolas, "DejaVu Sans Mono", monospace',
          theme: { background: '#101014', foreground: '#e6e6e6', cursor: '#ffffff' },
        })
        const fit = new FitAddon()
        term.loadAddon(fit)
        term.open(host.current)
        fit.fit()
        term.focus()

        const ws = new WebSocket(url())
        ws.binaryType = 'arraybuffer'
        ws.onmessage = (e) => {
          if (e.data instanceof ArrayBuffer) term.write(new Uint8Array(e.data))
        }
        ws.onclose = () => term.write(`\r\n\x1b[90m── ${t('shell:disconnected')} ──\x1b[0m\r\n`)

        // keyboard -> shell, terminal size -> PTY (also the fit on open)
        const data = term.onData((d) => ws.readyState === WebSocket.OPEN && ws.send(d))
        const resize = term.onResize(({ cols, rows }) => {
          if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ type: 'resize', cols, rows }))
        })
        const observer = new ResizeObserver(() => {
          try {
            fit.fit()
          } catch {
            // the element is hidden: nothing to fit to
          }
        })
        observer.observe(host.current)
        teardown = () => {
          observer.disconnect()
          data.dispose()
          resize.dispose()
          ws.close()
          term.dispose()
        }
      },
      () => {}, // the chunk failed to load: leave the empty panel
    )
    return () => {
      dead = true
      teardown?.()
    }
  }, [])

  return <div ref={host} className="h-full min-h-[40vh] w-full overflow-hidden p-1" />
}
