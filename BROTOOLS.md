# brotools fork

BroTools is a personal fork of [SoundCraft](https://github.com/storytold/soundcraft). It tracks
upstream closely and adds branding, themes and fixes on top. Keep the fork's diff small: it is
what you resolve on every upstream merge.

## Remotes

| Remote | URL | Use |
|---|---|---|
| `origin` | `github.com/mattpetters/brotools` | this fork |
| `upstream` | `github.com/storytold/soundcraft` | fetch only (push URL is `DISABLED`) |

## Versions: `<upstream>-bro.<n>`

The version is `<upstream semver>-bro.<n>`, for example `0.13.1-bro.1`, then `0.13.1-bro.2` for
your next release on the same upstream base. `n` goes back to 1 when you merge a new upstream release.
`bro.<n>` is a semver pre-release tag, so `bro.10` sorts after `bro.9`. The macOS Info.plist gets
only the numeric `X.Y.Z` part (`packaging/macos/package.sh` strips the tag).

Semver sorts `0.13.1-bro.1` *below* plain `0.13.1`. That is why the startup update check is off
(`apps/soundcraft/src/main.rs`): the updater reads upstream SoundCraft releases and would
offer to replace BroTools with stock SoundCraft. Don't use Help ▸ Check for Updates.

```sh
cargo xtask version                       # print the version
cargo xtask version set 0.13.1-bro.2      # next brotools iteration
```

## Syncing with upstream

```sh
git fetch upstream
git switch -c sync/upstream-X.Y.Z main
git merge upstream/main                   # conflicts land only in files the fork changed
cargo xtask version set X.Y.Z-bro.1       # X.Y.Z = upstream's new version
cargo xtask ci
```

Prefer merging over rebasing: the fork is long-lived and public, and merges keep its history stable.

## What the fork changes

| Change | Files |
|---|---|
| App name `BroTools`, bundle id `com.mattpetters.brotools` (installs next to SoundCraft) | `packaging/macos/Info.plist.in`, `packaging/macos/package.sh` |
| Window title, macOS app menu, startup update check off | `apps/soundcraft/src/main.rs`, `apps/soundcraft/src/native_menu.rs` |
| App icon (edit `icon.svg`, then run `packaging/icons.sh`) | `assets/app-icon/`, `ATTRIBUTION.md` |
| README title | `README.md` |

Crate names, binaries, the `.scraft` format and its UTI stay as upstream named them, so sessions
open in both apps and merges stay small.
