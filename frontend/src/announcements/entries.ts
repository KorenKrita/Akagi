import type { LucideIcon } from 'lucide-react'
import { AppWindow, Bot, CloudCog, CreditCard, Gamepad2, Sparkles, Zap } from 'lucide-react'

import { AKAGIMS_DOWNLOAD_URL } from '@/lib/external'
import akagimsScreenshot from '@/assets/akagims-fullauto.jpg'

/** One feature highlight inside an announcement's expanded view. */
export type AnnouncementFeature = {
  icon: LucideIcon
  /**
   * i18n leaf name: the dialog reads
   * `announcements.entries.<id>.<key>_title` and `…_desc`.
   */
  key: string
}

export type AnnouncementEntry = {
  /** Stable i18n slug: strings live under `announcements.entries.<id>`. */
  id: string
  /**
   * Publish date, ISO `YYYY-MM-DD`. Must be unique and strictly
   * descending down the array — it doubles as the "which announcements
   * has the user seen" ordering.
   */
  date: string
  /**
   * For release announcements: the exact Cargo.toml package version.
   * Entries whose version is newer than the running build are hidden
   * (an entry authored ahead of an upcoming release stays invisible
   * until that build ships). Product news entries omit this.
   */
  version?: string
  /** Optional bundled image shown expanded; requires `<id>.image_alt`. */
  image?: string
  /** Optional external action URL; requires `<id>.link_label`. */
  link?: string
  features: AnnouncementFeature[]
}

/**
 * All in-app announcements, newest first. Add an entry (plus locale
 * strings in all four i18n resources) for every release BEFORE tagging
 * it — the release tagging script refuses to tag a version that has no
 * committed entry here. See README.md in this directory for the workflow.
 */
export const ANNOUNCEMENTS: AnnouncementEntry[] = [
  {
    id: 'v3_5_0',
    date: '2026-08-12',
    version: '3.5.0',
    features: [
      { icon: Sparkles, key: 'mjot' },
      { icon: CreditCard, key: 'checkout' },
      { icon: CloudCog, key: 'health' },
    ],
  },
  {
    id: 'akagims',
    date: '2026-08-09',
    image: akagimsScreenshot,
    link: AKAGIMS_DOWNLOAD_URL,
    features: [
      { icon: Gamepad2, key: 'majsoul' },
      { icon: AppWindow, key: 'embedded' },
      { icon: Bot, key: 'fullauto' },
      { icon: Zap, key: 'zero_setup' },
    ],
  },
]
