import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import {
  CheckCircle2,
  RefreshCw,
  Eye,
  EyeOff,
  Ticket,
  Activity,
  ShoppingCart,
  AlertTriangle,
  XCircle,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { invoke } from '@/lib/tauri'
import { toast } from '@/components/ui/sonner'
import { PurchaseDialog } from '@/components/PurchaseDialog'
import { usePurchaseStore, type PurchasePhase } from '@/stores/purchaseStore'
import type { KeyStatus, ModelInfo, NativeApiConfig, RedeemResponse } from '@/types'

/**
 * Controlled editor for the built-in bot's cloud-inference settings
 * (`bot.api`). Fully controlled via `value` / `onChange` so it can bind to the
 * config store (Bots page) or to a wizard draft (Setup) alike. Persistence of
 * the whole config is the caller's job (a Save button, or the wizard's Finish);
 * the only thing this component persists on its own is a freshly-minted key.
 *
 * Enabling the API queries the available models once and auto-fills any empty
 * 4p / 3p model slot with the first model the key may use for that mode.
 */
export function NativeApiFields({
  value,
  onChange,
  onKeyMinted,
}: {
  value: NativeApiConfig
  onChange: (api: NativeApiConfig) => void
  /**
   * Called with the full api config after a redeem mints a **new** key, so the
   * caller can persist it immediately (single-use code, key shown once). The
   * Bots page passes `persistApiConfig`; the Setup wizard omits this and lets
   * its Finish step persist — persisting mid-wizard would fight the wizard's
   * draft↔store re-sync.
   */
  onKeyMinted?: (api: NativeApiConfig) => void | Promise<void>
}) {
  const { t } = useTranslation()
  const [showKey, setShowKey] = useState(false)
  const [checking, setChecking] = useState(false)
  const [enabling, setEnabling] = useState(false)
  const [loadingModels, setLoadingModels] = useState(false)
  const [status, setStatus] = useState<KeyStatus | null>(null)
  const [models, setModels] = useState<ModelInfo[] | null>(null)
  const [redeemOpen, setRedeemOpen] = useState(false)
  const [buyOpen, setBuyOpen] = useState(false)
  const purchasePhase = usePurchaseStore((s) => s.phase)
  const [err, setErr] = useState<string | null>(null)

  const set = (patch: Partial<NativeApiConfig>) => onChange({ ...value, ...patch })
  const hasUrlKey = value.base_url.trim() !== '' && value.key.trim() !== ''

  const queryModels = async (v: NativeApiConfig): Promise<ModelInfo[]> => {
    const list = await invoke<ModelInfo[]>('native_api_models', {
      baseUrl: v.base_url,
      key: v.key,
    })
    setModels(list)
    return list
  }

  // Turning the API on: flip immediately for a responsive switch, then query
  // the models once and fill any empty 4p/3p slot with the first eligible model
  // for that mode. Never clobbers a model the user already chose.
  const toggleEnabled = async (on: boolean) => {
    const next = { ...value, enabled: on }
    if (!on || !(next.base_url.trim() && next.key.trim())) {
      onChange(next)
      return
    }
    onChange(next)
    setEnabling(true)
    setErr(null)
    try {
      const list = await queryModels(next)
      const first4p = list.find((m) => m.game === '4p')?.id
      const first3p = list.find((m) => m.game === '3p')?.id
      const filled = { ...next }
      if (!filled.model_4p && first4p) filled.model_4p = first4p
      if (!filled.model_3p && first3p) filled.model_3p = first3p
      if (filled.model_4p !== next.model_4p || filled.model_3p !== next.model_3p) {
        onChange(filled)
      }
    } catch (e) {
      setErr(String(e))
    } finally {
      setEnabling(false)
    }
  }

  const checkKey = async () => {
    if (!hasUrlKey) {
      setErr(t('bots.api.need_url_key'))
      return
    }
    setChecking(true)
    setErr(null)
    setStatus(null)
    try {
      setStatus(
        await invoke<KeyStatus>('native_api_key_status', {
          baseUrl: value.base_url,
          key: value.key,
        }),
      )
    } catch (e) {
      setErr(String(e))
    } finally {
      setChecking(false)
    }
  }

  const fetchModels = async () => {
    if (!hasUrlKey) {
      setErr(t('bots.api.need_url_key'))
      return
    }
    setLoadingModels(true)
    setErr(null)
    try {
      await queryModels(value)
    } catch (e) {
      setErr(String(e))
    } finally {
      setLoadingModels(false)
    }
  }

  // Shared sink for a freshly-minted key (redeemed code or purchase): the
  // code/key is single-use and shown once, so reflect it in the edited value
  // immediately and let the caller persist it (Bots page) before it can be
  // lost. The Setup wizard omits `onKeyMinted` and persists on Finish.
  const adoptNewKey = async (key: string) => {
    const nextApi = { ...value, key }
    onChange(nextApi)
    if (onKeyMinted) {
      try {
        await onKeyMinted(nextApi)
      } catch (e) {
        toast.error(t('bots.api.save_failed'), { description: String(e) })
      }
    }
  }

  const health = async () => {
    if (value.base_url.trim() === '') {
      setErr(t('bots.api.need_url'))
      return
    }
    setErr(null)
    try {
      const h = await invoke<{ status: string; models: string[] }>('native_api_health', {
        baseUrl: value.base_url,
      })
      toast.success(t('bots.api.health_ok', { status: h.status }), {
        description: h.models.join(', '),
      })
    } catch (e) {
      setErr(String(e))
    }
  }

  return (
    <div className="grid gap-4">
      <p className="text-sm text-muted-foreground">{t('bots.api.desc')}</p>

      <div className="flex items-center justify-between gap-4">
        <div className="flex flex-col">
          <Label>{t('bots.api.enable')}</Label>
          <span className="text-xs text-muted-foreground">{t('bots.api.enable_hint')}</span>
        </div>
        <div className="flex items-center gap-2">
          {enabling && <RefreshCw className="h-4 w-4 animate-spin text-muted-foreground" />}
          <Switch checked={value.enabled} onCheckedChange={(v) => void toggleEnabled(v)} />
        </div>
      </div>

      <div className="grid gap-1.5">
        <Label>{t('bots.api.base_url')}</Label>
        <Input
          value={value.base_url}
          onChange={(e) => set({ base_url: e.target.value })}
          placeholder="https://mjapi.shinkuan.me"
          autoComplete="off"
          spellCheck={false}
        />
      </div>

      <div className="grid gap-1.5">
        <Label>{t('bots.api.key')}</Label>
        <div className="flex gap-2">
          <Input
            type={showKey ? 'text' : 'password'}
            value={value.key}
            onChange={(e) => set({ key: e.target.value })}
            placeholder="••••••••••••••••••••••••••••••••"
            autoComplete="off"
            spellCheck={false}
            className="font-mono"
          />
          <Button
            variant="outline"
            size="icon"
            className="shrink-0"
            onClick={() => setShowKey((s) => !s)}
            title={showKey ? t('bots.api.hide_key') : t('bots.api.show_key')}
          >
            {showKey ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
          </Button>
        </div>
      </div>

      <div className="grid gap-4 sm:grid-cols-2">
        <div className="grid gap-1.5">
          <Label>{t('bots.api.model_4p')}</Label>
          <Input
            value={value.model_4p}
            onChange={(e) => set({ model_4p: e.target.value })}
            placeholder="4p-ot2"
            autoComplete="off"
            spellCheck={false}
            className="font-mono"
          />
        </div>
        <div className="grid gap-1.5">
          <Label>{t('bots.api.model_3p')}</Label>
          <Input
            value={value.model_3p}
            onChange={(e) => set({ model_3p: e.target.value })}
            placeholder="3p-ot"
            autoComplete="off"
            spellCheck={false}
            className="font-mono"
          />
        </div>
      </div>
      <span className="text-xs text-muted-foreground -mt-2">{t('bots.api.model_hint')}</span>

      {models && (
        <div className="grid gap-1.5">
          <Label className="text-xs text-muted-foreground">{t('bots.api.models_title')}</Label>
          <div className="flex flex-wrap gap-1.5">
            {models.length === 0 && (
              <span className="text-xs text-muted-foreground">{t('bots.api.models_empty')}</span>
            )}
            {models.map((m) => (
              <Button
                key={m.id}
                size="sm"
                variant="secondary"
                className="h-7 font-mono text-xs"
                title={m.desc}
                onClick={() => set(m.game === '3p' ? { model_3p: m.id } : { model_4p: m.id })}
              >
                {m.id}
              </Button>
            ))}
          </div>
        </div>
      )}

      {status && (
        <div className="grid grid-cols-2 gap-x-6 gap-y-1 rounded-md border border-border p-3 text-sm sm:grid-cols-3">
          <Kv label={t('bots.api.plan')} value={status.plan || '—'} />
          <Kv label={t('bots.api.expires')} value={status.expires_at || '—'} />
          <Kv label={t('bots.api.usage')} value={`${status.usage_today} / ${status.rpd}`} />
          <Kv label={t('bots.api.rpm')} value={String(status.rpm)} />
          <Kv label={t('bots.api.topk')} value={String(status.topk)} />
        </div>
      )}

      {err && <span className="text-sm text-red-400 [overflow-wrap:anywhere]">{err}</span>}

      <div className="flex flex-wrap items-center gap-2">
        <Button variant="outline" size="sm" onClick={checkKey} disabled={checking} className="gap-1.5">
          <CheckCircle2 className="h-4 w-4" />
          {checking ? t('bots.api.checking') : t('bots.api.check_key')}
        </Button>
        <Button
          variant="outline"
          size="sm"
          onClick={fetchModels}
          disabled={loadingModels}
          className="gap-1.5"
        >
          <RefreshCw className={`h-4 w-4 ${loadingModels ? 'animate-spin' : ''}`} />
          {t('bots.api.fetch_models')}
        </Button>
        <Button variant="outline" size="sm" onClick={health} className="gap-1.5">
          <Activity className="h-4 w-4" />
          {t('bots.api.health')}
        </Button>
        <Button variant="outline" size="sm" onClick={() => setRedeemOpen(true)} className="gap-1.5">
          <Ticket className="h-4 w-4" />
          {t('bots.api.redeem')}
        </Button>
        <Button variant="outline" size="sm" onClick={() => setBuyOpen(true)} className="gap-1.5">
          <ShoppingCart className="h-4 w-4" />
          {t('bots.api.buy')}
        </Button>
      </div>

      {purchasePhase !== 'idle' && !buyOpen && (
        <PurchaseChip phase={purchasePhase} onOpen={() => setBuyOpen(true)} />
      )}

      {redeemOpen && (
        <RedeemDialog
          baseUrl={value.base_url}
          currentKey={value.key}
          onClose={() => setRedeemOpen(false)}
          onNewKey={(key) => void adoptNewKey(key)}
        />
      )}

      {buyOpen && (
        <PurchaseDialog
          baseUrl={value.base_url}
          currentKey={value.key}
          onClose={() => setBuyOpen(false)}
          onNewKey={(key) => void adoptNewKey(key)}
        />
      )}
    </div>
  )
}

/** Compact status row for a purchase running with its dialog closed, so the
 *  buyer can find their way back to it (or see that it completed). */
function PurchaseChip({ phase, onOpen }: { phase: PurchasePhase; onOpen: () => void }) {
  const { t } = useTranslation()
  const [label, icon] =
    phase === 'redeem_failed'
      ? [t('bots.api.buy_chip_action'), <AlertTriangle key="i" className="h-4 w-4 text-amber-400" />]
      : phase === 'done'
        ? [t('bots.api.buy_chip_done'), <CheckCircle2 key="i" className="h-4 w-4 text-emerald-400" />]
        : phase === 'delivered'
          ? [t('bots.api.buy_chip_delivered'), <CheckCircle2 key="i" className="h-4 w-4 text-emerald-400" />]
          : phase === 'failed'
            ? [t('bots.api.buy_chip_failed'), <XCircle key="i" className="h-4 w-4 text-red-400" />]
            : [
                t('bots.api.buy_chip_pending'),
                <RefreshCw key="i" className="h-4 w-4 animate-spin text-muted-foreground" />,
              ]
  return (
    <button
      type="button"
      onClick={onOpen}
      className="flex w-fit items-center gap-2 rounded-md border border-border px-3 py-1.5 text-sm transition-colors hover:bg-accent"
    >
      {icon}
      {label}
    </button>
  )
}

function Kv({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex flex-col">
      <span className="text-xs text-muted-foreground">{label}</span>
      <span className="font-mono [overflow-wrap:anywhere]">{value}</span>
    </div>
  )
}

function RedeemDialog({
  baseUrl,
  currentKey,
  onClose,
  onNewKey,
}: {
  baseUrl: string
  currentKey: string
  onClose: () => void
  onNewKey: (key: string) => void
}) {
  const { t } = useTranslation()
  const [code, setCode] = useState('')
  const [email, setEmail] = useState('')
  const [renew, setRenew] = useState(false)
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)
  const [done, setDone] = useState<string | null>(null)

  const submit = async () => {
    if (baseUrl.trim() === '') {
      setErr(t('bots.api.need_url'))
      return
    }
    if (renew && currentKey.trim() === '') {
      setErr(t('bots.api.redeem_renew_hint'))
      return
    }
    setBusy(true)
    setErr(null)
    setDone(null)
    try {
      const resp = await invoke<RedeemResponse>('native_api_redeem', {
        baseUrl,
        code: code.trim(),
        email: email.trim() || undefined,
        renewKey: renew ? currentKey : undefined,
      })
      if (resp.key) {
        onNewKey(resp.key)
        setDone(t('bots.api.redeem_new_key', { last4: resp.key_last4, plan: resp.plan }))
        toast.success(t('bots.api.redeem_ok'))
      } else {
        setDone(t('bots.api.redeemed_extended', { last4: resp.key_last4, expires: resp.expires_at }))
        toast.success(t('bots.api.redeem_ok'))
      }
    } catch (e) {
      setErr(String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('bots.api.redeem_title')}</DialogTitle>
        </DialogHeader>
        <div className="grid min-w-0 gap-4 py-2">
          <div className="grid gap-1.5">
            <Label>{t('bots.api.redeem_code')}</Label>
            <Input
              value={code}
              onChange={(e) => setCode(e.target.value)}
              placeholder="XXXXXXXXXXXXXXXX"
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
            />
          </div>
          <div className="flex items-center justify-between gap-4">
            <div className="flex flex-col">
              <Label>{t('bots.api.redeem_renew')}</Label>
              <span className="text-xs text-muted-foreground">{t('bots.api.redeem_renew_hint')}</span>
            </div>
            <Switch checked={renew} onCheckedChange={setRenew} disabled={currentKey.trim() === ''} />
          </div>
          {!renew && (
            <div className="grid gap-1.5">
              <Label>{t('bots.api.redeem_email')}</Label>
              <Input
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                placeholder="you@example.com"
                autoComplete="off"
                spellCheck={false}
              />
              <span className="text-xs text-muted-foreground">{t('bots.api.redeem_email_hint')}</span>
            </div>
          )}
          {done && <span className="text-sm text-emerald-400 [overflow-wrap:anywhere]">{done}</span>}
          {err && <span className="text-sm text-red-400 [overflow-wrap:anywhere]">{err}</span>}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {t('common.close')}
          </Button>
          <Button onClick={submit} disabled={busy || code.trim() === ''}>
            {busy ? t('bots.api.redeeming') : t('bots.api.redeem_submit')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
