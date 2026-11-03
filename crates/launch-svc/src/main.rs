//! `infinity-launch-svc`: the Windows service that keeps the Infinity
//! Manager reachable from the sim.
//!
//! It listens on the sentinel port ([`svc::SENTINEL_BIND_ADDR`]). An in-sim
//! gauge's relay that cannot reach the manager connects there; the
//! connection itself is the signal, and the service starts
//! `infinity-manager.exe --remote-start` in the active console user's
//! session. It speaks no protocol and holds no credentials: the manager
//! serves the gauges itself, as the signed-in user.
//!
//! Wire- and disk-compatible with v3's service: same name, port, install
//! path and subcommands, so either manager can control either binary.
//!
//! Subcommands, run elevated by the manager (`ShellExecute` with `runas`):
//!
//! - `install` / `install_and_start` / `uninstall` / `start` / `stop`
//! - `update`: stop, replace the installed binary with this one, start
//! - `status`: `{"installed":…,"running":…}` on stdout
//! - `version`: this binary's version on stdout
//! - `--self-relocate <dest> <subcommand>`: copy this exe to `<dest>` and
//!   run the subcommand from there
//!
//! With no arguments it expects to be started by the service control
//! manager.

#![cfg_attr(not(windows), allow(clippy::print_stderr))]

#[cfg(windows)]
mod admin;
#[cfg(windows)]
mod svc;

#[cfg(windows)]
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "infinity-launch-svc is a Windows service; on Linux the manager installs systemd user units instead"
    );
    std::process::ExitCode::FAILURE
}

#[cfg(windows)]
#[allow(clippy::print_stdout, clippy::print_stderr)]
fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        [] => svc::run(),
        ["--self-relocate", dest, sub] => admin::self_relocate_and_exec(dest, sub),
        ["--self-relocate", ..] => Err("--self-relocate needs <dest> <subcommand>".into()),
        ["install"] => admin::install(),
        ["install_and_start"] => admin::install_and_start(),
        ["uninstall"] => admin::uninstall(),
        ["start"] => admin::start(),
        ["stop"] => admin::stop(),
        ["status"] => admin::print_status_json(),
        ["update"] => admin::update(),
        ["version"] => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        [other, ..] => {
            eprintln!("unknown subcommand: {other}");
            return std::process::ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
