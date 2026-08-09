import { useEffect, useState, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { AppWindow, Bot, Gamepad2, Zap } from 'lucide-react'
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { AKAGIMS_GITHUB_URL, openExternal } from '@/lib/external'
import { useUiPrefsStore } from '@/stores/uiPrefsStore'
import screenshot from '@/assets/akagims-fullauto.jpg'

// Delay so the announcement doesn't pop over the very first paint; keeps it
// clear of the UpdateNotifier toast (3s) which stacks fine next to a dialog.
const OPEN_DELAY_MS = 1500

// One-time release announcement for AkagiMS, shown on the first launch of a
// version that ships it. Dismissing it (any way the dialog closes) marks the
// `akagi.announcement.akagims` flag so it never reappears.
export function AkagiMsAnnouncementDialog() {
  const { t } = useTranslation()
  const seen = useUiPrefsStore((s) => s.akagimsAnnouncementSeen)
  const markSeen = useUiPrefsStore((s) => s.markAkagimsAnnouncementSeen)
  const [open, setOpen] = useState(false)

  useEffect(() => {
    if (seen) return
    const timer = setTimeout(() => setOpen(true), OPEN_DELAY_MS)
    return () => clearTimeout(timer)
  }, [seen])

  if (seen) return null

  const handleOpenChange = (v: boolean) => {
    setOpen(v)
    if (!v) markSeen()
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
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
          <DialogClose asChild>
            <Button variant="secondary" size="sm">
              {t('announcements.akagims.got_it')}
            </Button>
          </DialogClose>
          <Button size="sm" onClick={() => openExternal(AKAGIMS_GITHUB_URL)}>
            {t('announcements.akagims.view_github')}
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
