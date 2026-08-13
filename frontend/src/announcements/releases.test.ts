import { describe, expect, it } from 'vitest'

import { compareVersions } from '@/lib/appVersion'
import { releaseSlug, RELEASES } from './releases'

import en from '@/i18n/resources/en.json'
import ja from '@/i18n/resources/ja.json'
import zhTW from '@/i18n/resources/zh-TW.json'
import zhCN from '@/i18n/resources/zh-CN.json'

const LOCALES = { en, ja, 'zh-TW': zhTW, 'zh-CN': zhCN } as const

type AnnouncementsBlock = {
  whats_new: Record<string, string>
  releases: Record<string, Record<string, string>>
}

function announcementsOf(locale: keyof typeof LOCALES): AnnouncementsBlock {
  return (LOCALES[locale] as { announcements: AnnouncementsBlock }).announcements
}

describe('releaseSlug', () => {
  it('turns version separators into i18next-safe underscores', () => {
    expect(releaseSlug('3.5.0')).toBe('v3_5_0')
    expect(releaseSlug('3.0.0-8')).toBe('v3_0_0_8')
  })
})

describe('RELEASES data', () => {
  it('is non-empty', () => {
    expect(RELEASES.length).toBeGreaterThan(0)
  })

  it('is sorted newest first with no duplicate versions', () => {
    for (let i = 1; i < RELEASES.length; i++) {
      expect(
        compareVersions(RELEASES[i - 1].version, RELEASES[i].version),
        `${RELEASES[i - 1].version} should be newer than ${RELEASES[i].version}`,
      ).toBeGreaterThan(0)
    }
  })

  it('uses ISO dates', () => {
    for (const e of RELEASES) {
      expect(e.date, `date of ${e.version}`).toMatch(/^\d{4}-\d{2}-\d{2}$/)
      expect(Number.isNaN(new Date(`${e.date}T00:00:00`).getTime())).toBe(false)
    }
  })

  it('has at least one feature per entry', () => {
    for (const e of RELEASES) {
      expect(e.features.length, `features of ${e.version}`).toBeGreaterThan(0)
    }
  })

  it('has every feature string in every locale', () => {
    for (const locale of Object.keys(LOCALES) as (keyof typeof LOCALES)[]) {
      const block = announcementsOf(locale)
      for (const e of RELEASES) {
        const slug = releaseSlug(e.version)
        const strings = block.releases[slug]
        expect(strings, `${locale}: announcements.releases.${slug}`).toBeTruthy()
        for (const f of e.features) {
          for (const leaf of [`${f.key}_title`, `${f.key}_desc`]) {
            const value = strings[leaf]
            expect(
              typeof value === 'string' && value.length > 0,
              `${locale}: announcements.releases.${slug}.${leaf}`,
            ).toBe(true)
          }
        }
      }
    }
  })

  it('has the dialog UI strings in every locale', () => {
    const needed = ['title', 'intro', 'got_it', 'all_releases', 'settings_button']
    for (const locale of Object.keys(LOCALES) as (keyof typeof LOCALES)[]) {
      const block = announcementsOf(locale)
      for (const key of needed) {
        expect(
          typeof block.whats_new[key] === 'string' && block.whats_new[key].length > 0,
          `${locale}: announcements.whats_new.${key}`,
        ).toBe(true)
      }
    }
  })
})
