//! `noren` command line: palettes and transactional profiles.

use std::path::PathBuf;
use std::process::ExitCode;

use noren::Color;
use noren::palette::{Contrast, Mode, Palette, PaletteSpec, export};
use noren::rice::{self, Journal, Profile, SystemStore, describe};

const USAGE: &str = "\
noren: Windows ricing, transactional

usage:
  noren palette <#accent> [--light] [--high] [--export terminal|vscode|css|kde|firefox]
  noren diff <profile.toml>        what apply would change
  noren apply <profile.toml>       apply as one transaction
  noren rollback [--force]         undo the newest transaction
  noren log                        list transactions

options:
  --journal <dir>                  journal location (default: %LOCALAPPDATA%\\noren\\journal)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("noren: {e}");
            ExitCode::FAILURE
        }
    }
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn positional(args: &[String]) -> Option<&str> {
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
        } else if a == "--journal" || a == "--export" {
            skip = true;
        } else if !a.starts_with("--") {
            return Some(a);
        }
    }
    None
}

fn journal(args: &[String]) -> Result<Journal, String> {
    let dir = match option(args, "--journal") {
        Some(d) => PathBuf::from(d),
        None => Journal::default_dir().ok_or("cannot find a journal directory, pass --journal")?,
    };
    Journal::open(&dir).map_err(|e| format!("{}: {e}", dir.display()))
}

fn run(args: &[String]) -> Result<(), String> {
    let Some((command, rest)) = args.split_first() else {
        print!("{USAGE}");
        return Ok(());
    };
    match command.as_str() {
        "palette" => palette(rest),
        "diff" | "apply" => {
            let path = positional(rest).ok_or("missing profile path")?;
            let profile = Profile::load(path.as_ref())?;
            let mut store = SystemStore::default();
            let changes = profile.changes(&store).map_err(|e| e.to_string())?;
            if command == "diff" {
                let entries = rice::plan(&store, &changes).map_err(|e| e.to_string())?;
                if entries.is_empty() {
                    println!("up to date");
                }
                for e in entries {
                    println!(
                        "{:<22} {}\n{:<22} {} -> {}",
                        e.label,
                        e.slot,
                        "",
                        describe(&e.slot, e.before.as_deref()),
                        describe(&e.slot, e.after.as_deref())
                    );
                }
                return Ok(());
            }
            let journal = journal(rest)?;
            match rice::apply(&mut store, &journal, &profile.name, &changes).map_err(|e| e.to_string())? {
                Some(t) => println!(
                    "applied #{} ({} changes); undo with `noren rollback`",
                    t.id,
                    t.entries.len()
                ),
                None => println!("up to date"),
            }
            Ok(())
        }
        "rollback" => {
            let journal = journal(rest)?;
            let report = rice::rollback(&mut SystemStore::default(), &journal, flag(rest, "--force"))
                .map_err(|e| e.to_string())?;
            let Some(id) = report.transaction else {
                println!("nothing to roll back");
                return Ok(());
            };
            println!("rolled back #{id}: {} restored", report.restored.len());
            for slot in &report.conflicts {
                println!("  kept {slot}: changed since apply (use --force to overwrite)");
            }
            Ok(())
        }
        "log" => {
            for t in journal(rest)?.list().map_err(|e| e.to_string())? {
                println!(
                    "#{:<4} {:<11} {:<20} {} changes",
                    t.id,
                    format!("{:?}", t.status),
                    t.profile,
                    t.entries.len()
                );
            }
            Ok(())
        }
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    }
}

fn palette(args: &[String]) -> Result<(), String> {
    let raw = positional(args).ok_or("missing accent color")?;
    let accent = Color::from_hex(raw).ok_or_else(|| format!("bad color {raw:?}, want #rrggbb"))?;
    let spec = PaletteSpec {
        mode: if flag(args, "--light") {
            Mode::Light
        } else {
            Mode::Dark
        },
        contrast: if flag(args, "--high") {
            Contrast::High
        } else {
            Contrast::Standard
        },
        ..PaletteSpec::new(accent)
    };
    let p = Palette::generate(&spec);
    match option(args, "--export") {
        None => print_swatches(&p),
        Some("terminal") => println!("{:#}", export::windows_terminal_scheme(&p, "noren")),
        Some("vscode") => {
            let map: serde_json::Map<String, serde_json::Value> = export::vscode_colors(&p)
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect();
            println!(
                "{:#}",
                serde_json::json!({ "workbench.colorCustomizations": map })
            );
        }
        Some("css") => print!("{}", export::css_variables(&p, ":root")),
        Some("kde") => print!("{}", export::kdeglobals(&p)),
        Some("firefox") => print!("{}", export::firefox_user_chrome(&p)),
        Some(other) => return Err(format!("unknown export {other:?}")),
    }
    Ok(())
}

fn print_swatches(p: &Palette) {
    let swatch = |c: Color| {
        let [r, g, b, _] = c.to_rgba8();
        format!("\x1b[48;2;{r};{g};{b}m    \x1b[0m")
    };
    for (name, ramp) in [
        ("neutral", &p.neutral),
        ("accent", &p.accent),
        ("positive", &p.positive),
        ("warning", &p.warning),
        ("negative", &p.negative),
    ] {
        let row: String = ramp.0.iter().map(|c| swatch(*c)).collect();
        println!(
            "{name:<9}{row}  9={} 12={}",
            ramp.step(9).to_hex(),
            ramp.step(12).to_hex()
        );
    }
    let ansi: String = p.ansi.iter().map(|c| swatch(*c)).collect();
    println!("{:<9}{ansi}", "ansi");
    let lc = noren::palette::apca(p.text(), p.background());
    println!("text on background: APCA Lc {lc:.1}");
}
