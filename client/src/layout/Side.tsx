import { useEffect, useRef, useState } from 'react'
import { useStore } from 'zustand'
import { useTranslation } from 'react-i18next'
import { SlidersHorizontalIcon, ArrowsOutIcon, ArrowsInIcon } from '@phosphor-icons/react'
import { useApp } from '@/state/provider'
import { get, remember } from '@/state/storage'
import { Tabs, TabsList, TabsTrigger, TabsContent } from '@/components/ui/tabs'
import { IconButton } from '@/components/IconButton'
import { Settings } from '@/features/settings/Settings'
import { useSlot } from '@/plugins'
import type { PluginTab } from '@/plugins/types'
import { EachHook } from '@/components/EachHook'

// after the plugins' tabs
const settingsTab: PluginTab = {
  id: 'settings',
  icon: SlidersHorizontalIcon,
  label: 'side.settings',
  component: Settings,
}
const always = () => true

// side panel drag-resize limits (px)
const MIN_WIDTH = 280
const MAX_WIDTH = 1200

export function Side() {
  const tabs = [...useSlot('side.tab'), settingsTab]
  useEffect(() => {
    document.querySelector('aside')?.scrollIntoView({ behavior: 'smooth', block: 'start' })
  }, [])

  return (
    <EachHook items={tabs} use={(tab) => tab.useVisible ?? always}>
      {(visible) => <Panel tabs={tabs.filter((_, i) => visible[i])} />}
    </EachHook>
  )
}

// the tabs available now
function Panel({ tabs }: { tabs: PluginTab[] }) {
  const { t } = useTranslation()
  const app = useApp()
  const wanted = useStore(app, (s) => s.tab)
  // a remembered tab that is not available now falls back to the first one
  const tab = tabs.some((x) => x.id === wanted) ? wanted : tabs[0].id
  const Page = tabs.find((x) => x.id === tab)!.component

  // remembered width in px (0 = the default w-side); fullscreen drops the
  // panel out of the layout over the video stage instead
  const [width, setWidth] = useState<number>(() => get('side.width', 0))
  const [full, setFull] = useState(false)
  const aside = useRef<HTMLElement>(null)
  const drag = useRef<{ x: number; w: number } | null>(null)

  // ESC leaves fullscreen, like the video's
  useEffect(() => {
    if (!full) return
    const esc = (e: KeyboardEvent) => e.key === 'Escape' && setFull(false)
    window.addEventListener('keydown', esc)
    return () => window.removeEventListener('keydown', esc)
  }, [full])

  const startDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    drag.current = { x: e.clientX, w: aside.current?.offsetWidth ?? 0 }
    try {
      e.currentTarget.setPointerCapture(e.pointerId)
    } catch {
      // synthetic events in tests carry no active pointer
    }
  }
  const moveDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return
    const w = Math.round(Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, drag.current.w + (drag.current.x - e.clientX))))
    setWidth(w)
    remember('side.width', w)
  }
  const stopDrag = () => {
    drag.current = null
  }

  return (
    <Tabs value={tab} onValueChange={(id) => (app.setState({ tab: id }), remember('tab', id))} asChild>
      <aside
        ref={aside}
        className={
          full
            ? 'fixed inset-0 z-50 flex h-full w-full flex-col bg-background-primary'
            : 'tablet:w-full tablet:portrait:h-[60vh] tablet:landscape:h-screen relative flex max-h-full w-side max-w-full shrink-0 flex-col bg-background-primary'
        }
        style={!full && width ? { width } : undefined}
        data-testid="side"
      >
        {!full && (
          // the drag handle on the edge facing the video stage
          <div
            className="tablet:hidden absolute inset-y-0 left-0 z-10 w-2 -translate-x-1/2 cursor-col-resize touch-none hover:bg-style-primary/50 active:bg-style-primary/70"
            onPointerDown={startDrag}
            onPointerMove={moveDrag}
            onPointerUp={stopDrag}
            data-testid="side-resize"
          />
        )}
        <IconButton
          className="absolute right-1.5 top-1.5 z-10"
          label={t(full ? 'shell:windowed' : 'shell:fullscreen')}
          onClick={() => setFull((f) => !f)}
          data-testid="side-fullscreen"
        >
          {full ? <ArrowsInIcon className="size-3" /> : <ArrowsOutIcon className="size-3" />}
        </IconButton>
        <TabsList data-testid="tabs">
          {tabs.map(({ id, icon: Icon, label }) => (
            <TabsTrigger key={id} value={id}>
              <Icon className="mr-1 inline size-2.5" />
              <span>{t(label)}</span>
            </TabsTrigger>
          ))}
        </TabsList>
        <TabsContent value={tab} className="flex max-h-full grow overflow-auto pt-[5px]">
          <Page />
        </TabsContent>
      </aside>
    </Tabs>
  )
}
