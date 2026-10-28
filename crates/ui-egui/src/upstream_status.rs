//! brotools: the status bar's count of the commits upstream SoundCraft's `main` has that this
//! build doesn't. The build's upstream base commit comes from `BROTOOLS_UPSTREAM_BASE` at compile
//! time (set by `packaging/macos/install-brotools.sh`); without it the count isn't shown.

use std::sync::{Once, OnceLock};

static BEHIND: OnceLock<Option<u64>> = OnceLock::new();
static STARTED: Once = Once::new();

/// "3 commits behind upstream" once GitHub has answered; asked once per launch, off the UI thread.
pub(crate) fn label(ctx: &egui::Context) -> Option<String> {
    let base = option_env!("BROTOOLS_UPSTREAM_BASE").filter(|b| !b.is_empty())?;
    STARTED.call_once(|| {
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let behind = fetch(base).map_err(|e| log::warn!("upstream status: {e}")).ok();
            let _ = BEHIND.set(behind);
            ctx.request_repaint();
        });
    });
    BEHIND.get().copied().flatten().map(describe)
}

fn describe(n: u64) -> String {
    match n {
        0 => "up to date with upstream".into(),
        1 => "1 commit behind upstream".into(),
        n => format!("{n} commits behind upstream"),
    }
}

fn fetch(base: &str) -> Result<u64, String> {
    let http = craft_update::Http::new(concat!("BroTools/", env!("CARGO_PKG_VERSION")))?;
    let url = format!("https://api.github.com/repos/storytold/soundcraft/compare/{base}...main");
    match http.get_api(&url, None)? {
        craft_update::http::Fetched::Body { text, .. } => ahead_by(&text).ok_or_else(|| "no ahead_by in GitHub's answer".into()),
        other => Err(format!("GitHub answered {other:?}")),
    }
}

/// GitHub's compare answer: `ahead_by` counts the commits `main` has beyond the base.
fn ahead_by(json: &str) -> Option<u64> {
    serde_json::from_str::<serde_json::Value>(json).ok()?.get("ahead_by")?.as_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ahead_by_from_a_compare_answer() {
        assert_eq!(ahead_by(r#"{"status":"ahead","ahead_by":7,"behind_by":0}"#), Some(7));
        assert_eq!(ahead_by(r#"{"message":"Not Found"}"#), None);
        assert_eq!(ahead_by("not json"), None);
    }

    #[test]
    fn describes_the_count() {
        assert_eq!(describe(0), "up to date with upstream");
        assert_eq!(describe(1), "1 commit behind upstream");
        assert_eq!(describe(12), "12 commits behind upstream");
    }
}
