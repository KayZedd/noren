//! System Pulse: port of the `systempulse` KDE plasmoid (CPU/RAM/GPU rings, temperature,
//! network) as a top panel widget.
//!
//! ```text
//! cargo run --release --example pulse                    # Windows: native top panel
//! cargo run --example pulse --features preview -- --preview
//! cargo run --example pulse -- --screenshot out.png --popup
//! ```
//!
//! Flags: `--preview`, `--screenshot PATH`, `--popup` (screenshot with the popup open),
//! `--monitor N`, `--net` (network rates in the panel), `--scale S` (screenshot scale).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use noren::components::{SparklinePaint, card, plasmoid_heading, ring, sparkline};
use noren::host::{Entry, MonitorSelect, SurfaceOptions};
use noren::surface::MonitorInfo;
use noren::tiny_skia::PathBuilder;
use noren::{
    Canvas, Color, Command, Context, Element, History, Paint, Panel, PanelConfig, Rect, Sampler, Theme,
    Widget, column, container, grid, row, text,
};

/// Defaults of `package/contents/config/main.xml`.
#[derive(Clone, Debug)]
struct Config {
    interval: Duration,
    history: usize,
    show_cpu: bool,
    show_ram: bool,
    show_gpu: bool,
    show_temp: bool,
    show_net: bool,
    load_warn: f32,
    load_crit: f32,
    temp_warn: f32,
    temp_crit: f32,
    monitor_command: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(1000),
            history: 60,
            show_cpu: true,
            show_ram: true,
            show_gpu: true,
            show_temp: true,
            show_net: false,
            load_warn: 70.0,
            load_crit: 90.0,
            temp_warn: 70.0,
            temp_crit: 85.0,
            monitor_command: if cfg!(windows) {
                "taskmgr"
            } else {
                "plasma-systemmonitor"
            }
            .into(),
        }
    }
}

/// One reading. `None` = sensor unavailable ("n/a" in the plasmoid).
#[derive(Clone, Debug, Default)]
struct Sample {
    cpu: Option<f32>,
    ram: Option<f32>,
    ram_used: Option<f64>,
    ram_total: Option<f64>,
    gpu: Option<f32>,
    temp: Option<f32>,
    down: Option<f64>,
    up: Option<f64>,
}

// ---- pulse.js ----

/// 0 = normal, 1 = warning, 2 = critical.
fn level(value: Option<f32>, warn: f32, crit: f32) -> u8 {
    match value {
        Some(v) if v.is_finite() && v >= crit => 2,
        Some(v) if v.is_finite() && v >= warn => 1,
        _ => 0,
    }
}

fn format_percent(value: Option<f32>) -> String {
    value
        .filter(|v| v.is_finite())
        .map_or("n/a".into(), |v| format!("{}%", v.round()))
}

/// Sensors report 0 without a thermal zone, so 0 counts as missing.
fn format_temp(value: Option<f32>) -> String {
    value
        .filter(|v| v.is_finite() && *v > 0.0)
        .map_or("n/a".into(), |v| format!("{}°C", v.round()))
}

fn format_bytes(bytes: Option<f64>, suffix: &str) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let Some(mut v) = bytes.filter(|v| v.is_finite() && *v >= 0.0) else {
        return "n/a".into();
    };
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    // For v >= 0, f64::round matches JS Math.round.
    let text = if i == 0 || v >= 10.0 {
        format!("{}", v.round())
    } else {
        format!("{v:.1}")
    };
    format!("{text} {}{suffix}", UNITS[i])
}

fn format_rate(bytes_per_second: Option<f64>) -> String {
    format_bytes(bytes_per_second, "/s")
}

// ---- widget ----

struct Pulse {
    config: Config,
    sampler: Sampler<Sample>,
    latest: Sample,
    cpu: History,
    ram: History,
    gpu: History,
    temp: History,
    down: History,
    up: History,
}

#[derive(Clone)]
enum Msg {
    OpenMonitor,
}

/// Monitor-with-pulse glyph standing in for the `utilities-system-monitor` icon.
#[derive(Clone, Debug)]
struct MonitorIcon(Color);

impl Paint for MonitorIcon {
    fn paint(&self, canvas: &mut Canvas<'_>, rect: Rect) {
        let s = rect.w.min(rect.h);
        let screen = Rect::new(rect.x + s * 0.08, rect.y + s * 0.12, s * 0.84, s * 0.62);
        canvas.stroke_rounded_rect(screen, s * 0.08, (s * 0.08).max(1.0), self.0);
        let mut pb = PathBuilder::new();
        let (x0, y) = (screen.x + s * 0.12, screen.y + screen.h * 0.55);
        pb.move_to(x0, y);
        for (dx, dy) in [(0.18, 0.0), (0.27, -0.2), (0.38, 0.18), (0.48, 0.0), (0.6, 0.0)] {
            pb.line_to(x0 + s * dx, y + s * dy);
        }
        pb.move_to(rect.x + s * 0.3, rect.y + s * 0.9);
        pb.line_to(rect.x + s * 0.7, rect.y + s * 0.9);
        if let Some(path) = pb.finish() {
            canvas.stroke_path(&path, (s * 0.08).max(1.0), self.0, true);
        }
    }
}

impl Pulse {
    fn new(config: Config) -> Self {
        let n = config.history;
        let mut probe = Probe::new();
        Self {
            sampler: Sampler::spawn(config.interval, move || probe.sample()),
            latest: Sample::default(),
            cpu: History::new(n),
            ram: History::new(n),
            gpu: History::new(n),
            temp: History::new(n),
            down: History::new(n),
            up: History::new(n),
            config,
        }
    }

    fn load_color(&self, theme: &Theme, v: Option<f32>) -> Color {
        theme.level_color(level(v, self.config.load_warn, self.config.load_crit))
    }

    fn temp_color(&self, theme: &Theme, v: Option<f32>) -> Color {
        theme.level_color(level(v, self.config.temp_warn, self.config.temp_crit))
    }
}

/// `StatCard.qml`.
struct Stat<'a> {
    title: &'a str,
    value: String,
    detail: Option<String>,
    accent: Color,
    history: &'a History,
    max: f32,
}

fn stat_card(theme: &Theme, stat: Stat<'_>) -> Element<Msg> {
    card(theme)
        .border(stat.accent.with_alpha(0.25), 1.0)
        .child(
            row()
                .align_center()
                .child(text(stat.title).opacity(0.7).grow())
                .children(
                    stat.detail
                        .map(|d| text(d).font_size(theme.small_font_size()).opacity(0.6)),
                ),
        )
        .child(
            text(stat.value)
                .color(stat.accent)
                .font_size(theme.font_size() * 1.6)
                .bold(),
        )
        .child(
            sparkline(SparklinePaint::from_history(stat.history, stat.max, stat.accent))
                .height(theme.units.grid_unit * 2.5),
        )
}

impl Widget for Pulse {
    type Message = Msg;

    fn id(&self) -> &str {
        "systempulse"
    }

    fn poll(&mut self, _now: Instant) -> bool {
        let Some(s) = self.sampler.take() else {
            return false;
        };
        // pulse.js push(): missing samples count as 0 so the sparkline keeps scrolling.
        let zero = |v: Option<f32>| v.filter(|v| v.is_finite()).unwrap_or(0.0);
        self.cpu.push(zero(s.cpu));
        self.ram.push(zero(s.ram));
        self.gpu.push(zero(s.gpu));
        self.temp.push(zero(s.temp));
        self.down.push(zero(s.down.map(|v| v as f32)));
        self.up.push(zero(s.up.map(|v| v as f32)));
        self.latest = s;
        true
    }

    fn compact(&self, ctx: &Context<'_>) -> Element<Msg> {
        let theme = ctx.theme;
        let (s, c) = (&self.latest, &self.config);
        let cell = ctx.panel_height - 2.0 * theme.units.small_spacing;
        let gauge = |show: bool, v: Option<f32>, label: &str| {
            ring(
                theme,
                cell * 0.8,
                v.unwrap_or(0.0) / 100.0,
                self.load_color(theme, v),
                label,
            )
            .visible(show)
        };
        let net_size = (cell * 0.28).max(8.0);
        row()
            .align_center()
            .gap(theme.units.small_spacing)
            .child(gauge(c.show_cpu, s.cpu, "C"))
            .child(gauge(c.show_ram, s.ram, "R"))
            .child(gauge(c.show_gpu, s.gpu, "G"))
            .child(
                text(format_temp(s.temp))
                    .color(self.temp_color(theme, s.temp))
                    .font_size((cell * 0.36).max(9.0))
                    .bold()
                    .visible(c.show_temp),
            )
            .child(
                column()
                    .visible(c.show_net)
                    .child(text(format!("↓ {}", format_rate(s.down))).font_size(net_size))
                    .child(
                        text(format!("↑ {}", format_rate(s.up)))
                            .font_size(net_size)
                            .opacity(0.75),
                    ),
            )
    }

    fn popup(&self, ctx: &Context<'_>) -> Option<Element<Msg>> {
        let theme = ctx.theme;
        let units = &theme.units;
        let s = &self.latest;
        let net_max = self.down.scale_max(1024.0);
        let icon = units.icon_small;
        let monitor_button = container()
            .padding(units.small_spacing)
            .radius(units.corner_radius)
            .hover_background(theme.text_alpha(0.1))
            .on_click(Msg::OpenMonitor)
            .child(container().size(icon, icon).painter(MonitorIcon(theme.text)));
        let load = |title, v: Option<f32>, history, detail| Stat {
            title,
            value: format_percent(v),
            detail,
            accent: self.load_color(theme, v),
            history,
            max: 100.0,
        };
        let memory_detail = format!(
            "{} / {}",
            format_bytes(s.ram_used, ""),
            format_bytes(s.ram_total, "")
        );
        let temperature = Stat {
            title: "Temperature",
            value: format_temp(s.temp),
            detail: None,
            accent: self.temp_color(theme, s.temp),
            history: &self.temp,
            max: (self.config.temp_crit + 10.0).max(100.0),
        };
        let network = Stat {
            title: "Network ↓",
            value: format_rate(s.down),
            detail: Some(format!("↑ {}", format_rate(s.up))),
            accent: theme.highlight,
            history: &self.down,
            max: net_max,
        };
        // Upload sparkline drawn over the download one, as in FullView.qml.
        let upload = sparkline(SparklinePaint::from_history(&self.up, net_max, theme.positive))
            .height(units.grid_unit * 2.5)
            .absolute(
                Some(units.large_spacing),
                None,
                Some(units.large_spacing),
                Some(units.large_spacing),
            );
        Some(
            column()
                .min_width(units.grid_unit * 22.0)
                .min_height(units.grid_unit * 24.0)
                .child(plasmoid_heading(theme, "System Pulse", vec![monitor_button]))
                .child(
                    grid(2)
                        .padding(units.large_spacing)
                        .gap(units.large_spacing)
                        .child(stat_card(theme, load("CPU", s.cpu, &self.cpu, None)))
                        .child(stat_card(
                            theme,
                            load("Memory", s.ram, &self.ram, Some(memory_detail)),
                        ))
                        .child(stat_card(theme, load("GPU", s.gpu, &self.gpu, None)))
                        .child(stat_card(theme, temperature))
                        .child(stat_card(theme, network).span(2).child(upload)),
                ),
        )
    }

    fn update(&mut self, message: Msg, _now: Instant) -> Command {
        match message {
            Msg::OpenMonitor => {
                if let Err(e) = std::process::Command::new(&self.config.monitor_command).spawn() {
                    eprintln!("pulse: cannot start {}: {e}", self.config.monitor_command);
                }
                Command::ClosePopup
            }
        }
    }
}

// ---- sensors ----

/// Each source is optional: one that fails to initialize reads as "n/a", like a missing
/// KSysGuard sensor.
struct Probe {
    system: Option<sysinfo::System>,
    networks: Option<sysinfo::Networks>,
    components: Option<sysinfo::Components>,
    last_net: Instant,
    gpu: gpu::Gpu,
}

impl Probe {
    fn new() -> Self {
        Self {
            system: sysinfo::System::new().ok(),
            networks: sysinfo::Networks::new_with_refreshed_list().ok(),
            components: sysinfo::Components::new_with_refreshed_list().ok(),
            last_net: Instant::now(),
            gpu: gpu::Gpu::new(),
        }
    }

    fn sample(&mut self) -> Sample {
        let elapsed = self.last_net.elapsed().as_secs_f64().max(0.001);
        self.last_net = Instant::now();
        let mut sample = Sample {
            gpu: self.gpu.usage(),
            ..Sample::default()
        };
        if let Some(system) = &mut self.system {
            system.refresh_cpu_usage();
            system.refresh_memory();
            let total = system.total_memory() as f64;
            let used = system.used_memory() as f64;
            sample.cpu = Some(system.global_cpu_usage());
            sample.ram = (total > 0.0).then(|| (used / total * 100.0) as f32);
            sample.ram_used = Some(used);
            sample.ram_total = Some(total);
        }
        if let Some(networks) = &mut self.networks {
            networks.refresh(true);
            let (down, up) = networks.iter().fold((0u64, 0u64), |(d, u), (_, n)| {
                (d + n.received(), u + n.transmitted())
            });
            sample.down = Some(down as f64 / elapsed);
            sample.up = Some(up as f64 / elapsed);
        }
        if let Some(components) = &mut self.components {
            components.refresh(false);
            sample.temp = components
                .iter()
                .filter_map(|c| c.temperature())
                .filter(|t| t.is_finite())
                .reduce(f32::max);
        }
        sample
    }
}

#[cfg(windows)]
mod gpu {
    //! GPU load from the "GPU Engine" performance counters (what Task Manager shows): the sum
    //! of 3D engine utilization over all processes, capped at 100 %.
    use windows::Win32::System::Performance::{
        PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
        PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
        PdhOpenQueryW,
    };
    use windows::core::w;

    pub struct Gpu(Option<(PDH_HQUERY, PDH_HCOUNTER)>);

    // PDH handles are process-wide identifiers, usable from the sampler thread.
    unsafe impl Send for Gpu {}

    impl Gpu {
        pub fn new() -> Self {
            let mut query = PDH_HQUERY::default();
            let mut counter = PDH_HCOUNTER::default();
            // Query and counter are owned by Gpu and closed in Drop.
            unsafe {
                if PdhOpenQueryW(None, 0, &mut query) != 0 {
                    return Self(None);
                }
                let path = w!("\\GPU Engine(*engtype_3D)\\Utilization Percentage");
                if PdhAddEnglishCounterW(query, path, 0, &mut counter) != 0 {
                    PdhCloseQuery(query);
                    return Self(None);
                }
                // Rate counters need a first collection as the baseline.
                PdhCollectQueryData(query);
            }
            Self(Some((query, counter)))
        }

        pub fn usage(&mut self) -> Option<f32> {
            let (query, counter) = self.0?;
            // Two-call pattern: ask for the buffer size, then fill a buffer at least that big.
            unsafe {
                if PdhCollectQueryData(query) != 0 {
                    return None;
                }
                let (mut size, mut count) = (0u32, 0u32);
                let status =
                    PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, None);
                if status != PDH_MORE_DATA || size == 0 {
                    return Some(0.0);
                }
                let item = size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
                let mut buffer = vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); (size as usize).div_ceil(item)];
                let status = PdhGetFormattedCounterArrayW(
                    counter,
                    PDH_FMT_DOUBLE,
                    &mut size,
                    &mut count,
                    Some(buffer.as_mut_ptr()),
                );
                if status != 0 {
                    return None;
                }
                let total: f64 = buffer[..(count as usize).min(buffer.len())]
                    .iter()
                    .map(|i| i.FmtValue.Anonymous.doubleValue)
                    .filter(|v| v.is_finite())
                    .sum();
                Some(total.clamp(0.0, 100.0) as f32)
            }
        }
    }

    impl Drop for Gpu {
        fn drop(&mut self) {
            if let Some((query, _)) = self.0 {
                // Closing the query frees its counters too.
                unsafe {
                    PdhCloseQuery(query);
                }
            }
        }
    }
}

#[cfg(not(windows))]
mod gpu {
    //! AMD/Intel via sysfs `gpu_busy_percent`; `None` when no card exposes it.
    pub struct Gpu(Option<std::path::PathBuf>);

    impl Gpu {
        pub fn new() -> Self {
            let path = std::fs::read_dir("/sys/class/drm").ok().and_then(|dir| {
                dir.flatten()
                    .map(|e| e.path().join("device/gpu_busy_percent"))
                    .find(|p| p.exists())
            });
            Self(path)
        }

        pub fn usage(&mut self) -> Option<f32> {
            let path = self.0.as_ref()?;
            std::fs::read_to_string(path).ok()?.trim().parse().ok()
        }
    }
}

// ---- entry point ----

#[derive(Debug, Default, PartialEq)]
struct Args {
    preview: bool,
    screenshot: Option<PathBuf>,
    popup: bool,
    monitor: Option<usize>,
    net: bool,
    scale: f32,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out = Args {
        scale: 1.0,
        ..Args::default()
    };
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--preview" => out.preview = true,
            "--popup" => out.popup = true,
            "--net" => out.net = true,
            "--screenshot" => out.screenshot = Some(value("--screenshot")?.into()),
            "--monitor" => {
                out.monitor = Some(
                    value("--monitor")?
                        .parse()
                        .map_err(|e| format!("--monitor: {e}"))?,
                )
            }
            "--scale" => out.scale = value("--scale")?.parse().map_err(|e| format!("--scale: {e}"))?,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(out)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("pulse: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args(std::env::args().skip(1))?;
    let theme = Theme::kanagawa_wave();
    let wallpaper = theme.alternate_background;
    let mut panel = Panel::new(PanelConfig::default(), theme);
    panel.add(Pulse::new(Config {
        show_net: args.net,
        ..Config::default()
    }));

    if let Some(path) = &args.screenshot {
        return screenshot(panel, path, &args, wallpaper);
    }
    if args.preview {
        #[cfg(feature = "preview")]
        return noren::host::preview::run(
            vec![Entry::new(panel, SurfaceOptions::panel())],
            1280.0,
            720.0,
            wallpaper,
        );
        #[cfg(not(feature = "preview"))]
        return Err("rebuild with --features preview".into());
    }
    let monitor = args.monitor.map_or(MonitorSelect::Primary, MonitorSelect::Index);
    noren::host::run(vec![Entry::new(panel, SurfaceOptions::panel().on(monitor))])
}

/// Renders the revealed panel (optionally with its popup) on a virtual 1280×720 monitor after a
/// few samples and writes a PNG.
fn screenshot(mut panel: Panel, path: &Path, args: &Args, wallpaper: Color) -> Result<(), String> {
    use noren::host::sim::Simulator;
    let start = Instant::now();
    let mut sim = Simulator::new(
        MonitorInfo {
            width: 1280.0,
            height: 720.0,
            scale: args.scale,
        },
        wallpaper,
    );
    panel.set_viewport(1280.0, 720.0, args.scale);
    panel.reveal_now();
    if args.popup {
        panel.open_popup("systempulse", start);
    }
    sim.add(Box::new(panel));
    // Pointer on the bar keeps it revealed; five samples give the sparklines a shape.
    sim.set_pointer(Some((640.0, 20.0)));
    let until = start + Duration::from_millis(4500);
    while Instant::now() < until {
        sim.tick(Instant::now());
        std::thread::sleep(Duration::from_millis(50));
    }
    sim.set_pointer(None);
    let now = Instant::now();
    sim.tick(now);
    sim.render(now).save_png(path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_matches_pulse_js() {
        assert_eq!(format_percent(Some(42.5)), "43%");
        assert_eq!(format_percent(None), "n/a");
        assert_eq!(format_temp(Some(0.0)), "n/a");
        assert_eq!(format_temp(Some(61.4)), "61°C");
        assert_eq!(format_bytes(Some(512.0), ""), "512 B");
        assert_eq!(format_bytes(Some(1536.0), ""), "1.5 KiB");
        assert_eq!(format_bytes(Some(10.0 * 1024.0 * 1024.0), ""), "10 MiB");
        assert_eq!(format_bytes(Some(-1.0), ""), "n/a");
        assert_eq!(format_rate(Some(2048.0)), "2.0 KiB/s");
    }

    #[test]
    fn levels_use_warn_and_crit_thresholds() {
        assert_eq!(level(Some(69.9), 70.0, 90.0), 0);
        assert_eq!(level(Some(70.0), 70.0, 90.0), 1);
        assert_eq!(level(Some(90.0), 70.0, 90.0), 2);
        assert_eq!(level(None, 70.0, 90.0), 0);
        assert_eq!(level(Some(f32::NAN), 70.0, 90.0), 0);
    }

    #[test]
    fn args_parse_flags_and_reject_unknown() {
        let parse = |s: &str| parse_args(s.split_whitespace().map(String::from));
        let a = parse("--preview --monitor 2 --net --scale 1.5").unwrap();
        assert!(a.preview && a.net);
        assert_eq!((a.monitor, a.scale), (Some(2), 1.5));
        assert!(parse("--monitor").is_err());
        assert!(parse("--bogus").is_err());
    }
}
