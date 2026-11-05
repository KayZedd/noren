//! The now-playing widget with a made-up track, for screenshots of how the cover recolors it.
//!
//! ```text
//! cargo run --example media -- out.png            # a warm cover
//! cargo run --example media -- out.png --cool     # a cool one
//! ```
//!
//! On Windows, `noren panel` with `[media]` in the profile shows the real session.

use std::sync::Arc;
use std::time::{Duration, Instant};

use noren::host::sim::Simulator;
use noren::media::{Control, Cover, MediaConfig, MediaWidget, Player, Status, Track};
use noren::surface::MonitorInfo;
use noren::{Color, PaletteSpec, Panel, PanelConfig, Theme};

/// A cover drawn from two colors: a diagonal gradient with a sun.
fn cover(from: [f32; 3], to: [f32; 3], sun: [f32; 3]) -> Cover {
    let side = 192u32;
    let mut rgba = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let (fx, fy) = (x as f32 / side as f32, y as f32 / side as f32);
            let t = (fx * 0.3 + fy * 0.7).clamp(0.0, 1.0);
            let mut c: [f32; 3] = std::array::from_fn(|i| from[i] + (to[i] - from[i]) * t);
            let d = ((fx - 0.62).powi(2) + (fy - 0.42).powi(2)).sqrt();
            if d < 0.2 {
                c = sun;
            } else if fy > 0.7 && ((fy * 40.0).sin() > 0.4) {
                c = c.map(|v| v * 0.7);
            }
            rgba.extend(c.map(|v| (v.clamp(0.0, 1.0) * 255.0) as u8));
            rgba.push(255);
        }
    }
    Cover {
        width: side,
        height: side,
        rgba,
    }
}

struct Fixed(Track);

impl Player for Fixed {
    fn read(&mut self) -> Result<Option<Track>, String> {
        Ok(Some(self.0.clone()))
    }
    fn send(&mut self, _: Control) -> Result<(), String> {
        Ok(())
    }
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .ok_or("usage: media OUT.png [--cool]")?;
    let cool = args.iter().any(|a| a == "--cool");
    let track = if cool {
        Track {
            title: "Low Tide".into(),
            artist: "Harbour Lights".into(),
            album: "Salt and Glass".into(),
            cover: Some(Arc::new(cover(
                [0.05, 0.2, 0.35],
                [0.0, 0.45, 0.5],
                [0.75, 0.95, 0.9],
            ))),
            ..Track::default()
        }
    } else {
        Track {
            title: "Paper Lanterns".into(),
            artist: "Mira Okafor".into(),
            album: "Summer Hours".into(),
            cover: Some(Arc::new(cover(
                [0.95, 0.45, 0.2],
                [0.55, 0.1, 0.3],
                [1.0, 0.85, 0.4],
            ))),
            ..Track::default()
        }
    };
    let track = Track {
        status: Status::Playing,
        position: Some(Duration::from_secs(83)),
        length: Some(Duration::from_secs(214)),
        ..track
    };

    let spec = PaletteSpec::new(Color::rgb8(125, 151, 212));
    let theme = noren::Palette::generate(&spec).theme(Theme::aizome());
    let mut panel = Panel::new(PanelConfig::default(), theme);
    panel.add(MediaWidget::with_player(
        MediaConfig::default(),
        spec,
        move || Ok(Box::new(Fixed(track)) as Box<dyn Player>),
    ));
    panel.set_viewport(640.0, 400.0, 1.0);
    panel.reveal_now();
    let background = panel.theme().alternate_background;
    let mut sim = Simulator::new(
        MonitorInfo {
            width: 640.0,
            height: 400.0,
            scale: 1.0,
            origin: (0, 0),
        },
        background,
    );
    let start = Instant::now();
    panel.tick(start, None, false);
    panel.open_popup("media", start);
    sim.add(Box::new(panel));
    sim.set_pointer(Some((320.0, 10.0)));
    while start.elapsed() < Duration::from_millis(400) {
        sim.tick(Instant::now());
        std::thread::sleep(Duration::from_millis(20));
    }
    let now = Instant::now();
    sim.tick(now);
    sim.render(now).save_png(out).map_err(|e| e.to_string())
}
