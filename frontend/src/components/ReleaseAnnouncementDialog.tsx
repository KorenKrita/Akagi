import { useEffect, useRef } from 'react'
import { useTranslation } from 'react-i18next'

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { releaseSlug, type ReleaseEntry } from '@/announcements/releases'
import { useAnnouncementStore } from '@/stores/announcementStore'
import { AKAGI_GITHUB_URL, openExternal } from '@/lib/external'
import { getAppVersion } from '@/lib/appVersion'

// Delay so the What's-new doesn't pop over the very first paint. Shorter
// than the AkagiMS promo delay: this dialog decides first and the promo
// checks `launchShowPending` before showing, so the two never stack.
const OPEN_DELAY_MS = 1500

/**
 * "What's new" dialog. Two ways in:
 *  - launch: after an update (or on a fresh install) it lists every
 *    bundled release announcement the user hasn't seen yet — skip-level
 *    updates replay all the versions in between. Any close records the
 *    running version as seen, so it shows once per update.
 *  - history: Settings → Updates → "What's new" re-opens the full list.
 */
export function ReleaseAnnouncementDialog() {
  const { t } = useTranslation()
  const open = useAnnouncementStore((s) => s.open)
  const entries = useAnnouncementStore((s) => s.entries)
  const close = useAnnouncementStore((s) => s.close)
  // One-shot per app launch (the component remounts on route changes but
  // the store's armed/seen state is global, so the ref is just belt and
  // braces against double-arming the timer under StrictMode).
  const fired = useRef(false)

  useEffect(() => {
    if (fired.current) return
    fired.current = true
    let timer: number | undefined
    let cancelled = false
    getAppVersion().then((version) => {
      if (cancelled) return
      if (!useAnnouncementStore.getState().prepareLaunch(version)) return
      timer = window.setTimeout(() => {
        useAnnouncementStore.getState().showLaunch()
      }, OPEN_DELAY_MS)
    })
    return () => {
      cancelled = true
      window.clearTimeout(timer)
    }
  }, [])

  return (
    <Dialog open={open} onOpenChange={(v) => !v && close()}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t('announcements.whats_new.title')}</DialogTitle>
          <DialogDescription>{t('announcements.whats_new.intro')}</DialogDescription>
        </DialogHeader>

        <div className="grid gap-5 max-h-[60vh] overflow-y-auto pr-1">
          {entries.map((entry) => (
            <ReleaseSection key={entry.version} entry={entry} />
          ))}
        </div>

        <DialogFooter className="bg-transparent p-0 border-0 mx-0 mb-0 flex-wrap gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => openExternal(`${AKAGI_GITHUB_URL}/releases`)}
          >
            {t('announcements.whats_new.all_releases')}
          </Button>
          <Button size="sm" onClick={close}>
            {t('announcements.whats_new.got_it')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function ReleaseSection({ entry }: { entry: ReleaseEntry }) {
  const { t, i18n } = useTranslation()
  const slug = releaseSlug(entry.version)
  // The ISO date renders in the viewer's locale; parse as local midnight
  // (plain YYYY-MM-DD strings would otherwise be treated as UTC and can
  // display one day off west of Greenwich).
  const date = new Date(`${entry.date}T00:00:00`).toLocaleDateString(i18n.language)
  return (
    <section className="grid gap-3">
      <h3 className="flex items-baseline gap-2 border-b border-border pb-1.5">
        <span className="font-mono text-sm font-semibold">v{entry.version}</span>
        <span className="text-xs text-muted-foreground">{date}</span>
      </h3>
      <ul className="grid gap-3 sm:grid-cols-2">
        {entry.features.map((f) => {
          const Icon = f.icon
          return (
            <li key={f.key} className="flex items-start gap-3">
              <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-md bg-muted text-foreground">
                <Icon className="size-4" />
              </span>
              <div className="min-w-0">
                <div className="text-sm font-medium">
                  {t(`announcements.releases.${slug}.${f.key}_title`)}
                </div>
                <div className="text-xs text-muted-foreground">
                  {t(`announcements.releases.${slug}.${f.key}_desc`)}
                </div>
              </div>
            </li>
          )
        })}
      </ul>
    </section>
  )
}
