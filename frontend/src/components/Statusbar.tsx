import { useTranslation } from 'react-i18next'
import { useConfigStore } from '@/stores/configStore'
import { useApiStatusStore } from '@/stores/apiStatusStore'

// Reserved built-in bot names (see `src/bot/native.rs`).
const NATIVE_4P = 'akagi-native'
const NATIVE_3P = 'akagi-native3p'

export function Statusbar() {
  const { t } = useTranslation()
  const config = useConfigStore((s) => s.config)
  const degraded = useApiStatusStore((s) => s.degraded)
  const apiError = useApiStatusStore((s) => s.error)

  // "Using online API" = a native bot is active AND the cloud API is fully
  // configured (enabled + URL + key). Config-derived so it reflects intent even
  // between games; health (degraded) comes from the live backend notifications.
  const api = config?.bot.api
  const nativeActive =
    config?.bot.active_4p === NATIVE_4P ||
    config?.bot.active_4p === NATIVE_3P ||
    config?.bot.active_3p === NATIVE_4P ||
    config?.bot.active_3p === NATIVE_3P
  const usingApi =
    !!api &&
    api.enabled &&
    api.base_url.trim() !== '' &&
    api.key.trim() !== '' &&
    nativeActive

  return (
    <footer className="flex items-center justify-between border-t border-border px-4 py-1.5 text-xs text-muted-foreground bg-muted/30">
      <span className="flex items-center gap-1.5">
        <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
        <span>{t('status.connected')}</span>
      </span>
      <span className="flex items-center gap-3">
        {usingApi && (
          <span
            className="flex items-center gap-1.5"
            title={
              degraded
                ? apiError ?? t('status.api_degraded_hint')
                : t('status.api_active_hint')
            }
          >
            <span
              className={`h-1.5 w-1.5 rounded-full ${degraded ? 'bg-red-400' : 'bg-emerald-400'}`}
            />
            {degraded ? t('status.api_degraded') : t('status.api_active')}
          </span>
        )}
        <span className="flex items-center gap-1.5">
          <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
          {t('status.events_live')}
        </span>
        <span className="flex items-center gap-1.5">
          <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
          {t('status.analysis_live')}
        </span>
      </span>
    </footer>
  )
}
