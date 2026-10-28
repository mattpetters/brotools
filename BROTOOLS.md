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

## Build and install

```sh
packaging/macos/install-brotools.sh --dry-run   # show what would change
packaging/macos/install-brotools.sh             # build, then replace /Applications/BroTools.app
```

It builds for this Mac, then replaces BroTools in `/Applications` only if the build differs. A
running BroTools is quit first; if it doesn't quit (unsaved changes), the script stops and changes
nothing. The old app is restored if the swap fails. The Dock's BroTools tile is pointed at the new
app (added if missing, via `dockutil`), and BroTools is relaunched if it was running.

The build records the upstream commit it is based on, and the Edit window's status bar shows
"N commits behind upstream" (asked from GitHub once per launch). `cargo run` builds leave it out.

## What the fork changes

| Change | Files |
|---|---|
| App name `BroTools`, bundle id `com.mattpetters.brotools` (installs next to SoundCraft) | `packaging/macos/Info.plist.in`, `packaging/macos/package.sh` |
| Window title, macOS app menu, startup update check off | `apps/soundcraft/src/main.rs`, `apps/soundcraft/src/native_menu.rs` |
| App icon (edit `icon.svg`, then run `packaging/icons.sh`) | `assets/app-icon/`, `ATTRIBUTION.md` |
| README title | `README.md` |
| Status bar "N commits behind upstream" | `crates/ui-egui/src/upstream_status.rs`, `edit_window.rs`, `lib.rs` |
| Build-and-install script | `packaging/macos/install-brotools.sh` |

Crate names, binaries, the `.scraft` format and its UTI stay as upstream named them, so sessions
open in both apps and merges stay small.

## Contributing back to upstream

Fork-only work (branding, the upstream counter, install script, personal QOL) stays here. Fixes and
features that would help every SoundCraft user can go upstream as their own PRs. To keep that easy:

- Commit upstream-worthy changes separately from fork-only ones, never mixed in one commit.
- To contribute, branch from `upstream/main`, cherry-pick the commits, run `cargo xtask ci`, and
  open the PR from this fork to `storytold/soundcraft`. Follow upstream's `AGENTS.md` (and
  `storytold/craftrules`); add yourself to `contributors/people.toml` with the first PR.
- When upstream merges it, the next sync dedupes it: drop the row below.

| Change | Commit(s) | Upstream target | Status |
|---|---|---|---|
| Adapt #568 to the current folder picker and `Env` API | `6b4a79b` | comment or commit on storytold/soundcraft#568 | not sent |
| Scan child reads its arguments with `args_os` (launch panic on non-Unicode args, ast-grep `args-os`) | `753164f` | storytold/soundcraft#568 | not sent |
