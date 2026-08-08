import { describe, expect, it } from 'vitest'

import { withFirstRunCaptureDefault } from './setupDefaults'
import type { AppConfig, CaptureMode, PlatformKind } from '@/types'

function makeConfig(over: {
  firstRunCompleted?: boolean
  platform?: PlatformKind
  mode?: CaptureMode
}): AppConfig {
  return {
    general: {
      first_run_completed: over.firstRunCompleted ?? false,
      developer_mode: false,
    },
    logging: { dir: '', level: 'info', all_level: 'warn' },
    platform: { kind: over.platform ?? 'Majsoul' },
    proxy: { enabled: true, addr: '127.0.0.1:23410', ca_dir: '' },
    bot: {
      enabled: true,
      active_4p: 'akagi-native',
      active_3p: 'akagi-native3p',
      auto_sync: false,
      dir: '',
      api: {
        enabled: false,
        base_url: '',
        key: '',
        model_4p: '',
        model_3p: '',
        use_system_proxy: false,
        proxy_enabled: false,
        proxy: '',
      },
    },
    capture: {
      mode: over.mode ?? 'mitm',
      chromium: {
        executable: '',
        user_data_dir: '',
        start_url: 'https://game.maj-soul.com/1/',
        cft_channel: 'stable',
        force_cft: false,
        show_danger_overlay: false,
        show_recommendation_overlay: false,
        extra_args: [],
      },
    },
    autoplay: {
      enabled: false,
      majsoul: {
        mode: 'packet_with_click_fallback',
        pre_click_delay_min_ms: 0,
        pre_click_delay_max_ms: 0,
        inter_click_delay_ms: 0,
        hover_delay_ms: 0,
        click_hold_ms: 0,
        dealer_first_discard_extra_delay_ms: 0,
        auto_join_game: false,
        auto_join_level: 2,
        auto_join_mode: '3e',
        auto_join_stop_after_games: 0,
        auto_join_stop_after_minutes: 0,
      },
      delay: {
        mode: 'legacy',
        min_delay_ms: 0,
        min_button_delay_ms: 0,
        distribution: 'uniform',
        lognormal: {},
        bank_on_long_thought: false,
        riichi_extra_ms: 0,
        kan_extra_ms: 0,
        close_margin: 0,
        close_margin_extra_ms: 0,
        obvious_top_prob: 0,
        obvious_max_ms: 0,
        safety_margin_ms: 0,
        bank_use_fraction: 0,
        bank_max_single_ms: 0,
        no_budget_cap_ms: 0,
      },
    },
    overlay: { enabled: true, top_n: 3, opacity: 1, always_on_top: true },
  }
}

describe('withFirstRunCaptureDefault', () => {
  it('pre-selects Chromium on a first run for a Chromium-capable platform', () => {
    const out = withFirstRunCaptureDefault(makeConfig({ platform: 'Majsoul', mode: 'mitm' }))
    expect(out.capture.mode).toBe('chromium')
  })

  it('keeps the saved mode when setup was already completed (Settings re-run)', () => {
    // A returning user who saved MITM must not be flipped back to Chromium.
    const out = withFirstRunCaptureDefault(
      makeConfig({ firstRunCompleted: true, mode: 'mitm' }),
    )
    expect(out.capture.mode).toBe('mitm')
  })

  it('leaves native-only platforms on MITM even on a first run', () => {
    // Riichi City has no web build → Chromium capture cannot drive it.
    const out = withFirstRunCaptureDefault(
      makeConfig({ platform: 'RiichiCity', mode: 'mitm' }),
    )
    expect(out.capture.mode).toBe('mitm')
  })

  it('never overrides a mode that is already set (not the untouched default)', () => {
    const out = withFirstRunCaptureDefault(makeConfig({ mode: 'chromium' }))
    expect(out.capture.mode).toBe('chromium')
  })

  it('is idempotent and does not mutate its input', () => {
    const input = makeConfig({ platform: 'Majsoul', mode: 'mitm' })
    const once = withFirstRunCaptureDefault(input)
    const twice = withFirstRunCaptureDefault(once)
    expect(twice).toEqual(once)
    // Original untouched — the seed helpers rely on a pure return.
    expect(input.capture.mode).toBe('mitm')
  })
})
