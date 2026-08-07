import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { invoke } from '@/lib/tauri'
import { RefreshCw } from 'lucide-react'
import { useConfigStore } from '@/stores/configStore'
import { useApiStatusStore } from '@/stores/apiStatusStore'
import { useCaptureStore } from '@/stores/captureStore'
import { useBotStore } from '@/stores/botStore'
import {
  BOT_LABEL,
  botTone,
  CAPTURE_LABEL,
  captureTone,
  TONE_DOT,
  type IndicatorTone,
} from '@/lib/statusIndicators'

// Reserved built-in bot names (see `src/bot/native.rs`).
const NATIVE_4P = 'akagi-native'
const NATIVE_3P = 'akagi-native3p'

function IndicatorDot({
  tone,
  label,
  title,
}: {
  tone: IndicatorTone
  label: string
  title?: string
}) {
  return (
    <span className="flex items-center gap-1.5" title={title}>
      <span className={`h-1.5 w-1.5 rounded-full ${TONE_DOT[tone]}`} />
      {label}
    </span>
  )
}

export function Statusbar() {
  const { t } = useTranslation()
  const config = useConfigStore((s) => s.config)
  const degraded = useApiStatusStore((s) => s.degraded)
  const apiError = useApiStatusStore((s) => s.error)
  const lastAttemptAt = useApiStatusStore((s) => s.lastAttemptAt)
  const retrying = useApiStatusStore((s) => s.retrying)
  const capture = useCaptureStore((s) => s.status)
  const bot = useBotStore((s) => s.status)
  const [, refreshElapsed] = useState(0)

  useEffect(() => {
    if (!degraded || !lastAttemptAt) return
    const timer = window.setInterval(() => refreshElapsed((value) => value + 1), 1_000)
    return () => window.clearInterval(timer)
  }, [degraded, lastAttemptAt])

  // Capture LED — "is Akagi reading the game?" (proxy / capture transport).
  // Tooltip surfaces the capture descriptor, or the error message on failure,
  // so the user can see *why* it's down.
  const capTone = captureTone(capture.state)
  const captureTitle =
    capture.state === 'error'
      ? capture.error
      : capture.state === 'running' || capture.state === 'starting'
        ? capture.descriptor
        : undefined

  // Bot LED — "is the recommendation engine up?". Independent of capture: the
  // bot can error (broken env, failed spawn) while capture keeps running.
  const botT = botTone(bot.state)
  const botTitle =
    bot.state === 'error'
      ? bot.error
      : bot.state === 'loading'
        ? `${bot.bot} · ${t(`status.stage_${bot.stage}`)}`
        : bot.state === 'ready' || bot.state === 'stopped'
          ? bot.bot
          : undefined

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

  const elapsedSeconds = lastAttemptAt
    ? Math.max(0, Math.floor((Date.now() - lastAttemptAt) / 1_000))
    : undefined
  const elapsedLabel = elapsedSeconds === undefined
    ? undefined
    : elapsedSeconds < 60
      ? t('status.api_last_attempt_seconds', { count: elapsedSeconds })
      : t('status.api_last_attempt_minutes', { count: Math.floor(elapsedSeconds / 60) })

  const retryApi = async () => {
    const store = useApiStatusStore.getState()
    if (store.retrying) return
    store.setRetrying(true)
    try {
      await invoke('retry_native_api')
    } catch {
      // The backend emits the classified failure notification used by both the
      // toast and status indicator.
    } finally {
      useApiStatusStore.getState().setRetrying(false)
    }
  }

  return (
    <footer className="flex items-center justify-between border-t border-border px-4 py-1.5 text-xs text-muted-foreground bg-muted/30">
      <IndicatorDot
        tone={capTone}
        label={t(CAPTURE_LABEL[capTone])}
        title={captureTitle}
      />
      <span className="flex items-center gap-3">
        {usingApi && (
          <span className="flex items-center gap-1.5">
            <IndicatorDot
              tone={degraded ? 'fault' : 'live'}
              label={degraded ? t('status.api_degraded') : t('status.api_active')}
              title={
                degraded
                  ? apiError ?? t('status.api_degraded_hint')
                  : t('status.api_active_hint')
              }
            />
            {degraded && (
              <>
                {elapsedLabel && <span className="text-muted-foreground/80">· {elapsedLabel}</span>}
                <button
                  type="button"
                  className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 hover:bg-muted disabled:opacity-50"
                  onClick={() => void retryApi()}
                  disabled={retrying}
                  title={t('status.api_retry_hint')}
                >
                  <RefreshCw className={`h-3 w-3 ${retrying ? 'animate-spin' : ''}`} />
                  {retrying ? t('status.api_retrying') : t('status.api_retry')}
                </button>
              </>
            )}
          </span>
        )}
        <IndicatorDot
          tone={botT}
          label={t(BOT_LABEL[bot.state])}
          title={botTitle}
        />
      </span>
    </footer>
  )
}
