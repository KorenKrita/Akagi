import type { NativeApiConfig } from '@/types'

/**
 * Accepted proxy schemes, mirroring the backend whitelist in
 * `crate::bot::api::http_client` (`http`, `https`, `socks5`, `socks5h`). The
 * match is case-insensitive and requires at least one non-space char after
 * `://`, so a bare scheme like `socks5://` is rejected.
 */
const PROXY_URL = /^(?:https?|socks5h?):\/\/\S/i

/** True when `s` is a proxy URL the backend will accept. */
export function isValidProxyUrl(s: string): boolean {
  return PROXY_URL.test(s.trim())
}

/**
 * The proxy actually sent to the backend for this config: the trimmed URL when
 * the toggle is on, else `''` (direct). Keeps the on/off decision in one place
 * so a disabled-but-nonempty proxy never reaches a command.
 */
export function effectiveProxy(api: NativeApiConfig): string {
  return api.proxy_enabled ? api.proxy.trim() : ''
}

/**
 * Whether the proxy portion of the config is savable: always fine when the
 * toggle is off; when on, the URL must be valid (an enabled proxy must point
 * somewhere).
 */
export function proxyConfigValid(api: NativeApiConfig): boolean {
  return !api.proxy_enabled || isValidProxyUrl(api.proxy)
}
