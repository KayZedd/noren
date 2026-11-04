//! Any program is a widget: noren runs it and draws what it prints.
//!
//! ```toml
//! [[widget]]
//! name = "weather"
//! run = ["curl", "-s", "wttr.in/Warsaw?format=%t"]
//! interval = 600          # seconds between runs; leave out for a program that keeps printing
//! on_click = ["explorer.exe", "https://wttr.in/Warsaw"]
//! ```
//!
//! Each line the program prints is one update. A line starting with `{` is JSON in the shape
//! waybar's custom modules use, so their scripts work as they are:
//!
//! ```json
//! {"text": "21°", "tooltip": "Clear sky", "class": "ok", "percentage": 40,
//!  "rows": [["Humidity", "60%"], ["Wind", "12 km/h"]]}
//! ```
//!
//! Every field is optional. `class` (a string or a list) picks the color: `ok`/`good`/
//! `positive`, `warn`/`warning`, `fail`/`critical`/`negative`, `busy`/`accent`; `color`
//! (`#rrggbb`) overrides it. `percentage` (0 to 100) draws a ring. `tooltip` and `rows` fill
//! the popup. Any other line is plain text. With `interval`, the last line of each run counts;
//! without it the program runs for as long as the widget does and is restarted if it exits.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command as Process, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::components::{caption, ring};
use crate::widget::{Command, Context, Widget};
use crate::{Color, Element, Theme, column, row, text};

/// How long a program that keeps printing waits before it is started again after it exits.
const RESTART: Duration = Duration::from_secs(5);
/// Shortest `interval`, so a typo cannot fork a process every frame.
const MIN_INTERVAL: f64 = 0.5;

/// `[[widget]]` in a profile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecConfig {
    pub name: String,
    /// Program and arguments, not run through a shell.
    pub run: Vec<String>,
    /// Seconds between runs; `None` keeps one process running and reads every line.
    pub interval: Option<f64>,
    /// Started (and not waited for) when the widget is clicked, instead of opening the popup.
    pub on_click: Option<Vec<String>>,
}

impl ExecConfig {
    pub fn validate(&self) -> Result<(), String> {
        let who = format!("widget {:?}", self.name);
        if self.name.trim().is_empty() {
            return Err("a [[widget]] needs a name".into());
        }
        if self.run.first().is_none_or(|p| p.trim().is_empty()) {
            return Err(format!("{who}: run needs a program"));
        }
        if self
            .on_click
            .as_ref()
            .is_some_and(|c| c.first().is_none_or(|p| p.trim().is_empty()))
        {
            return Err(format!("{who}: on_click needs a program"));
        }
        if let Some(i) = self.interval
            && !(i.is_finite() && i >= MIN_INTERVAL)
        {
            return Err(format!("{who}: interval must be at least {MIN_INTERVAL} seconds"));
        }
        Ok(())
    }
}

/// What a widget color comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Plain,
    Positive,
    Warning,
    Negative,
    Accent,
}

impl Tone {
    fn from_class(class: &str) -> Option<Self> {
        Some(match class.to_ascii_lowercase().as_str() {
            "ok" | "good" | "positive" | "success" => Tone::Positive,
            "warn" | "warning" => Tone::Warning,
            "fail" | "critical" | "negative" | "error" | "urgent" => Tone::Negative,
            "busy" | "accent" | "active" => Tone::Accent,
            _ => return None,
        })
    }

    fn color(self, theme: &Theme) -> Color {
        match self {
            Tone::Plain => theme.text,
            Tone::Positive => theme.positive,
            Tone::Warning => theme.neutral,
            Tone::Negative => theme.negative,
            Tone::Accent => theme.highlight,
        }
    }
}

/// One update from the program.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Output {
    pub text: String,
    pub tooltip: Option<String>,
    pub tone: Tone,
    /// `color` from the JSON, over `tone`.
    pub color: Option<Color>,
    /// 0..=1.
    pub fraction: Option<f32>,
    pub rows: Vec<(String, String)>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Classes {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct Json {
    #[serde(default)]
    text: Option<serde_json::Value>,
    tooltip: Option<String>,
    class: Option<Classes>,
    color: Option<String>,
    percentage: Option<f64>,
    #[serde(default)]
    rows: Vec<(String, serde_json::Value)>,
}

/// Plain text for a JSON scalar: strings unquoted, numbers and booleans as written.
fn plain(v: serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s,
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

impl Output {
    /// Reads one printed line. Broken JSON shows as an error rather than as raw text.
    pub fn parse(line: &str) -> Self {
        let line = line.trim();
        if !line.starts_with('{') {
            return Self {
                text: line.to_owned(),
                ..Self::default()
            };
        }
        let json: Json = match serde_json::from_str(line) {
            Ok(j) => j,
            Err(e) => return Self::error(format!("bad JSON: {e}")),
        };
        let classes = match json.class {
            Some(Classes::One(c)) => vec![c],
            Some(Classes::Many(c)) => c,
            None => Vec::new(),
        };
        Self {
            text: json.text.map(plain).unwrap_or_default(),
            tooltip: json.tooltip.filter(|t| !t.is_empty()),
            tone: classes
                .iter()
                .find_map(|c| Tone::from_class(c))
                .unwrap_or_default(),
            color: json.color.as_deref().and_then(Color::from_hex),
            fraction: json
                .percentage
                .filter(|p| p.is_finite())
                .map(|p| (p / 100.0).clamp(0.0, 1.0) as f32),
            rows: json.rows.into_iter().map(|(k, v)| (k, plain(v))).collect(),
        }
    }

    /// A red `!` with the reason in the popup.
    pub fn error(reason: impl Into<String>) -> Self {
        Self {
            text: "!".into(),
            tooltip: Some(reason.into()),
            tone: Tone::Negative,
            ..Self::default()
        }
    }

    fn color(&self, theme: &Theme) -> Color {
        self.color.unwrap_or_else(|| self.tone.color(theme))
    }
}

fn command(argv: &[String]) -> Process {
    let mut c = Process::new(&argv[0]);
    c.args(&argv[1..]).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        /// No console window flashing up for every run.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// The last non-empty line of `out`.
fn last_line(out: &[u8]) -> Option<String> {
    String::from_utf8_lossy(out)
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(str::to_owned)
}

fn run_once(argv: &[String]) -> Output {
    match command(argv).stderr(Stdio::null()).output() {
        Ok(out) => match last_line(&out.stdout) {
            Some(line) => Output::parse(&line),
            None if out.status.success() => Output::default(),
            None => Output::error(format!("{} exited with {}", argv[0], out.status)),
        },
        Err(e) => Output::error(format!("{}: {e}", argv[0])),
    }
}

/// Runs `argv` every `every` until `stop` hangs up.
fn every(argv: Vec<String>, every: Duration, tx: Sender<Output>, stop: Receiver<()>) {
    loop {
        if tx.send(run_once(&argv)).is_err() {
            return;
        }
        match stop.recv_timeout(every) {
            Err(RecvTimeoutError::Timeout) => {}
            _ => return,
        }
    }
}

/// Keeps `argv` running and forwards every line until `stop` hangs up. `child` lets the
/// widget kill the process while this thread blocks on its output.
fn stream(argv: Vec<String>, tx: Sender<Output>, stop: Receiver<()>, child: Arc<Mutex<Option<Child>>>) {
    loop {
        match command(&argv)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut c) => {
                let out = c.stdout.take();
                *child.lock().unwrap_or_else(|e| e.into_inner()) = Some(c);
                // The widget may have gone while we spawned.
                if matches!(stop.try_recv(), Err(mpsc::TryRecvError::Disconnected)) {
                    kill(&child);
                    return;
                }
                for line in out.into_iter().flat_map(|o| BufReader::new(o).lines()) {
                    let Ok(line) = line else { break };
                    if !line.trim().is_empty() && tx.send(Output::parse(&line)).is_err() {
                        kill(&child);
                        return;
                    }
                }
                kill(&child);
            }
            Err(e) => {
                if tx.send(Output::error(format!("{}: {e}", argv[0]))).is_err() {
                    return;
                }
            }
        }
        match stop.recv_timeout(RESTART) {
            Err(RecvTimeoutError::Timeout) => {}
            _ => return,
        }
    }
}

fn kill(child: &Mutex<Option<Child>>) {
    if let Some(mut c) = child.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// A panel with one widget per config, colored by `theme`. Names must be unique: they are the
/// widgets' ids.
pub fn panel(configs: &[ExecConfig], theme: Theme) -> Result<crate::Panel, String> {
    if configs.is_empty() {
        return Err("the profile has no [[widget]]".into());
    }
    for (i, c) in configs.iter().enumerate() {
        if configs[..i].iter().any(|o| o.name == c.name) {
            return Err(format!("two widgets are named {:?}", c.name));
        }
        c.validate()?;
    }
    let mut panel = crate::Panel::new(crate::PanelConfig::default(), theme);
    for c in configs {
        panel.add(ExecWidget::spawn(c.clone())?);
    }
    Ok(panel)
}

/// A widget fed by a program.
pub struct ExecWidget {
    config: ExecConfig,
    latest: Option<Output>,
    rx: Receiver<Output>,
    /// Dropped to stop the runner thread.
    _stop: Sender<()>,
    child: Arc<Mutex<Option<Child>>>,
}

#[derive(Clone, Debug)]
pub enum Msg {
    Click,
}

impl ExecWidget {
    /// Starts the program on a background thread.
    pub fn spawn(config: ExecConfig) -> Result<Self, String> {
        config.validate()?;
        let (tx, rx) = mpsc::channel();
        let (stop, stopped) = mpsc::channel();
        let child = Arc::new(Mutex::new(None));
        let argv = config.run.clone();
        let name = format!("noren-widget-{}", config.name);
        let spawned = match config.interval {
            Some(s) => thread::Builder::new()
                .name(name)
                .spawn(move || every(argv, Duration::from_secs_f64(s), tx, stopped)),
            None => {
                let child = Arc::clone(&child);
                thread::Builder::new()
                    .name(name)
                    .spawn(move || stream(argv, tx, stopped, child))
            }
        };
        spawned.map_err(|e| e.to_string())?;
        Ok(Self {
            config,
            latest: None,
            rx,
            _stop: stop,
            child,
        })
    }

    /// The newest update, if any arrived yet.
    pub fn latest(&self) -> Option<&Output> {
        self.latest.as_ref()
    }
}

impl Drop for ExecWidget {
    fn drop(&mut self) {
        kill(&self.child);
    }
}

impl Widget for ExecWidget {
    type Message = Msg;

    fn id(&self) -> &str {
        &self.config.name
    }

    fn poll(&mut self, _now: Instant) -> bool {
        let newest = self.rx.try_iter().last();
        match newest {
            Some(o) if self.latest.as_ref() != Some(&o) => {
                self.latest = Some(o);
                true
            }
            _ => false,
        }
    }

    fn compact(&self, ctx: &Context<'_>) -> Element<Msg> {
        let theme = ctx.theme;
        let cell = ctx.panel_height - 2.0 * theme.units.small_spacing;
        let out = self.latest.clone().unwrap_or_else(|| Output {
            text: "…".into(),
            tone: Tone::Plain,
            ..Output::default()
        });
        let color = out.color(theme);
        let mut view = row().align_center().gap(theme.units.small_spacing);
        if let Some(f) = out.fraction {
            view = view.child(ring(theme, cell * 0.8, f, color, ""));
        }
        if !out.text.is_empty() {
            view = view.child(text(out.text).color(color).font_size((cell * 0.4).max(9.0)));
        }
        match self.config.on_click {
            Some(_) => view.on_click(Msg::Click),
            None => view,
        }
    }

    fn popup(&self, ctx: &Context<'_>) -> Option<Element<Msg>> {
        let out = self.latest.as_ref()?;
        if out.tooltip.is_none() && out.rows.is_empty() {
            return None;
        }
        let theme = ctx.theme;
        let mut view = column().gap(theme.units.small_spacing).min_width(200.0);
        if let Some(t) = &out.tooltip {
            view = view.child(text(t.clone()).color(theme.text));
        }
        for (label, value) in &out.rows {
            view = view.child(
                row()
                    .justify_between()
                    .gap(theme.units.large_spacing)
                    .child(caption(theme, label))
                    .child(text(value.clone()).color(theme.text)),
            );
        }
        Some(view)
    }

    fn update(&mut self, message: Msg, _now: Instant) -> Command {
        match message {
            Msg::Click => {
                if let Some(argv) = &self.config.on_click
                    && let Err(e) = command(argv).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
                {
                    self.latest = Some(Output::error(format!("{}: {e}", argv[0])));
                }
                Command::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(run: &[&str], interval: Option<f64>) -> ExecConfig {
        ExecConfig {
            name: "t".into(),
            run: run.iter().map(|s| s.to_string()).collect(),
            interval,
            on_click: None,
        }
    }

    #[test]
    fn waybar_json_parses() {
        let o = Output::parse(
            r##"{"text": "21°", "tooltip": "Clear", "class": ["x", "warning"], "percentage": 140, "rows": [["Wind", 12], ["Sky", "clear"]]}"##,
        );
        assert_eq!(o.text, "21°");
        assert_eq!(o.tooltip.as_deref(), Some("Clear"));
        assert_eq!(o.tone, Tone::Warning);
        assert_eq!(o.fraction, Some(1.0));
        assert_eq!(
            o.rows,
            vec![("Wind".into(), "12".into()), ("Sky".into(), "clear".into())]
        );
    }

    #[test]
    fn numbers_colors_and_classes() {
        let o = Output::parse(r##"{"text": 42, "class": "critical", "color": "#00ff00"}"##);
        assert_eq!(o.text, "42");
        assert_eq!(o.tone, Tone::Negative);
        assert_eq!(o.color, Color::from_hex("#00ff00"));
        let theme = Theme::aizome();
        assert_eq!(o.color(&theme), Color::from_hex("#00ff00").unwrap());
        assert_eq!(Output::parse(r#"{"class": "ok"}"#).color(&theme), theme.positive);
        assert_eq!(Output::parse(r#"{"class": "unknown"}"#).tone, Tone::Plain);
    }

    #[test]
    fn plain_lines_and_broken_json() {
        assert_eq!(Output::parse("  hello  ").text, "hello");
        let bad = Output::parse("{\"text\": ");
        assert_eq!((bad.text.as_str(), bad.tone), ("!", Tone::Negative));
        assert!(bad.tooltip.unwrap().starts_with("bad JSON"));
    }

    #[test]
    fn last_line_skips_blank_tail() {
        assert_eq!(last_line(b"a\nb\n\n  \n").as_deref(), Some("b"));
        assert_eq!(last_line(b"\n"), None);
    }

    #[test]
    fn configs_are_checked() {
        assert!(config(&["x"], None).validate().is_ok());
        assert!(config(&[], None).validate().is_err());
        assert!(config(&["x"], Some(0.1)).validate().is_err());
        assert!(config(&["x"], Some(f64::NAN)).validate().is_err());
        let blank_click = ExecConfig {
            on_click: Some(vec![]),
            ..config(&["x"], None)
        };
        assert!(blank_click.validate().is_err());
    }

    fn wait(w: &mut ExecWidget, until: impl Fn(&Output) -> bool) -> Output {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            w.poll(Instant::now());
            if let Some(o) = w.latest().filter(|o| until(o)) {
                return o.clone();
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("no matching update, last: {:?}", w.latest());
    }

    #[cfg(unix)]
    #[test]
    fn interval_programs_report_their_last_line() {
        let mut w = ExecWidget::spawn(config(
            &["sh", "-c", r#"echo skip; echo '{"text":"up","class":"ok"}'"#],
            Some(60.0),
        ))
        .unwrap();
        let o = wait(&mut w, |_| true);
        assert_eq!((o.text.as_str(), o.tone), ("up", Tone::Positive));
    }

    #[cfg(unix)]
    #[test]
    fn streaming_programs_update_per_line_and_die_with_the_widget() {
        let mut w =
            ExecWidget::spawn(config(&["sh", "-c", "echo one; echo two; exec sleep 30"], None)).unwrap();
        wait(&mut w, |o| o.text == "two");
        let child = Arc::clone(&w.child);
        drop(w);
        assert!(child.lock().unwrap().is_none(), "the program was killed");
    }

    #[test]
    fn panels_need_unique_valid_widgets() {
        let theme = Theme::aizome();
        assert!(panel(&[], theme.clone()).is_err());
        let twins = [config(&["x"], Some(60.0)), config(&["y"], Some(60.0))];
        assert!(panel(&twins, theme.clone()).err().unwrap().contains("two widgets"));
        assert!(panel(&[config(&["x"], Some(0.0))], theme).is_err());
    }

    #[test]
    fn the_popup_shows_tooltip_and_rows_only_when_there_are_some() {
        let theme = Theme::aizome();
        let ctx = Context {
            theme: &theme,
            now: Instant::now(),
            popup_open: true,
            panel_height: 36.0,
        };
        let mut w = ExecWidget::spawn(config(&["noren-no-such-program"], Some(60.0))).unwrap();
        w.latest = Some(Output::parse(r#"{"text": "a"}"#));
        assert!(w.popup(&ctx).is_none());
        w.latest = Some(Output::parse(r#"{"text": "a", "rows": [["k", "v"]]}"#));
        assert!(w.popup(&ctx).is_some());
        // A click opens the popup unless on_click takes it.
        assert!(w.compact(&ctx).click_message().is_none());
        w.config.on_click = Some(vec!["x".into()]);
        assert!(w.compact(&ctx).click_message().is_some());
    }

    #[test]
    fn a_missing_program_shows_an_error() {
        let mut w = ExecWidget::spawn(config(&["noren-no-such-program"], Some(60.0))).unwrap();
        let o = wait(&mut w, |_| true);
        assert_eq!(o.tone, Tone::Negative);
        assert!(o.tooltip.unwrap().contains("noren-no-such-program"));
    }
}
