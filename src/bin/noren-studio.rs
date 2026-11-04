//! `noren-studio`: the noren GUI. Built with `cargo build --release --features studio`.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    noren::studio::main()
}
