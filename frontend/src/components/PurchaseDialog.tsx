import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import {
  AlertTriangle,
  CheckCircle2,
  ExternalLink,
  Mail,
  RefreshCw,
  XCircle,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { PRODUCTS, type Product } from '@/lib/products'
import { usePurchaseStore } from '@/stores/purchaseStore'

/**
 * Self-serve key purchase (PayPal). Mirrors the RedeemDialog contract: the
 * minted key is handed to `onNewKey`, which routes it into the key field and
 * (on the Bots page) persists it immediately. The purchase itself runs in
 * `purchaseStore`, so closing this dialog mid-checkout does NOT abort it —
 * the buyer can finish on PayPal and reopen the dialog (or just wait; a key
 * that arrives with the dialog closed is auto-saved to the config).
 */
export function PurchaseDialog({
  baseUrl,
  useSystemProxy,
  currentKey,
  onClose,
  onNewKey,
}: {
  baseUrl: string
  useSystemProxy: boolean
  currentKey: string
  onClose: () => void
  onNewKey: (key: string) => void
}) {
  const { t } = useTranslation()
  const p = usePurchaseStore()
  const [productId, setProductId] = useState<string | null>(null)
  const [renew, setRenew] = useState(false)
  const [err, setErr] = useState<string | null>(null)

  const selected: Product | null = PRODUCTS.find((x) => x.id === productId) ?? null
  const busy = p.phase === 'creating' || p.phase === 'approving' || p.phase === 'redeeming'

  // While mounted, a freshly-minted key flows through the same path as a
  // redeemed one (key field + caller-side persist).
  useEffect(() => {
    p.setClaimSink(onNewKey)
    return () => p.setClaimSink(null)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [onNewKey])

  // Claim a key that arrived while no dialog was open (it was auto-saved to
  // the config already; this syncs the visible key field too).
  useEffect(() => {
    if (p.phase === 'done' && p.key && !p.deliveredToUi) {
      usePurchaseStore.getState().markDeliveredToUi()
      onNewKey(p.key)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [p.phase, p.key, p.deliveredToUi])

  const buy = () => {
    if (!selected) return
    if (baseUrl.trim() === '') {
      setErr(t('bots.api.need_url'))
      return
    }
    setErr(null)
    usePurchaseStore.getState().start({
      baseUrl,
      useSystemProxy,
      product: selected,
      renewKey: renew && selected.kind === 'onetime' ? currentKey : undefined,
    })
  }

  const closeTerminal = () => {
    usePurchaseStore.getState().dismiss()
    onClose()
  }

  const failMessage = (failCode: string | null): string => {
    switch (failCode) {
      case 'create':
        return t('bots.api.buy_fail_create')
      case 'claim':
        return t('bots.api.buy_fail_claim')
      case 'timeout':
        return t('bots.api.buy_fail_timeout')
      case 'refunded':
        return t('bots.api.buy_fail_refunded')
      case 'cancelled':
        return t('bots.api.buy_fail_cancelled')
      case 'expired':
        return t('bots.api.buy_fail_expired')
      case 'suspended':
        return t('bots.api.buy_fail_suspended')
      default:
        return t('bots.api.buy_fail_generic', { status: failCode ?? '?' })
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('bots.api.buy_title')}</DialogTitle>
        </DialogHeader>

        {p.phase === 'idle' && (
          <div className="grid min-w-0 gap-4 py-2">
            <p className="text-sm text-muted-foreground">{t('bots.api.buy_desc')}</p>
            <div className="grid gap-2 sm:grid-cols-2">
              {PRODUCTS.map((prod) => (
                <button
                  key={prod.id}
                  type="button"
                  onClick={() => setProductId(prod.id)}
                  // The dialog surface is `bg-popover`, which in the dark
                  // themes sits within a hair of `--border` — a border alone
                  // is invisible there. Give the cards a darker fill
                  // (`bg-background/50`) so they read as items on every
                  // theme, and double up the selected border with a ring.
                  className={`rounded-md border p-3 text-left transition-colors ${
                    productId === prod.id
                      ? 'border-primary bg-primary/10 ring-1 ring-primary'
                      : 'border-border bg-background/50 hover:bg-accent'
                  }`}
                >
                  <div className="flex items-baseline justify-between gap-2">
                    <span className="font-medium">{t(prod.nameKey)}</span>
                    <span className="font-mono text-sm">{prod.price}</span>
                  </div>
                  <span className="text-xs text-muted-foreground">
                    {t(prod.priceSuffixKey)}
                    {prod.kind === 'subscription' && ` · ${t('bots.api.buy_auto_renews')}`}
                  </span>
                </button>
              ))}
            </div>
            <span className="text-xs text-muted-foreground">{t('bots.api.buy_price_note')}</span>

            {selected?.kind === 'onetime' && currentKey.trim() !== '' && (
              <div className="flex items-center justify-between gap-4">
                <div className="flex flex-col">
                  <Label>{t('bots.api.buy_renew')}</Label>
                  <span className="text-xs text-muted-foreground">
                    {t('bots.api.buy_renew_hint')}
                  </span>
                </div>
                <Switch checked={renew} onCheckedChange={setRenew} />
              </div>
            )}
            {selected?.kind === 'subscription' && (
              <span className="text-xs text-muted-foreground">{t('bots.api.buy_sub_hint')}</span>
            )}
            {err && <span className="text-sm text-red-400 [overflow-wrap:anywhere]">{err}</span>}
          </div>
        )}

        {(p.phase === 'creating' || p.phase === 'approving' || p.phase === 'redeeming') && (
          <div className="grid min-w-0 gap-4 py-2">
            <div className="flex items-center gap-3">
              <RefreshCw className="h-5 w-5 shrink-0 animate-spin text-muted-foreground" />
              <span className="text-sm">
                {p.phase === 'creating' && t('bots.api.buy_creating')}
                {p.phase === 'approving' && t('bots.api.buy_waiting')}
                {p.phase === 'redeeming' && t('bots.api.buy_redeeming')}
              </span>
            </div>
            {p.phase === 'approving' && (
              <>
                <span className="text-xs text-muted-foreground">
                  {t('bots.api.buy_waiting_hint')}
                </span>
                <div className="flex flex-wrap gap-2">
                  <Button
                    variant="outline"
                    size="sm"
                    className="gap-1.5"
                    onClick={() => usePurchaseStore.getState().reopenApproveUrl()}
                  >
                    <ExternalLink className="h-4 w-4" />
                    {t('bots.api.buy_reopen')}
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    className="gap-1.5"
                    onClick={() => usePurchaseStore.getState().dismiss()}
                  >
                    <XCircle className="h-4 w-4" />
                    {t('bots.api.buy_cancel')}
                  </Button>
                </div>
              </>
            )}
          </div>
        )}

        {p.phase === 'redeem_failed' && (
          <div className="grid min-w-0 gap-3 py-2">
            <div className="flex items-start gap-2">
              <AlertTriangle className="mt-0.5 h-5 w-5 shrink-0 text-amber-400" />
              <span className="text-sm">{t('bots.api.buy_redeem_failed')}</span>
            </div>
            {p.error && (
              <span className="text-sm text-red-400 [overflow-wrap:anywhere]">{p.error}</span>
            )}
            <div className="grid gap-1.5">
              <Label className="text-xs text-muted-foreground">
                {t('bots.api.buy_code_label')}
              </Label>
              <div className="select-all rounded-md border border-border bg-background/50 p-2 font-mono text-sm [overflow-wrap:anywhere]">
                {p.code}
              </div>
            </div>
            <div className="flex flex-wrap gap-2">
              {p.renewKey ? (
                <>
                  <Button
                    size="sm"
                    className="gap-1.5"
                    onClick={() => usePurchaseStore.getState().retryRedeem(true)}
                  >
                    <RefreshCw className="h-4 w-4" />
                    {t('bots.api.buy_retry_extend')}
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => usePurchaseStore.getState().retryRedeem(false)}
                  >
                    {t('bots.api.buy_mint_instead')}
                  </Button>
                </>
              ) : (
                <Button
                  size="sm"
                  className="gap-1.5"
                  onClick={() => usePurchaseStore.getState().retryRedeem(false)}
                >
                  <RefreshCw className="h-4 w-4" />
                  {t('bots.api.buy_retry')}
                </Button>
              )}
            </div>
          </div>
        )}

        {p.phase === 'done' && (
          <div className="grid min-w-0 gap-3 py-2">
            <div className="flex items-start gap-2">
              <CheckCircle2 className="mt-0.5 h-5 w-5 shrink-0 text-emerald-400" />
              <span className="text-sm">
                {p.key
                  ? t('bots.api.buy_done_key', {
                      plan: p.plan ?? '—',
                      last4: p.key.slice(-4),
                    })
                  : t('bots.api.buy_done_extended', {
                      last4: p.extendedLast4 ?? '????',
                      until: p.until ?? '—',
                    })}
              </span>
            </div>
            {p.key && p.until && (
              <span className="text-sm text-muted-foreground">
                {p.product?.kind === 'subscription'
                  ? t('bots.api.buy_done_billing', { until: p.until })
                  : t('bots.api.buy_done_until', { until: p.until })}
              </span>
            )}
          </div>
        )}

        {p.phase === 'delivered' && (
          <div className="flex items-start gap-2 py-2">
            <Mail className="mt-0.5 h-5 w-5 shrink-0 text-muted-foreground" />
            <span className="text-sm">{t('bots.api.buy_delivered')}</span>
          </div>
        )}

        {p.phase === 'failed' && (
          <div className="grid min-w-0 gap-2 py-2">
            <div className="flex items-start gap-2">
              <XCircle className="mt-0.5 h-5 w-5 shrink-0 text-red-400" />
              <span className="text-sm">{failMessage(p.failCode)}</span>
            </div>
            {p.error && (
              <span className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
                {p.error}
              </span>
            )}
          </div>
        )}

        <DialogFooter>
          {p.phase === 'idle' && (
            <>
              <Button variant="outline" onClick={onClose}>
                {t('common.close')}
              </Button>
              <Button onClick={buy} disabled={!selected}>
                {t('bots.api.buy_submit')}
              </Button>
            </>
          )}
          {busy && (
            <Button variant="outline" onClick={onClose}>
              {t('common.close')}
            </Button>
          )}
          {p.phase === 'redeem_failed' && (
            <Button variant="outline" onClick={onClose}>
              {t('common.close')}
            </Button>
          )}
          {(p.phase === 'done' || p.phase === 'delivered' || p.phase === 'failed') && (
            <Button onClick={closeTerminal}>{t('common.close')}</Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
