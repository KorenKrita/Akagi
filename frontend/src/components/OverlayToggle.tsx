import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { PictureInPicture2 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { toast } from '@/components/ui/sonner'
import { useConfigStore } from '@/stores/configStore'
import { invoke } from '@/lib/tauri'

// Show/hide the always-on-top suggestion overlay, from the Game page toolbar —
// the one screen you're on when you realise you want it (or want it gone).
//
// It drives the same persisted `overlay.enabled` flag as the Settings card, via
// `set_overlay_enabled`. The backend broadcasts `overlay-config` afterwards, so
// this button and the Settings card stay in sync. The overlay's own × button
// only closes its window for the current session.
export function OverlayToggle() {
  const { t } = useTranslation()
  const overlay = useConfigStore((s) => s.config?.overlay)
  const setOverlay = useConfigStore((s) => s.setOverlay)
  const [busy, setBusy] = useState(false)

  if (!overlay) return null

  const toggle = async () => {
    const next = !overlay.enabled
    setBusy(true)
    // Optimistic: the window takes a moment to appear and a toolbar button that
    // does nothing for half a second reads as broken. `overlay-config` confirms.
    setOverlay({ ...overlay, enabled: next })
    try {
      await invoke('set_overlay_enabled', { enabled: next })
    } catch (e) {
      setOverlay({ ...overlay, enabled: !next })
      toast.error(t('game.overlay_toggle_failed'), { description: String(e) })
    } finally {
      setBusy(false)
    }
  }

  // The label stays put and the variant carries the state — a button whose text
  // flips between "Show" and "Hide" makes you read it before every click.
  const action = overlay.enabled ? t('game.overlay_hide') : t('game.overlay_show')

  return (
    <Button
      variant={overlay.enabled ? 'secondary' : 'ghost'}
      size="sm"
      onClick={toggle}
      disabled={busy}
      className="text-xs"
      title={action}
      aria-label={action}
      aria-pressed={overlay.enabled}
    >
      <PictureInPicture2 className="size-4" />
      {t('game.overlay')}
    </Button>
  )
}
