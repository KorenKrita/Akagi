import { useEffect, useRef, useState, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { AppWindow, Bot, Gamepad2, Zap } from 'lucide-react'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { AKAGIMS_DOWNLOAD_URL, openExternal } from '@/lib/external'
import { MAX_AKAGIMS_ANNOUNCEMENT_SHOWS, useUiPrefsStore } from '@/stores/uiPrefsStore'
import screenshot from '@/assets/akagims-fullauto.jpg'

// Delay so the announcement doesn't pop over the very first paint; keeps it
// clear of the UpdateNotifier toast (3s) which stacks fine next to a dialog.
const OPEN_DELAY_MS = 1500

// Release announcement for AkagiMS. Closing it distinguishes intent:
// "Got it" dismisses for good, while X / Esc / outside-click only ends this
// showing — the dialog comes back on a later launch until it has been shown
// MAX_AKAGIMS_ANNOUNCEMENT_SHOWS times in total. A quieter permanent entry
// point lives in the sidebar footer and the Overview promo card.
export function AkagiMsAnnouncementDialog() {
  const { t } = useTranslation()
  const dismissed = useUiPrefsStore((s) => s.akagimsAnnouncementDismissed)
  const shows = useUiPrefsStore((s) => s.akagimsAnnouncementShows)
  const markDismissed = useUiPrefsStore((s) => s.markAkagimsAnnouncementDismissed)
  const recordShown = useUiPrefsStore((s) => s.recordAkagimsAnnouncementShown)
  const [open, setOpen] = useState(false)
  // One-shot per app launch: recordShown bumps `shows`, which re-runs the
  // effect — without the ref that re-run would arm a second timer and count
  // the same launch twice.
  const fired = useRef(false)

  useEffect(() => {
    if (fired.current) return
    if (dismissed || shows >= MAX_AKAGIMS_ANNOUNCEMENT_SHOWS) return
    fired.current = true
    const timer = setTimeout(() => {
      setOpen(true)
      recordShown()
    }, OPEN_DELAY_MS)
    return () => clearTimeout(timer)
  }, [dismissed, shows, recordShown])

  const handleGotIt = () => {
    markDismissed()
    setOpen(false)
  }

  return (
    <Dialog open={open} onOpenChange={(v) => setOpen(v)}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t('announcements.akagims.title')}</DialogTitle>
          <DialogDescription>{t('announcements.akagims.intro')}</DialogDescription>
        </DialogHeader>

        <div className="grid gap-4 max-h-[60vh] overflow-y-auto">
          <img
            src={screenshot}
            alt={t('announcements.akagims.screenshot_alt')}
            className="w-full rounded-md border border-border"
          />
          <ul className="grid gap-3 sm:grid-cols-2">
            <FeatureRow
              icon={<Gamepad2 className="size-4" />}
              title={t('announcements.akagims.feature_majsoul_title')}
              desc={t('announcements.akagims.feature_majsoul_desc')}
            />
            <FeatureRow
              icon={<AppWindow className="size-4" />}
              title={t('announcements.akagims.feature_embedded_title')}
              desc={t('announcements.akagims.feature_embedded_desc')}
            />
            <FeatureRow
              icon={<Bot className="size-4" />}
              title={t('announcements.akagims.feature_fullauto_title')}
              desc={t('announcements.akagims.feature_fullauto_desc')}
            />
            <FeatureRow
              icon={<Zap className="size-4" />}
              title={t('announcements.akagims.feature_zero_setup_title')}
              desc={t('announcements.akagims.feature_zero_setup_desc')}
            />
          </ul>
        </div>

        <DialogFooter className="bg-transparent p-0 border-0 mx-0 mb-0 flex-wrap gap-2">
          <Button variant="secondary" size="sm" onClick={handleGotIt}>
            {t('announcements.akagims.got_it')}
          </Button>
          <Button size="sm" onClick={() => openExternal(AKAGIMS_DOWNLOAD_URL)}>
            {t('announcements.akagims.view_download')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function FeatureRow({ icon, title, desc }: { icon: ReactNode; title: string; desc: string }) {
  return (
    <li className="flex items-start gap-3">
      <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-md bg-muted text-foreground">
        {icon}
      </span>
      <div className="min-w-0">
        <div className="text-sm font-medium">{title}</div>
        <div className="text-xs text-muted-foreground">{desc}</div>
      </div>
    </li>
  )
}
