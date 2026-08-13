import { describe, expect, it } from 'vitest'
import { Sparkles } from 'lucide-react'

import type { AnnouncementEntry } from './entries'
import { eligibleEntries, MAX_FRESH_ENTRIES, selectUnseenEntries } from './select'

function entry(id: string, date: string, version?: string): AnnouncementEntry {
  return { id, date, version, features: [{ icon: Sparkles, key: 'x' }] }
}

// Newest first, like the real data. `news` has no version — product news
// (e.g. the AkagiMS announcement) must stay eligible on every build.
const ENTRIES: AnnouncementEntry[] = [
  entry('v3_6_0', '2026-09-01', '3.6.0'),
  entry('v3_5_0', '2026-08-12', '3.5.0'),
  entry('news', '2026-08-09'),
  entry('v3_4_0', '2026-07-01', '3.4.0'),
  entry('v3_3_0', '2026-06-01', '3.3.0'),
]

const ids = (xs: AnnouncementEntry[]) => xs.map((e) => e.id)

describe('eligibleEntries', () => {
  it('hides version-tagged entries newer than the running build', () => {
    // A 3.6.0 entry authored ahead of the upcoming release must not show on 3.5.0.
    expect(ids(eligibleEntries(ENTRIES, '3.5.0'))).toEqual([
      'v3_5_0',
      'news',
      'v3_4_0',
      'v3_3_0',
    ])
  })

  it('always keeps version-less news entries', () => {
    expect(ids(eligibleEntries(ENTRIES, '3.3.0'))).toContain('news')
  })
})

describe('selectUnseenEntries', () => {
  it('replays every entry the user skipped over', () => {
    expect(ids(selectUnseenEntries(ENTRIES, '2026-07-01', '3.6.0'))).toEqual([
      'v3_6_0',
      'v3_5_0',
      'news',
    ])
  })

  it('shows nothing when the baseline is current', () => {
    expect(selectUnseenEntries(ENTRIES, '2026-09-01', '3.6.0')).toEqual([])
  })

  it('shows nothing new on a downgraded build', () => {
    // Baseline from a 3.6.0 run; now running 3.5.0.
    expect(selectUnseenEntries(ENTRIES, '2026-09-01', '3.5.0')).toEqual([])
  })

  it('caps the no-baseline case at the newest few entries', () => {
    const fresh = selectUnseenEntries(ENTRIES, null, '3.6.0')
    expect(fresh.length).toBe(MAX_FRESH_ENTRIES)
    expect(ids(fresh)).toEqual(['v3_6_0', 'v3_5_0', 'news'])
  })

  it('still hides not-yet-released entries in the no-baseline case', () => {
    expect(ids(selectUnseenEntries(ENTRIES, null, '3.5.0'))).toEqual([
      'v3_5_0',
      'news',
      'v3_4_0',
    ])
  })
})
