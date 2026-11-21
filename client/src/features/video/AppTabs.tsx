import { useStore } from 'zustand'
import { useNeko } from '@/state/provider'
import { cn } from '@/lib/utils'

// the neko-rs apptab switcher: multiple apps share one X display, the chosen
// one is raised to the front server-side and the broadcast updates everyone
export function AppTabs() {
  const { client, app } = useNeko()
  const connected = useStore(client.store, (s) => s.connection.status === 'connected')
  const apps = useStore(app, (s) => s.apps)
  const active = useStore(app, (s) => s.activeApp)
  if (!connected || apps.length < 2) return null

  return (
    <div className="absolute left-2 top-2 z-20 flex gap-0.5 rounded-[3px] bg-black/40 p-0.5" data-testid="app-tabs">
      {apps.map((id) => (
        <button
          key={id}
          type="button"
          onClick={() => client.send('control/apptab', { app: id })}
          className={cn(
            'cursor-pointer rounded-[3px] px-2.5 py-0.5 text-[12px] font-semibold capitalize',
            id === active
              ? 'bg-white/90 text-black'
              : 'bg-white/10 text-white/60 hover:bg-white/20 hover:text-white/90',
          )}
          data-testid={`app-tab-${id}`}
        >
          {id}
        </button>
      ))}
    </div>
  )
}
