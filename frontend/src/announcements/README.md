# Release announcements

Bundled "What's new" entries shown by `<ReleaseAnnouncementDialog />` after
an update. On launch the dialog compares the running version against the
persisted `akagi.announcement.releases.lastSeen` baseline and shows every
entry in between (skip-level updates replay all missed versions); with no
baseline (fresh install / first run of the build that introduced this) it
shows the newest few. Settings → Updates → "What's new" reopens the full
history at any time.

## Adding an announcement for a release (pre-release checklist)

Do this **before** tagging the release — the maintainer's release tagging
script (a local script, not part of this repo) refuses to tag a version
that has no committed entry here.

1. **`releases.ts`** — prepend an entry to `RELEASES` (newest first):

   ```ts
   {
     version: '3.6.0',            // exact version you are about to tag
     date: '2026-08-20',          // planned publish date, ISO
     features: [
       { icon: Rocket, key: 'tenhou_autoplay' },
       // 2–4 highlights is the sweet spot; icons come from lucide-react
     ],
   },
   ```

2. **Locale strings** — for each feature `key`, add `<key>_title` and
   `<key>_desc` under `announcements.releases.<slug>` in **all four**
   resources (`en.json`, `ja.json`, `zh-TW.json`, `zh-CN.json`), where
   `<slug>` is the version with dots/dashes replaced by underscores and a
   leading `v` — `3.6.0` → `v3_6_0`:

   ```jsonc
   "announcements": {
     "releases": {
       "v3_6_0": {
         "tenhou_autoplay_title": "…",
         "tenhou_autoplay_desc": "…"
       }
     }
   }
   ```

3. **Commit**, then tag the release as usual.

`releases.test.ts` fails if entries are out of order, duplicated, dated
wrongly, or missing any locale string — run `npm test` in `frontend/` to
validate. Keep highlights user-facing (what changed for the player), not a
commit log; the dialog links to GitHub releases for the full notes.

## Files

- `releases.ts` — the data: one `ReleaseEntry` per release, newest first.
- `select.ts` — pure selection logic (which entries a launch should show).
- Store/UI live in `../stores/announcementStore.ts` and
  `../components/ReleaseAnnouncementDialog.tsx`.
