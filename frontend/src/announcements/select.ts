import { compareVersions } from '@/lib/appVersion'
import type { ReleaseEntry } from './releases'

/**
 * With no recorded baseline we can't tell a fresh install from an
 * upgrade out of a pre-announcements build, so both get the same gentle
 * default: the newest few entries instead of the whole history.
 */
export const MAX_FRESH_ENTRIES = 3

/** Entries for this build, newest first. Entries for versions newer than
 *  the running build (added ahead of an upcoming release) are hidden. */
export function releasedEntries(
  entries: readonly ReleaseEntry[],
  currentVersion: string,
): ReleaseEntry[] {
  return [...entries]
    .filter((e) => compareVersions(e.version, currentVersion) <= 0)
    .sort((a, b) => compareVersions(b.version, a.version))
}

/**
 * What the launch dialog should show: every entry newer than the
 * last-seen baseline (so skip-level updates replay the announcements in
 * between), or the newest `MAX_FRESH_ENTRIES` when there is no baseline.
 */
export function selectUnseenReleases(
  entries: readonly ReleaseEntry[],
  lastSeenVersion: string | null,
  currentVersion: string,
): ReleaseEntry[] {
  const eligible = releasedEntries(entries, currentVersion)
  if (lastSeenVersion === null) return eligible.slice(0, MAX_FRESH_ENTRIES)
  return eligible.filter((e) => compareVersions(e.version, lastSeenVersion) > 0)
}
