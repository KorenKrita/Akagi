import { invoke } from '@/lib/tauri'
import { useConfigStore } from '@/stores/configStore'
import type { NativeApiConfig } from '@/types'

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
