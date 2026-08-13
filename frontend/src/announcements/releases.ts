import type { LucideIcon } from 'lucide-react'
import { CloudCog, CreditCard, Sparkles } from 'lucide-react'

/** One feature highlight inside a release announcement. */
export type ReleaseFeature = {
  icon: LucideIcon
  /**
   * i18n leaf name: the dialog reads
   * `announcements.releases.<slug>.<key>_title` and `…_desc`
   * where `<slug>` is `releaseSlug(entry.version)`.
   */
  key: string
}

export type ReleaseEntry = {
  /** Exact Cargo.toml package version of the release this describes. */
  version: string
  /** Publish date, ISO `YYYY-MM-DD`. */
  date: string
  features: ReleaseFeature[]
}

/**
 * i18n namespace slug for a version: dots/dashes are i18next key
 * separators, so `3.5.0` announces under `announcements.releases.v3_5_0`.
 */
export function releaseSlug(version: string): string {
  return 'v' + version.replace(/[.-]/g, '_')
}

/**
 * Release announcements, newest first. Add an entry (plus locale strings
 * in all four i18n resources) for every release BEFORE tagging it — the
 * release tagging script refuses to tag a version that has no committed
 * entry here. See README.md in this directory for the workflow.
 */
export const RELEASES: ReleaseEntry[] = [
  {
    version: '3.5.0',
    date: '2026-08-12',
    features: [
      { icon: Sparkles, key: 'mjot' },
      { icon: CreditCard, key: 'checkout' },
      { icon: CloudCog, key: 'health' },
    ],
  },
]
