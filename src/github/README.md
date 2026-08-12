# GitHub Module

Shared primitives for talking to GitHub Releases, used by `bot::install`
(bot zips) and `updater` (Akagi's own binary). Three files:

- `mod.rs` — release-metadata fetch, streaming downloads (SHA-256 on the
  fly), safe zip extraction, and the fallback iteration (`try_each`)
  behind `fetch_latest_release_mirrored`, `download_with_fallback`, and
  `fetch_text_with_fallback`.
- `mirror.rs` — gh-proxy-style accelerator fallback. Turns one GitHub
  URL plus the `[network]` config into an ordered candidate list, each
  tagged `Source::Direct` or `Source::Mirror`.
- `signing.rs` — minisign verification of release assets. The embedded
  public key matches `minisign.pub` at the repo root; CI signs every
  release zip (see `.github/workflows/release.yml`).

## Trust model

Anything fetched through a mirror is attacker-supplied until verified:

- **App updates** (`updater::apply`): a valid minisign signature is
  mandatory whenever the metadata or the zip came via a mirror. The
  expected trusted comment is *computed* from the claimed version and
  the running platform (`expected_asset_name`), never taken from the
  metadata — so forged metadata pairing a fake newer tag with a genuine
  signed older zip fails verification. `apply_update` also only acts on
  the `UpdateInfo` stashed server-side by the last check
  (`AppState::pending_update`); the webview never hands one back.
- **Bot installs** (`bot::install`): verified when the release ships a
  `.minisig`; otherwise installable with a warning notification when a
  mirror was involved (bots are third-party by design — the user picked
  the repo). A mirror that forges the metadata can omit the signature
  asset, so for third-party bots the signature only hardens the
  direct-metadata + mirrored-download case.
- The SHA-256 digest from release metadata is checked when the metadata
  came direct; it rides the same channel as the download otherwise, so
  it guards against corruption, not tampering.

## Maintaining the mirror list

`mirror::BUILTIN_MIRRORS` is a plain list of accelerator origins. Not
all of them can proxy `api.github.com` (some 403 it) — that's fine, the
refusal is fast and candidate iteration moves on. To vet a new one:

```sh
curl -s -o /dev/null -w '%{http_code}\n' -L -r 0-99 \
  'https://<mirror>/https://github.com/shinkuan/Akagi/releases/download/<tag>/<asset>.zip'
```

Public accelerators churn; the user-facing `github_custom_mirror`
setting exists so a rotten list is not fatal between releases.

## Key rotation

Generate a new passwordless keypair (`rsign generate -W`), replace the
base64 line in `signing.rs::RELEASE_PUBKEY_B64` **and** the tracked
`minisign.pub`, and update the `MINISIGN_SECRET_KEY` repository secret.
Old app builds only accept updates signed by the old key, so ship at
least one release signed with **both** keys' assets if continuity
matters (or accept that old builds fall back to the release page).
