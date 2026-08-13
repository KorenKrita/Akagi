import { compareVersions } from '@/lib/appVersion'
import type { AnnouncementEntry } from './entries'

/**
 * With no recorded baseline we can't tell a fresh install from an
 * upgrade out of a pre-announcements build, so both get the same gentle
 * default: the newest few entries instead of the whole history.
 */
export const MAX_FRESH_ENTRIES = 3

/**
 * Entries visible to this build, in array (newest-first) order.
 * Version-tagged entries newer than the running build are hidden;
 * version-less product news is always eligible.
 */
export function eligibleEntries(
  entries: readonly AnnouncementEntry[],
  currentVersion: string,
): AnnouncementEntry[] {
  return entries.filter(
    (e) => e.version === undefined || compareVersions(e.version, currentVersion) <= 0,
  )
}

/**
 * What the launch dialog should show: every eligible entry dated after
 * the last-seen baseline (so skip-level updates replay the announcements
 * in between), or the newest `MAX_FRESH_ENTRIES` when there is no
 * baseline. ISO dates order lexically, so plain string compare works.
 */
export function selectUnseenEntries(
  entries: readonly AnnouncementEntry[],
  lastSeenDate: string | null,
  currentVersion: string,
): AnnouncementEntry[] {
  const eligible = eligibleEntries(entries, currentVersion)
  if (lastSeenDate === null) return eligible.slice(0, MAX_FRESH_ENTRIES)
  return eligible.filter((e) => e.date > lastSeenDate)
}
