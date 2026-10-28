#!/usr/bin/env bash
# brotools: build BroTools for this Mac and replace a stale copy in /Applications and the Dock.
#
# Usage: packaging/macos/install-brotools.sh [--dry-run] [--skip-build]
#
#   --dry-run     print what would change, change nothing (the build still runs unless --skip-build)
#   --skip-build  install the last build in target/macos-package
#
# Steps: build (package.sh, this Mac's arch) with the upstream base commit baked in for the status
# bar's "N commits behind upstream"; quit a running BroTools (aborts if it doesn't quit, so unsaved
# work is never lost); swap the new app into /Applications (the old one is restored on failure);
# point the Dock's BroTools tile at it, adding one if missing; relaunch if it was running.
#
# Env: APPS_DIR (default /Applications), DOCK_PLIST (default: the user's Dock; set it to a copy
# to try the Dock step without touching the real one).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
APPS_DIR="${APPS_DIR:-/Applications}"
BUNDLE_ID=com.mattpetters.brotools
NEW="$ROOT/target/macos-package/BroTools.app"
DEST="$APPS_DIR/BroTools.app"
LSREGISTER=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

DRY=0
SKIP_BUILD=0
for a in "$@"; do
  case "$a" in
    --dry-run) DRY=1 ;;
    --skip-build) SKIP_BUILD=1 ;;
    -h | --help) sed -n '2,16p' "$0"; exit 0 ;;
    *) echo "unknown argument: $a" >&2; exit 2 ;;
  esac
done

run() {
  if [[ "$DRY" = 1 ]]; then echo "would run: $*"; else "$@"; fi
}

# ---- build -------------------------------------------------------------------------------------
if [[ "$SKIP_BUILD" = 0 ]]; then
  if git -C "$ROOT" remote get-url upstream >/dev/null 2>&1; then
    git -C "$ROOT" fetch -q upstream main
    BROTOOLS_UPSTREAM_BASE="$(git -C "$ROOT" merge-base HEAD upstream/main)"
    export BROTOOLS_UPSTREAM_BASE
    echo "==> upstream base $BROTOOLS_UPSTREAM_BASE ($(git -C "$ROOT" rev-list --count HEAD..upstream/main) commits behind now)"
  else
    echo "warning: no 'upstream' remote; the status bar won't show commits behind upstream" >&2
  fi
  arch="$(uname -m)"
  [[ "$arch" = arm64 ]] && arch=aarch64
  "$ROOT/packaging/macos/package.sh" --arch "$arch"
fi
[[ -d "$NEW" ]] || { echo "error: no build at $NEW" >&2; exit 1; }

# ---- /Applications -----------------------------------------------------------------------------
if [[ -d "$DEST" ]] && cmp -s "$NEW/Contents/MacOS/BroTools" "$DEST/Contents/MacOS/BroTools" \
  && cmp -s "$NEW/Contents/Info.plist" "$DEST/Contents/Info.plist"; then
  echo "==> $DEST is already this build"
  replaced=0
else
  replaced=1
fi

was_running=0
if [[ "$replaced" = 1 ]] && pgrep -qf "$DEST/Contents/MacOS/BroTools"; then
  was_running=1
  echo "==> quitting the running BroTools"
  if [[ "$DRY" = 0 ]]; then
    osascript -e "tell application id \"$BUNDLE_ID\" to quit" >/dev/null 2>&1 || true
    for _ in $(seq 60); do
      pgrep -qf "$DEST/Contents/MacOS/BroTools" || break
      sleep 0.5
    done
    if pgrep -qf "$DEST/Contents/MacOS/BroTools"; then
      echo "error: BroTools is still running (unsaved changes?). Quit it, then run this again." >&2
      exit 1
    fi
  else
    echo "would quit BroTools (and stop here if it doesn't quit within 30 s)"
  fi
fi

if [[ "$replaced" = 1 ]]; then
  echo "==> installing $(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$NEW/Contents/Info.plist") to $DEST"
  staged="$APPS_DIR/.BroTools.app.new-$$"
  backup="$APPS_DIR/.BroTools.app.old-$$"
  restore() {
    echo "error: install failed; restoring the previous BroTools" >&2
    rm -rf "$staged"
    if [[ -d "$backup" ]]; then
      rm -rf "$DEST"
      mv "$backup" "$DEST"
    fi
  }
  # Stage a full copy first, so the swap is two renames on the same volume.
  run ditto "$NEW" "$staged"
  if [[ "$DRY" = 0 ]]; then
    trap restore ERR
    [[ -d "$DEST" ]] && mv "$DEST" "$backup"
    mv "$staged" "$DEST"
    trap - ERR
    rm -rf "$backup"
  else
    [[ -d "$DEST" ]] && echo "would run: mv $DEST $backup"
    echo "would run: mv $staged $DEST"
    [[ -d "$DEST" ]] && echo "would run: rm -rf $backup"
  fi
  # Re-read the bundle, so Finder and the Dock show the new icon and version.
  run "$LSREGISTER" -f "$DEST"
fi

# ---- Dock --------------------------------------------------------------------------------------
if command -v dockutil >/dev/null; then
  dock=()
  [[ -n "${DOCK_PLIST:-}" ]] && dock=("$DOCK_PLIST")
  # --replacing keeps the tile's position, or adds the tile at the end if there's none.
  echo "==> pointing the Dock's BroTools tile at $DEST"
  run dockutil --add "$DEST" --replacing BroTools --no-restart ${dock[@]+"${dock[@]}"}
  [[ -z "${DOCK_PLIST:-}" ]] && run killall Dock
else
  echo "warning: dockutil not found (brew install dockutil); Dock left as is" >&2
fi

if [[ "$was_running" = 1 ]]; then
  echo "==> relaunching BroTools"
  run open "$DEST"
fi
echo "==> done"
