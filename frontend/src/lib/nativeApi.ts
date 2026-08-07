import { invoke } from '@/lib/tauri'
import { useConfigStore } from '@/stores/configStore'
import type { KeyStatus, NativeApiConfig } from '@/types'

/**
 * Result of {@link checkApiBeforeSave}. `ok` gates whether the caller may
 * persist the config; `kind` distinguishes the two blocking reasons so the
 * caller can pick the right (localised) message:
 *  - `missing` — enabled but no server URL / key entered yet.
 *  - `error`   — the server rejected the key; `message` is the raw reason.
 */
export type ApiSaveCheck =
  | { ok: true }
  | { ok: false; kind: 'missing' }
  | { ok: false; kind: 'error'; message: string }

/**
 * Guard run before persisting `bot.api`: when cloud inference is **enabled**,
 * confirm the key actually works (via `GET /v3/key`) so a broken key can't be
 * saved in the enabled state — otherwise the built-in bot would silently fall
 * back to the local model every turn with no signal to the user. A disabled
 * API always passes; there is nothing to check.
 */
export async function checkApiBeforeSave(api: NativeApiConfig): Promise<ApiSaveCheck> {
  if (!api.enabled) return { ok: true }
  const flya = (api.provider ?? 'ot3') === 'flya'
  const baseUrl = flya ? (api.flya_base_url ?? 'https://api.nashout.com') : api.base_url
  const key = flya ? (api.flya_key ?? '') : api.key
  if (baseUrl.trim() === '' || key.trim() === '') {
    return { ok: false, kind: 'missing' }
  }
  try {
    await invoke<KeyStatus>('native_api_key_status', {
      baseUrl,
      key,
      provider: api.provider ?? 'ot3',
      useSystemProxy: api.use_system_proxy,
    })
    return { ok: true }
  } catch (e) {
    return { ok: false, kind: 'error', message: String(e) }
  }
}

/**
 * Persist a `bot.api` change immediately, layered on the *stored* (on-disk)
 * config so only `bot.api` differs from disk. That keeps `update_config` from
 * restarting capture (it only restarts on capture/proxy/platform changes) and
 * touches nothing else. Used for the one case that must not wait for an
 * explicit Save: a redeemed single-use code whose key the server shows once.
 */
export async function persistApiConfig(api: NativeApiConfig): Promise<void> {
  const store = useConfigStore.getState()
  const cfg = store.config
  if (!cfg) return
  const next = { ...cfg, bot: { ...cfg.bot, api } }
  await invoke('update_config', { newConfig: next })
  store.setConfig(next)
}
