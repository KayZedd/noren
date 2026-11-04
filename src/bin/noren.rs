//! `noren` command line: palettes and transactional profiles.

use std::path::PathBuf;
use std::process::ExitCode;

use noren::Color;
use noren::host::{Entry, MonitorSelect, SurfaceOptions};
use noren::palette::{Contrast, Mode, Palette, PaletteSpec, export};
use noren::rice::{self, Journal, Profile, SystemStore, describe};
use noren::shell::{self, Rules, State, state};

const USAGE: &str = "\
noren: Windows ricing, transactional

usage:
  noren palette <#accent> [--light] [--high] [--export terminal|vscode|css|kde|firefox]
  noren diff <profile.toml>        what apply would change
  noren apply <profile.toml>       apply as one transaction
  noren rollback [--force]         undo the newest transaction
  noren log                        list transactions
  noren run <profile.toml>         hold [[window]] rules, [focus], [xray], chords, hot
                                   corners, [[glow]] and [schedule] until Ctrl+C
  noren card <profile.toml> <out.png>
                                   a PNG of the profile to share [--scale <s>, default 2]
  noren panel <profile.toml>       a top panel with the profile's [[widget]] programs and [media]
                                   [--monitor <n>] [--screenshot <out.png>]
  noren schedule <profile.toml>    today's dark/light switch times from [schedule]
  noren commands <profile.toml>    list the profile's [[command]]s
  noren commands <profile.toml> <name>
                                   run one by name
  noren windows [<profile.toml>]   list windows, and which rules match them
  noren state                      list states
  noren state <name> <ok|fail|warn|busy|clear>
                                   set a state read by border = state:<name>
  noren report [<profile.toml>]    print a compatibility report (no names or paths)
                                   [--open: open it as a GitHub issue to review and submit]
  noren update [--check]           install the newest signed release, or only check

options:
  --journal <dir>                  journal location (default: %LOCALAPPDATA%\\noren\\journal)
  --state-dir <dir>                state location (default: %LOCALAPPDATA%\\noren\\state)
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

/// Arguments that are not flags or option values.
fn positionals(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
        } else if matches!(
            a.as_str(),
            "--journal" | "--export" | "--state-dir" | "--screenshot" | "--monitor" | "--scale"
        ) {
            skip = true;
        } else if !a.starts_with("--") {
            out.push(a.as_str());
        }
    }
    out
}

fn positional(args: &[String]) -> Option<&str> {
    positionals(args).first().copied()
}

fn state_dir(args: &[String]) -> Result<PathBuf, String> {
    match option(args, "--state-dir") {
        Some(d) => Ok(PathBuf::from(d)),
        None => state::default_dir().ok_or_else(|| "cannot find a state directory, pass --state-dir".into()),
    }
}

fn journal(args: &[String]) -> Result<Journal, String> {
    let dir = match option(args, "--journal") {
        Some(d) => PathBuf::from(d),
        None => Journal::default_dir().ok_or("cannot find a journal directory, pass --journal")?,
    };
    Journal::open(&dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// Renders the revealed panel on a virtual 1280x720 monitor after 1.5 s and writes a PNG.
fn screenshot(mut panel: noren::Panel, path: &std::path::Path) -> Result<(), String> {
    use noren::host::sim::Simulator;
    use noren::surface::MonitorInfo;
    use std::time::{Duration, Instant};
    let start = Instant::now();
    let background = panel.theme().alternate_background;
    panel.set_viewport(1280.0, 720.0, 1.0);
    panel.reveal_now();
    let mut sim = Simulator::new(
        MonitorInfo {
            width: 1280.0,
            height: 720.0,
            scale: 1.0,
        },
        background,
    );
    sim.add(Box::new(panel));
    sim.set_pointer(Some((640.0, 20.0)));
    // Long enough for programs that print at once; streaming ones show what they printed.
    while start.elapsed() < Duration::from_millis(1500) {
        sim.tick(Instant::now());
        std::thread::sleep(Duration::from_millis(50));
    }
    let now = Instant::now();
    sim.tick(now);
    sim.render(now).save_png(path).map_err(|e| e.to_string())
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
        "run" => {
            let profile = Profile::load(positional(rest).ok_or("missing profile path")?.as_ref())?;
            let dir = option(rest, "--state-dir").map(PathBuf::from);
            shell::run(&profile, dir).map_err(|e| e.to_string())
        }
        "panel" => {
            let profile = Profile::load(positional(rest).ok_or("missing profile path")?.as_ref())?;
            let panel = profile.panel()?;
            if let Some(out) = option(rest, "--screenshot") {
                return screenshot(panel, out.as_ref());
            }
            let monitor = match option(rest, "--monitor") {
                // 1-based, like `monitor = N` in window rules.
                Some(n) => match n.parse::<usize>() {
                    Ok(i) if i >= 1 => MonitorSelect::Index(i - 1),
                    _ => return Err(format!("bad monitor {n:?}, monitors count from 1")),
                },
                None => MonitorSelect::Primary,
            };
            noren::host::run(vec![Entry::new(panel, SurfaceOptions::panel().on(monitor))])
        }
        "card" => {
            let args = positionals(rest);
            let [profile, out] = args[..] else {
                return Err("usage: noren card <profile.toml> <out.png> [--scale <s>]".into());
            };
            let profile = Profile::load(profile.as_ref())?;
            let scale = match option(rest, "--scale") {
                Some(s) => s.parse::<f32>().map_err(|_| format!("bad scale {s:?}"))?,
                None => 2.0,
            };
            let png = noren::card::render(&profile, scale)?;
            png.save_png(out).map_err(|e| format!("{out}: {e}"))?;
            println!("{out}");
            Ok(())
        }
        "schedule" => {
            let profile = Profile::load(positional(rest).ok_or("missing profile path")?.as_ref())?;
            let config = profile.schedule.as_ref().ok_or("the profile has no [schedule]")?;
            let schedule = shell::schedule::Schedule::new(config)?;
            print!("{}", schedule.report(shell::schedule::Local::now()));
            Ok(())
        }
        "commands" => {
            let path = positional(rest).ok_or("missing profile path")?;
            let profile = Profile::load(path.as_ref())?;
            let commands = profile.commands()?;
            let dir = option(rest, "--state-dir").map(PathBuf::from);
            match positionals(rest)[1..] {
                [] => {
                    for c in &commands.list {
                        let keys = match (&commands.leader, c.keys.is_empty()) {
                            (Some(l), false) => format!("{l}, {}", c.keys_label()),
                            _ => String::new(),
                        };
                        let corner = c.corner.map(|k| k.to_string()).unwrap_or_default();
                        println!("{:<28} {:<24} {corner}", c.name, keys);
                    }
                    Ok(())
                }
                [name] => {
                    let c = commands
                        .list
                        .iter()
                        .find(|c| c.name.eq_ignore_ascii_case(name))
                        .ok_or_else(|| format!("no command named {name:?}"))?;
                    match &c.action {
                        shell::Action::Palette => {
                            let source = toml::to_string(&profile).map_err(|e| e.to_string())?;
                            shell::command::open_palette(&source, dir.as_deref())
                        }
                        action => shell::command::exec(action, dir.as_deref()),
                    }
                    .map_err(|e| e.to_string())
                }
                _ => Err("usage: noren commands <profile.toml> [<name>]".into()),
            }
        }
        "windows" => {
            let rules = match positional(rest) {
                Some(path) => Some(Rules::new(&Profile::load(path.as_ref())?.window)?),
                None => None,
            };
            for w in shell::windows().map_err(|e| e.to_string())? {
                let matched = rules
                    .as_ref()
                    .map(|r| {
                        let ids: Vec<String> = r.matching(&w).iter().map(|i| format!("#{}", i + 1)).collect();
                        format!(
                            "  rules {}",
                            if ids.is_empty() { "-".into() } else { ids.join(",") }
                        )
                    })
                    .unwrap_or_default();
                println!("{:<28} {:<36} {}{matched}", w.exe, w.class, w.title);
            }
            Ok(())
        }
        "state" => {
            let dir = state_dir(rest)?;
            match positionals(rest)[..] {
                [] => {
                    for (name, s) in state::read_all(&dir).map_err(|e| e.to_string())? {
                        println!("{name:<24} {s}");
                    }
                    Ok(())
                }
                [name, value] => {
                    let value = match value {
                        "clear" => None,
                        v => Some(v.parse::<State>()?),
                    };
                    state::set(&dir, name, value).map_err(|e| e.to_string())
                }
                _ => Err("usage: noren state [<name> <ok|fail|warn|busy|clear>]".into()),
            }
        }
        "report" => {
            let profile = match positional(rest) {
                Some(path) => Some(Profile::load(path.as_ref())?),
                None => None,
            };
            // A missing or unreadable journal only leaves the transactions out.
            let transactions = journal(rest).and_then(|j| j.list().map_err(|e| e.to_string()));
            let report = noren::report::Report::collect(profile.as_ref(), &transactions.unwrap_or_default());
            println!("{}\n", report.title());
            print!("{}", report.markdown());
            if flag(rest, "--open") {
                shell::command::open(&report.issue_url()).map_err(|e| e.to_string())?;
                println!("\nopened a new GitHub issue in the browser; nothing is sent until you submit it");
            } else {
                println!("\nnoren report --open opens this as a GitHub issue to review and submit");
            }
            Ok(())
        }
        "update" => {
            use noren::update::{self, Outcome, Version};
            if flag(rest, "--check") {
                match update::check()? {
                    Some(r) => println!(
                        "noren {} is out (this is {}): {}",
                        r.version,
                        Version::current(),
                        r.page
                    ),
                    None => println!("noren {} is the newest release", Version::current()),
                }
                return Ok(());
            }
            match update::update()? {
                Outcome::UpToDate(v) => println!("noren {v} is the newest release"),
                Outcome::Updated { release, programs } => {
                    println!("updated to noren {}: {}", release.version, programs.join(", "));
                    println!("release notes: {}", release.page);
                }
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
