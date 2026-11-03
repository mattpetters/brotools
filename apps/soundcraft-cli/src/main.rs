//! `soundcraft-cli`: SoundCraft without a window. See the library crate for the commands.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

fn main() {
    // `args_os`, not `args`: a non-Unicode argument must reach clap, not panic before anything is printed.
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    // A plugin scan's child process (see below): load one file, answer, exit.
    if let Some(reply) = soundcraft_engine::plugin_scan_child(&args) {
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), "{reply}");
        let _ = std::io::stdout().flush();
        soundcraft_engine::shutdown_plugin_hosts();
        std::process::exit(0);
    }
    // Scans load each plugin file in a copy of this program, so one that aborts cannot end the CLI.
    soundcraft_engine::set_plugin_scanner(std::env::current_exe().ok());
    let code = soundcraft_cli::run(args, &mut std::io::stdout(), &mut std::io::stderr());
    // The subcommand's engine and its plugin instances are gone by now. This is process teardown,
    // so it lives here and not in `run`, which tests call many times in one process.
    soundcraft_engine::shutdown_plugin_hosts();
    std::process::exit(code);
}
