// First-run wizard defaults that differ from the persisted config defaults.
//
// The Rust side (`src/config/capture.rs`) keeps `mitm` as the capture-mode
// default so that a bare/older config still round-trips to MITM. The first-run
// wizard, however, should greet brand-new users with the friendlier Chromium
// backend pre-selected — it needs no CA-certificate install. This module
// bridges that gap without changing the persisted default.

import { platformInfo } from './platforms'
import type { AppConfig } from '@/types'

/**
 * Pre-select the Chromium capture backend for genuinely new users.
 *
 * A freshly created config carries the Rust-side default capture mode
 * (`mitm`). For a real first run we instead default the wizard to Chromium,
 * but only when every condition holds:
 *   - setup has never been completed (`first_run_completed === false`) — a
 *     Settings re-run must keep the user's saved mode, not reset it;
 *   - the active platform actually supports Chromium — native-only platforms
 *     such as Riichi City are MITM-only;
 *   - the mode is still the untouched default (`mitm`) — we never override a
 *     mode a previous step/seed already chose.
 *
 * Returns the config unchanged when any condition fails, so it is safe to run
 * on every draft seed (idempotent).
 */
export function withFirstRunCaptureDefault(config: AppConfig): AppConfig {
  if (config.general.first_run_completed) return config
  if (config.capture.mode !== 'mitm') return config
  if (!platformInfo(config.platform.kind).supportsChromium) return config
  return {
    ...config,
    capture: { ...config.capture, mode: 'chromium' },
  }
}
