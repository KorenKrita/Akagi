import { useEffect, useRef, useState } from 'react'
import { useBlocker } from 'react-router-dom'
import { useTranslation } from 'react-i18next'
import { Plus, Settings as SettingsIcon, RefreshCw, CheckCircle2, Trash2, FileArchive, Download } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog'
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { open as openFileDialog } from '@tauri-apps/plugin-dialog'
import { invoke } from '@/lib/tauri'
import { withInstallBlock } from '@/lib/install'
import { toast } from '@/components/ui/sonner'
import { useBotStore } from '@/stores/botStore'
import { useConfigStore } from '@/stores/configStore'
import type { AppConfig, BotInfo, BotSettings, NativeApiConfig } from '@/types'
import { ManifestField } from '@/components/ManifestField'
import { MjotLogo } from '@/components/MjotBrand'
import { NativeApiFields } from '@/components/NativeApiFields'
import { checkApiBeforeSave, persistApiConfig, withNativeBotForApi } from '@/lib/nativeApi'
import { NATIVE_3P, NATIVE_4P, isNativeBot } from '@/lib/nativeBots'
import { proxyConfigValid } from '@/lib/proxy'
import { mergeExternal } from '@/lib/merge'

// The built-in bots have no directory, no manifest, and nothing to
// install/configure/delete; names and the `isNativeBot` predicate come from
// the shared `@/lib/nativeBots`.
function nativeModes(name: string): string[] {
  return name === NATIVE_3P ? ['3p'] : ['4p']
}

export function Bots() {
  const { t } = useTranslation()
  const list = useBotStore((s) => s.list)
  const setList = useBotStore((s) => s.setList)
  const config = useConfigStore((s) => s.config)
  const setConfig = useConfigStore((s) => s.setConfig)
  const [loading, setLoading] = useState(false)
  const [editing, setEditing] = useState<string | null>(null)
  const [deleting, setDeleting] = useState<string | null>(null)
  const [installingEnv, setInstallingEnv] = useState<string | null>(null)

  const refresh = async () => {
    setLoading(true)
    try {
      const [bots, cfg] = await Promise.all([
        invoke<BotInfo[]>('list_bots'),
        invoke<AppConfig>('get_config'),
      ])
      setList(bots)
      setConfig(cfg)
    } catch {
      /* notify event will surface */
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    // Mount-time load; refresh() flips loading/list state internally.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    if (list.length === 0) void refresh()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const setActive = async (mode: '4p' | '3p', name: string) => {
    const current = mode === '3p' ? config?.bot.active_3p : config?.bot.active_4p
    if (current === name) return
    // Optimistic: flip immediately so the Switch reflects the click; refresh
    // backfills from backend in case the call fails or the value differs.
    if (config) {
      const bot = { ...config.bot, [mode === '3p' ? 'active_3p' : 'active_4p']: name }
      setConfig({ ...config, bot })
    }
    try {
      await invoke('set_active_bot', { mode, name })
      // Mirror of "enabling MJOT selects the built-in bots": deactivating the
      // LAST built-in slot would leave the API enabled with nothing routing
      // through it — dead config that reads as working. Turn it off and say
      // so. One native slot remaining keeps the API on: it still serves that
      // mode.
      if (config?.bot.api.enabled) {
        const next4p = mode === '4p' ? name : config.bot.active_4p
        const next3p = mode === '3p' ? name : config.bot.active_3p
        if (!isNativeBot(next4p) && !isNativeBot(next3p)) {
          // Separate catch: the activation above already succeeded, so a
          // failure HERE must not be reported as "activate failed" — the API
          // just stayed enabled; the user can turn it off on the MJOT card.
          try {
            await persistApiConfig({ ...config.bot.api, enabled: false })
            toast.info(t('bots.api.disabled_no_native'))
          } catch (e) {
            toast.error(t('bots.api.save_failed'), { description: String(e) })
          }
        } else if (name !== '' && !isNativeBot(name)) {
          // Only this mode left the built-in bot; the other still uses it, so
          // the API stays on — but say which mode MJOT no longer covers, or
          // the user will assume both modes are served.
          toast.info(
            t('bots.api.mode_not_native', {
              mode: mode === '4p' ? '4P' : '3P',
              other: mode === '4p' ? '3P' : '4P',
            }),
          )
        }
      }
    } catch (e) {
      // set_active_bot rejects (e.g. env not installed) without emitting a
      // notify toast, so surface it here — otherwise the optimistic flip just
      // reverts on refresh with no explanation.
      toast.error(t('bots.activate_failed'), { description: String(e) })
    } finally {
      void refresh()
    }
  }

  // Build a bot's Python environment (uv sync) on demand. This is the only
  // path that works for a bot dropped straight into the bots directory with a
  // pyproject.toml but no manifest.toml: activation is gated on env readiness,
  // game-start sync needs activation first, and the drawer's "Reinstall
  // environment" button is reachable only via Configure (manifest-gated).
  // sync_bot_deps emits its own success/failure notify toasts, so we don't
  // double-report here.
  const installEnv = async (name: string) => {
    setInstallingEnv(name)
    try {
      await withInstallBlock(() => invoke('sync_bot_deps', { name, force: false }))
      await refresh()
    } catch {
      /* backend emits a `bot-sync-<name>` error toast */
    } finally {
      setInstallingEnv(null)
    }
  }

  function supportsMode(bot: BotInfo, mode: '4p' | '3p'): boolean {
    if (isNativeBot(bot.name)) return nativeModes(bot.name).includes(mode)
    const modes = bot.manifest?.bot.supported_modes ?? ['4p']
    return modes.includes(mode)
  }

  // Friendly label for the built-in bots (they have no manifest `display`).
  function botLabel(bot: BotInfo): string {
    if (bot.name === NATIVE_4P) return t('bots.native_4p')
    if (bot.name === NATIVE_3P) return t('bots.native_3p')
    return bot.manifest?.bot.display ?? bot.name
  }

  return (
    <div className="p-6 flex flex-col gap-4">
      <header className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold">{t('bots.title')}</h1>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" onClick={refresh} disabled={loading} className="gap-1.5">
            <RefreshCw className={`h-4 w-4 ${loading ? 'animate-spin' : ''}`} />
            {t('common.refresh')}
          </Button>
          <InstallFromGithubDialog onInstalled={refresh} />
          <InstallFromZipDialog onInstalled={refresh} />
        </div>
      </header>

      {/* MJOT (cloud inference) leads the page: it is the built-in bot's main
          upgrade knob, while the table below is the power-user path for
          author bots. */}
      <NativeApiSettings />

      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>{t('bots.table_name')}</TableHead>
            <TableHead>{t('bots.table_version')}</TableHead>
            <TableHead>{t('bots.table_manifest')}</TableHead>
            <TableHead>{t('bots.table_4p')}</TableHead>
            <TableHead>{t('bots.table_3p')}</TableHead>
            <TableHead className="text-right">{t('bots.table_actions')}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {list.length === 0 ? (
            <TableRow>
              <TableCell colSpan={6} className="text-center text-muted-foreground">
                {loading ? t('bots.loading') : t('bots.empty')}
              </TableCell>
            </TableRow>
          ) : list.map((bot) => {
            const isActive4p = config?.bot.active_4p === bot.name
            const isActive3p = config?.bot.active_3p === bot.name
            const isActive = isActive4p || isActive3p
            return (
              <TableRow key={bot.name}>
                <TableCell>
                  <div className="flex flex-col">
                    <span className="font-medium">{botLabel(bot)}</span>
                    <span className="text-xs text-muted-foreground font-mono">
                      {isNativeBot(bot.name) ? t('bots.native_builtin') : bot.dir}
                    </span>
                  </div>
                </TableCell>
                <TableCell className="font-mono text-xs">
                  {isNativeBot(bot.name) ? t('bots.native_version') : (bot.manifest?.bot.version ?? '—')}
                </TableCell>
                <TableCell>{bot.manifest ? <CheckCircle2 className="h-4 w-4 text-emerald-400" /> : '—'}</TableCell>
                <TableCell>
                  <span title={!bot.env_ready && !isActive4p ? t('bots.env_not_ready_tooltip') : undefined}>
                    <Switch
                      checked={isActive4p}
                      // Block activating a bot whose env isn't installed yet,
                      // but still allow turning one OFF (env may have been
                      // wiped while it was active).
                      disabled={!supportsMode(bot, '4p') || (!bot.env_ready && !isActive4p)}
                      onCheckedChange={(v) => void setActive('4p', v ? bot.name : '')}
                    />
                  </span>
                </TableCell>
                <TableCell>
                  <span title={!bot.env_ready && !isActive3p ? t('bots.env_not_ready_tooltip') : undefined}>
                    <Switch
                      checked={isActive3p}
                      disabled={!supportsMode(bot, '3p') || (!bot.env_ready && !isActive3p)}
                      onCheckedChange={(v) => void setActive('3p', v ? bot.name : '')}
                    />
                  </span>
                </TableCell>
                <TableCell className="text-right">
                  {isNativeBot(bot.name) ? (
                    <span className="text-xs text-muted-foreground">{t('bots.native_builtin')}</span>
                  ) : (
                  <div className="flex items-center justify-end gap-1">
                    {bot.has_pyproject && !bot.env_ready && (
                      <Button
                        size="sm"
                        variant="outline"
                        onClick={() => void installEnv(bot.name)}
                        disabled={installingEnv !== null}
                        title={t('bots.install_env_tooltip')}
                        className="gap-1.5"
                      >
                        <Download className={`h-4 w-4 ${installingEnv === bot.name ? 'animate-pulse' : ''}`} />
                        {installingEnv === bot.name ? t('bots.installing_env') : t('bots.install_env')}
                      </Button>
                    )}
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => setEditing(bot.name)}
                      disabled={!bot.manifest}
                      className="gap-1.5"
                    >
                      <SettingsIcon className="h-4 w-4" />
                      {t('bots.configure_btn')}
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => setDeleting(bot.name)}
                      disabled={isActive}
                      title={isActive ? t('bots.delete_tooltip_active') : t('common.delete')}
                      className="gap-1.5 text-red-400 hover:text-red-400"
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </div>
                  )}
                </TableCell>
              </TableRow>
            )
          })}
        </TableBody>
      </Table>

      {editing && (
        <BotSettingsDrawer
          name={editing}
          open
          onOpenChange={(open) => !open && setEditing(null)}
          onEnvChanged={refresh}
        />
      )}

      {deleting && (
        <DeleteBotDialog
          name={deleting}
          onClose={() => setDeleting(null)}
          onDeleted={refresh}
        />
      )}
    </div>
  )
}

function DeleteBotDialog({
  name, onClose, onDeleted,
}: {
  name: string
  onClose: () => void
  onDeleted: () => void
}) {
  const { t } = useTranslation()
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)

  const submit = async () => {
    setBusy(true)
    setErr(null)
    try {
      await invoke('delete_bot', { name })
      onClose()
      onDeleted()
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
          <DialogTitle>{t('bots.delete_title', { name })}</DialogTitle>
        </DialogHeader>
        <p className="text-sm text-muted-foreground">
          {t('bots.delete_desc_pre')}
          <span className="font-mono">{name}</span>
          {t('bots.delete_desc_post')}
        </p>
        {err && <span className="text-sm text-red-400 [overflow-wrap:anywhere]">{err}</span>}
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>{t('common.cancel')}</Button>
          <Button variant="destructive" onClick={submit} disabled={busy}>
            {busy ? t('common.deleting') : t('common.delete')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function InstallFromGithubDialog({ onInstalled }: { onInstalled: () => void }) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const [repo, setRepo] = useState('')
  const [name, setName] = useState('')
  const [glob, setGlob] = useState('')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)

  const submit = async () => {
    setBusy(true)
    setErr(null)
    try {
      await withInstallBlock(() =>
        invoke('install_bot_from_github', {
          repo,
          assetGlob: glob || undefined,
          name: name || undefined,
        }),
      )
      setOpen(false)
      setRepo('')
      setName('')
      setGlob('')
      onInstalled()
    } catch (e) {
      setErr(String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button size="sm" className="gap-1.5">
          <Plus className="h-4 w-4" />
          {t('bots.install_btn')}
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('bots.install_title')}</DialogTitle>
        </DialogHeader>
        <div className="grid min-w-0 gap-4 py-2">
          <div className="grid gap-1.5">
            <Label>{t('bots.install_repo')}</Label>
            <Input
              value={repo}
              onChange={(e) => setRepo(e.target.value)}
              placeholder={t('bots.install_repo_placeholder')}
            />
            <span className="text-xs text-muted-foreground">
              {t('bots.install_repo_hint_pre')}
              <span className="font-mono">owner/name</span>
              {t('bots.install_repo_hint_mid')}
              <span className="font-mono">https://github.com/owner/name</span>
              {t('bots.install_repo_hint_post')}
            </span>
          </div>
          <div className="grid gap-1.5">
            <Label>{t('bots.install_glob')}</Label>
            <Input value={glob} onChange={(e) => setGlob(e.target.value)} placeholder="*-linux.zip" />
          </div>
          <div className="grid gap-1.5">
            <Label>{t('bots.install_local_name')}</Label>
            <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="mortal" />
          </div>
          {err && (
            <span className="text-sm text-red-400 [overflow-wrap:anywhere]">{err}</span>
          )}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => setOpen(false)}>{t('common.cancel')}</Button>
          <Button onClick={submit} disabled={busy || !repo}>
            {busy ? t('common.installing') : t('common.install')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function InstallFromZipDialog({ onInstalled }: { onInstalled: () => void }) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const [zipPath, setZipPath] = useState('')
  const [name, setName] = useState('')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)

  const browse = async () => {
    setErr(null)
    try {
      const picked = await openFileDialog({
        multiple: false,
        directory: false,
        filters: [{ name: 'Zip archive', extensions: ['zip'] }],
      })
      if (typeof picked === 'string') setZipPath(picked)
    } catch (e) {
      setErr(String(e))
    }
  }

  const submit = async () => {
    setBusy(true)
    setErr(null)
    try {
      await withInstallBlock(() =>
        invoke('install_bot_from_zip', {
          zipPath,
          name: name || undefined,
        }),
      )
      setOpen(false)
      setZipPath('')
      setName('')
      onInstalled()
    } catch (e) {
      setErr(String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button size="sm" variant="outline" className="gap-1.5">
          <FileArchive className="h-4 w-4" />
          {t('bots.install_zip_btn')}
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('bots.install_zip_title')}</DialogTitle>
        </DialogHeader>
        <div className="grid min-w-0 gap-4 py-2">
          <div className="grid gap-1.5">
            <Label>{t('bots.install_zip_path')}</Label>
            <div className="flex gap-2">
              <Input
                value={zipPath}
                onChange={(e) => setZipPath(e.target.value)}
                placeholder={t('bots.install_zip_path_placeholder')}
              />
              <Button variant="outline" onClick={browse} className="shrink-0">
                {t('bots.install_zip_browse')}
              </Button>
            </div>
            <span className="text-xs text-muted-foreground">
              {t('bots.install_zip_path_hint')}
            </span>
          </div>
          <div className="grid gap-1.5">
            <Label>{t('bots.install_local_name')}</Label>
            <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="mortal" />
          </div>
          {err && (
            <span className="text-sm text-red-400 [overflow-wrap:anywhere]">{err}</span>
          )}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => setOpen(false)}>{t('common.cancel')}</Button>
          <Button onClick={submit} disabled={busy || !zipPath}>
            {busy ? t('common.installing') : t('common.install')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function BotSettingsDrawer({ name, open, onOpenChange, onEnvChanged }: { name: string; open: boolean; onOpenChange: (open: boolean) => void; onEnvChanged: () => void }) {
  const { t } = useTranslation()
  const [data, setData] = useState<BotSettings | null>(null)
  const [values, setValues] = useState<Record<string, unknown>>({})
  const [saving, setSaving] = useState(false)
  const [resyncing, setResyncing] = useState(false)
  const [err, setErr] = useState<string | null>(null)

  useEffect(() => {
    if (!open) return
    invoke<BotSettings>('get_bot_settings', { name })
      .then((s) => {
        setData(s)
        setValues(s.values)
      })
      .catch((e) => setErr(String(e)))
  }, [name, open])

  const save = async () => {
    setSaving(true)
    setErr(null)
    try {
      await invoke('update_bot_settings', { name, values })
      onOpenChange(false)
    } catch (e) {
      setErr(String(e))
    } finally {
      setSaving(false)
    }
  }

  const reinstallEnv = async () => {
    setResyncing(true)
    setErr(null)
    try {
      await withInstallBlock(() => invoke('sync_bot_deps', { name, force: true }))
      // Re-list bots so `env_ready` reflects the freshly-synced env and the
      // activation switch becomes enabled without a manual refresh.
      onEnvChanged()
    } catch (e) {
      setErr(String(e))
    } finally {
      setResyncing(false)
    }
  }

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className="flex flex-col gap-4 overflow-y-auto p-4 sm:max-w-md">
        <SheetHeader className="p-0">
          <SheetTitle>{data?.manifest.bot.display ?? name}</SheetTitle>
          <SheetDescription>{data?.manifest.bot.description}</SheetDescription>
        </SheetHeader>

        {!data ? (
          <div className="text-muted-foreground text-sm">{t('bots.drawer_loading')}</div>
        ) : (
          <div className="grid gap-4">
            {Object.entries(data.manifest.settings).map(([key, spec]) => (
              <ManifestField
                key={key}
                fieldKey={key}
                spec={spec}
                value={values[key] ?? spec.default}
                onChange={(v) => setValues({ ...values, [key]: v })}
              />
            ))}
          </div>
        )}

        {err && <span className="text-sm text-red-400 [overflow-wrap:anywhere]">{err}</span>}

        <div className="flex justify-between gap-2 mt-auto pt-2 border-t border-border">
          <Button
            variant="outline"
            onClick={reinstallEnv}
            disabled={saving || resyncing}
            title={t('bots.drawer_reinstall_tooltip')}
          >
            {resyncing ? t('bots.drawer_reinstalling') : t('bots.drawer_reinstall')}
          </Button>
          <div className="flex gap-2">
            <Button variant="outline" onClick={() => onOpenChange(false)}>{t('common.cancel')}</Button>
            <Button onClick={save} disabled={saving || resyncing || !data}>
              {saving ? t('common.saving') : t('common.save')}
            </Button>
          </div>
        </div>
      </SheetContent>
    </Sheet>
  )
}

// Cloud-inference settings for the built-in native bot, as a card on the Bots
// tab. The field editor is shared with the Setup wizard (`NativeApiFields`);
// here we wrap it in Card chrome and an explicit Save button.
function NativeApiSettings() {
  const { t } = useTranslation()
  const config = useConfigStore((s) => s.config)
  const setConfig = useConfigStore((s) => s.setConfig)
  const api = config?.bot.api

  const [draft, setDraft] = useState<NativeApiConfig | null>(null)
  const [saving, setSaving] = useState(false)
  // Stored `bot.api` snapshot the draft was last synced against — the merge
  // base for folding external config changes into the open draft.
  const syncedApiRef = useRef<NativeApiConfig | null>(null)

  useEffect(() => {
    if (!api) return
    const prev = syncedApiRef.current
    syncedApiRef.current = api
    if (!prev) {
      // Seed the editable draft once the config loads.
      setDraft({ ...api })
      return
    }
    // The stored config changed while the draft is open (e.g. a purchased key
    // was delivered and persisted while the purchase dialog was unmounted).
    // Three-way merge: fields the user hasn't touched (draft still equal to
    // the previous stored snapshot) adopt the new stored value; fields the
    // user edited keep their draft value. `dirty` below recomputes from the
    // merged result, so Save can never silently revert a delivered key.
    setDraft((cur) => (cur ? mergeExternal(cur, prev, api) : { ...api }))
  }, [api])

  // Same unsaved-changes guard as the Settings page: block in-app navigation
  // while the draft differs from the stored config, and warn on window close.
  // (A minted or purchased key never trips this: `onKeyMinted` persists it
  // immediately, and external persists — e.g. the purchase store's fallback —
  // are merged into the untouched draft fields above, so stored and draft
  // only diverge where the user really has unsaved edits.)
  const dirty = !!api && !!draft && JSON.stringify(draft) !== JSON.stringify(api)

  const blocker = useBlocker(
    ({ currentLocation, nextLocation }) =>
      dirty && currentLocation.pathname !== nextLocation.pathname,
  )

  useEffect(() => {
    if (!dirty) return
    const handler = (e: BeforeUnloadEvent) => {
      e.preventDefault()
      e.returnValue = ''
    }
    window.addEventListener('beforeunload', handler)
    return () => window.removeEventListener('beforeunload', handler)
  }, [dirty])

  if (!config || !draft) return null

  const save = async (): Promise<boolean> => {
    setSaving(true)
    try {
      // With cloud inference enabled, refuse to persist a key that doesn't
      // work: a saved-but-broken key silently falls back to the local model
      // every turn, so the user would think the API is on when it isn't. Block
      // the save and surface why — they must fix the key or turn the API off.
      const check = await checkApiBeforeSave(draft)
      if (!check.ok) {
        toast.error(t('bots.api.save_key_check_failed'), {
          description: check.kind === 'missing' ? t('bots.api.need_url_key') : check.message,
        })
        return false
      }
      // Saving with the API enabled also selects the built-in bots — MJOT
      // only applies to them, so leaving an author bot active would persist
      // an enabled API that nothing uses.
      const { next, switched } = withNativeBotForApi(config, draft)
      await invoke('update_config', { newConfig: next })
      setConfig(next)
      toast.success(t('bots.api.saved'))
      if (switched) toast.info(t('bots.api.native_selected'))
      return true
    } catch (e) {
      toast.error(t('bots.api.save_failed'), { description: String(e) })
      return false
    } finally {
      setSaving(false)
    }
  }

  const saveAndLeave = async () => {
    if (await save()) {
      blocker.proceed?.()
    } else {
      blocker.reset?.()
    }
  }

  const discardAndLeave = () => {
    setDraft({ ...config.bot.api })
    blocker.proceed?.()
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2.5">
          {/* The lockup's wordmark reads "MJOT", so the visible title text
              stays the descriptive half only. */}
          <MjotLogo className="h-6 w-auto" label="MJOT" />
          <span className="text-muted-foreground">·</span>
          {t('bots.api.title')}
        </CardTitle>
      </CardHeader>
      <CardContent className="grid gap-4">
        <NativeApiFields value={draft} onChange={setDraft} onKeyMinted={persistApiConfig} />
        <div className="border-t border-border pt-3">
          <Button onClick={save} disabled={saving || !dirty || !proxyConfigValid(draft)}>
            {saving ? t('common.saving') : t('common.save')}
          </Button>
        </div>
      </CardContent>

      <Dialog
        open={blocker.state === 'blocked'}
        onOpenChange={(open) => {
          if (!open) blocker.reset?.()
        }}
      >
        <DialogContent showCloseButton={false}>
          <DialogHeader>
            <DialogTitle>{t('settings.unsaved_title')}</DialogTitle>
            <DialogDescription>{t('settings.unsaved_desc')}</DialogDescription>
          </DialogHeader>
          <DialogFooter className="bg-transparent p-0 border-0 mx-0 mb-0">
            <Button
              variant="outline"
              size="sm"
              onClick={() => blocker.reset?.()}
              disabled={saving}
            >
              {t('common.stay')}
            </Button>
            <Button variant="destructive" size="sm" onClick={discardAndLeave} disabled={saving}>
              {t('common.discard')}
            </Button>
            <Button size="sm" onClick={saveAndLeave} disabled={saving}>
              {saving ? t('common.saving') : t('settings.save_and_leave')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Card>
  )
}

