import { useState } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'

import { NativeApiFields } from './NativeApiFields'
import type { NativeApiConfig } from '@/types'

// Regression cover for #221: pasting an API key stored the key and nothing
// else, so the bot kept answering from the local model with a config that
// looked configured. A complete key must now enable cloud inference itself.

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string) => key,
    i18n: { language: 'en', changeLanguage: vi.fn() },
  }),
  initReactI18next: { type: '3rdParty', init: () => {} },
}))

const invoke = vi.fn()
vi.mock('@/lib/tauri', () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invoke(cmd, args),
  HAS_TAURI: false,
  listen: () => Promise.resolve(() => {}),
}))

// The purchase flow is a whole handshake of its own and none of it is under
// test here; the dialog only ever renders after a button press we never make.
vi.mock('@/components/PurchaseDialog', () => ({ PurchaseDialog: () => null }))
vi.mock('@/components/ui/sonner', () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}))

/** A key of the shape the server issues: 32 letters and digits. */
const FULL_KEY = 'a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6'

const base = (patch: Partial<NativeApiConfig> = {}): NativeApiConfig => ({
  enabled: false,
  base_url: 'https://mjapi.example.test',
  key: '',
  model_4p: '',
  model_3p: '',
  proxy_enabled: false,
  proxy: '',
  react_timeout_ms: 3000,
  ...patch,
})

const KEY_PLACEHOLDER = '••••••••••••••••••••••••••••••••'

/**
 * Render the (fully controlled) component behind a state holder, the way the
 * Bots page and the Setup wizard both drive it. `latest()` reads back what the
 * component asked its owner to store.
 *
 * Everything below the enable switch is collapsed while MJOT is off, so by
 * default the helper flips the switch first — with no key configured that
 * flip is local (no server call) and just reveals the fields.
 */
function renderFields(initial: NativeApiConfig, { expand = true }: { expand?: boolean } = {}) {
  let current = initial
  function Host() {
    const [api, setApi] = useState(initial)
    current = api
    return <NativeApiFields value={api} onChange={setApi} />
  }
  render(<Host />)
  if (expand && !initial.enabled) {
    fireEvent.click(screen.getByRole('switch'))
  }
  return {
    keyInput: () => screen.getByPlaceholderText(KEY_PLACEHOLDER),
    latest: () => current,
  }
}

describe('NativeApiFields — entering a key', () => {
  beforeEach(() => {
    invoke.mockReset()
  })

  it('collapses the settings while off and reveals them when switched on', () => {
    const { latest } = renderFields(base(), { expand: false })

    // Off → only the enable switch row; no key field, no server calls.
    expect(screen.queryByPlaceholderText(KEY_PLACEHOLDER)).toBeNull()
    expect(invoke).not.toHaveBeenCalled()

    // Flipping the switch with no key configured reveals the fields without
    // querying anything.
    fireEvent.click(screen.getByRole('switch'))
    expect(screen.getByPlaceholderText(KEY_PLACEHOLDER)).toBeTruthy()
    expect(latest().enabled).toBe(true)
    expect(invoke).not.toHaveBeenCalled()
  })

  it('keeps cloud inference on and fills empty model slots on a complete key', async () => {
    invoke.mockResolvedValueOnce([
      { id: 'mortal-4p', game: '4p', desc: '' },
      { id: 'mortal-3p', game: '3p', desc: '' },
    ])
    const { keyInput, latest } = renderFields(base())

    fireEvent.change(keyInput(), { target: { value: FULL_KEY } })

    await waitFor(() => expect(latest().model_4p).toBe('mortal-4p'))
    expect(latest().enabled).toBe(true)
    expect(latest().key).toBe(FULL_KEY)
    expect(latest().model_3p).toBe('mortal-3p')
    expect(invoke).toHaveBeenCalledWith('native_api_models', {
      baseUrl: 'https://mjapi.example.test',
      proxy: '',
      key: FULL_KEY,
    })
  })

  it('never clobbers a model the user already chose', async () => {
    invoke.mockResolvedValueOnce([
      { id: 'mortal-4p', game: '4p', desc: '' },
      { id: 'mortal-3p', game: '3p', desc: '' },
    ])
    const { keyInput, latest } = renderFields(base({ model_4p: 'my-pick' }))

    fireEvent.change(keyInput(), { target: { value: FULL_KEY } })

    await waitFor(() => expect(latest().model_3p).toBe('mortal-3p'))
    expect(latest().model_4p).toBe('my-pick')
  })

  it('stores a partial key without calling the server', () => {
    const { keyInput, latest } = renderFields(base())

    fireEvent.change(keyInput(), { target: { value: FULL_KEY.slice(0, 20) } })

    expect(latest().key).toBe(FULL_KEY.slice(0, 20))
    expect(invoke).not.toHaveBeenCalled()
  })

  it('fills nothing and surfaces the reason when the server rejects the key', async () => {
    invoke.mockRejectedValueOnce('401 Unauthorized: unknown key')
    const { keyInput, latest } = renderFields(base())

    fireEvent.change(keyInput(), { target: { value: FULL_KEY } })

    await waitFor(() => screen.getByText(/401 Unauthorized: unknown key/))
    // The key is stored (often a half-paste the user will fix), but no model
    // slot was filled from a rejected key. The Save-side check
    // (`checkApiBeforeSave`) is what blocks persisting a broken enabled
    // config.
    expect(latest().key).toBe(FULL_KEY)
    expect(latest().model_4p).toBe('')
  })

  it('retries a rejected key on the next edit, but adopts a good one only once', async () => {
    invoke
      .mockRejectedValueOnce('401 Unauthorized: unknown key')
      .mockResolvedValueOnce([{ id: 'mortal-4p', game: '4p', desc: '' }])
    const { keyInput, latest } = renderFields(base())

    // A key the server rejects must not stay silently unadopted forever:
    // fixing the field (here, retyping the last character) has to ask again.
    fireEvent.change(keyInput(), { target: { value: FULL_KEY } })
    await waitFor(() => screen.getByText(/401 Unauthorized/))
    fireEvent.change(keyInput(), { target: { value: FULL_KEY.slice(0, -1) } })
    fireEvent.change(keyInput(), { target: { value: FULL_KEY } })
    await waitFor(() => expect(latest().model_4p).toBe('mortal-4p'))
    expect(invoke).toHaveBeenCalledTimes(2)

    // Once adopted, editing away and back does not re-query the same key.
    fireEvent.change(keyInput(), { target: { value: FULL_KEY.slice(0, -1) } })
    fireEvent.change(keyInput(), { target: { value: FULL_KEY } })
    expect(invoke).toHaveBeenCalledTimes(2)
  })

  it('does not query the server when no base URL is set', () => {
    const { keyInput, latest } = renderFields(base({ base_url: '' }))

    fireEvent.change(keyInput(), { target: { value: FULL_KEY } })

    expect(latest().key).toBe(FULL_KEY)
    expect(invoke).not.toHaveBeenCalled()
  })
})
