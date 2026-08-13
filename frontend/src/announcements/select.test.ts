import { describe, expect, it } from 'vitest'
import { Sparkles } from 'lucide-react'

import type { ReleaseEntry } from './releases'
import { MAX_FRESH_ENTRIES, releasedEntries, selectUnseenReleases } from './select'

function entry(version: string): ReleaseEntry {
  return { version, date: '2026-01-01', features: [{ icon: Sparkles, key: 'x' }] }
}

// Deliberately unsorted so the selectors' own ordering is exercised.
const ENTRIES: ReleaseEntry[] = [
  entry('3.4.0'),
  entry('3.6.0'),
  entry('3.3.0'),
  entry('3.5.0'),
  entry('3.2.0'),
]

const versions = (xs: ReleaseEntry[]) => xs.map((e) => e.version)

describe('releasedEntries', () => {
  it('sorts newest first and hides entries newer than the running build', () => {
    // A 3.6.0 entry added ahead of the upcoming release must not show on 3.5.0.
    expect(versions(releasedEntries(ENTRIES, '3.5.0'))).toEqual([
      '3.5.0',
      '3.4.0',
      '3.3.0',
      '3.2.0',
    ])
  })
})

describe('selectUnseenReleases', () => {
  it('replays every skipped version between the baseline and the build', () => {
    expect(versions(selectUnseenReleases(ENTRIES, '3.2.0', '3.5.0'))).toEqual([
      '3.5.0',
      '3.4.0',
      '3.3.0',
    ])
  })

  it('shows nothing when the baseline is current', () => {
    expect(selectUnseenReleases(ENTRIES, '3.5.0', '3.5.0')).toEqual([])
  })

  it('shows nothing when the baseline is newer (downgraded build)', () => {
    expect(selectUnseenReleases(ENTRIES, '3.6.0', '3.5.0')).toEqual([])
  })

  it('caps the no-baseline case at the newest few entries', () => {
    const fresh = selectUnseenReleases(ENTRIES, null, '3.6.0')
    expect(fresh.length).toBe(MAX_FRESH_ENTRIES)
    expect(versions(fresh)).toEqual(['3.6.0', '3.5.0', '3.4.0'])
  })

  it('still hides not-yet-released entries in the no-baseline case', () => {
    expect(versions(selectUnseenReleases(ENTRIES, null, '3.3.0'))).toEqual([
      '3.3.0',
      '3.2.0',
    ])
  })
})
