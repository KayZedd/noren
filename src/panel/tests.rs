use super::*;
use crate::{column, container};

#[derive(Default)]
struct Probe {
    clicks: u32,
    with_popup: bool,
}

#[derive(Clone)]
enum Msg {
    Inner,
    Close,
    Quit,
}

impl Widget for Probe {
    type Message = Msg;

    fn id(&self) -> &str {
        if self.with_popup { "probe" } else { "plain" }
    }

    fn compact(&self, _: &Context<'_>) -> Element<Msg> {
        crate::row()
            .child(container().size(20.0, 20.0))
            .child(container().size(10.0, 10.0).on_click(Msg::Inner))
    }

    fn popup(&self, _: &Context<'_>) -> Option<Element<Msg>> {
        self.with_popup.then(|| {
            column()
                .child(container().size(300.0, 30.0).on_click(Msg::Close))
                .child(container().size(300.0, 30.0).on_click(Msg::Quit))
                .child(
                    column()
                        .height(50.0)
                        .scrollable("list")
                        .children((0..10).map(|_| container().height(20.0))),
                )
        })
    }

    fn update(&mut self, m: Msg, _: Instant) -> Command {
        self.clicks += 1;
        match m {
            Msg::Inner => Command::None,
            Msg::Close => Command::ClosePopup,
            Msg::Quit => Command::Quit,
        }
    }
}

fn panel() -> (Panel, Instant) {
    let mut p = Panel::with_text(
        PanelConfig::default(),
        Theme::aizome(),
        TextSystem::from_font_data(Vec::new(), &[]),
    );
    p.add(Probe {
        with_popup: true,
        ..Probe::default()
    });
    p.add(Probe::default());
    p.set_viewport(1000.0, 800.0, 1.0);
    let now = Instant::now();
    (p, now)
}

const MS: Duration = Duration::from_millis(1);

#[test]
fn collapsed_panel_draws_nothing_and_hot_edge_reveals_it() {
    let (mut p, now) = panel();
    p.tick(now, Some((500.0, 300.0)), false);
    assert!(p.is_hidden(now));
    let r = p.tick(now + MS, Some((500.0, 0.0)), false);
    assert!(r.redraw);
    assert_eq!(p.visibility(), Visibility::Revealed);
    // Slide animation in progress, done after long_duration.
    assert!(p.animating(now + 10 * MS));
    assert!(!p.animating(now + 300 * MS));
}

#[test]
fn cells_are_centered_and_clicking_one_opens_its_popup_under_it() {
    let (mut p, now) = panel();
    p.reveal_now();
    p.tick(now, Some((500.0, 20.0)), false);
    let cell = p.cell_rect("probe").unwrap();
    let plain = p.cell_rect("plain").unwrap();
    // Two cells of 30 + 2*4 padding, gap 4, centered in the 984-wide bar at x=8.
    assert_eq!(cell.w, 38.0);
    assert_eq!(plain.x - cell.right(), 4.0);
    assert!((cell.x + plain.right() - 2.0 * 500.0).abs() < 1.0);
    let r = p.click(cell.x + 2.0, 20.0, now);
    assert!(r.redraw);
    assert_eq!(p.popup_open(), Some("probe"));
    p.tick(now + MS, Some((500.0, 20.0)), false);
    let popup = p.popup_rect().unwrap();
    assert_eq!(popup.y, 8.0 + 36.0 + 6.0);
    assert!((popup.center_x() - cell.center_x()).abs() <= 1.0);
    // Toggling the same cell closes it.
    p.click(cell.x + 2.0, 20.0, now);
    assert_eq!(p.popup_open(), None);
}

#[test]
fn inner_click_targets_the_widget_not_the_popup_toggle() {
    let (mut p, now) = panel();
    p.reveal_now();
    p.tick(now, None, false);
    let cell = p.cell_rect("probe").unwrap();
    // The 10x10 inner element sits after the 20 px box, vertically centered.
    p.click(cell.x + 4.0 + 25.0, 20.0, now);
    assert_eq!(p.widgets.len(), 2);
    assert_eq!(p.popup_open(), None);
}

#[test]
fn widget_without_popup_ignores_toggle() {
    let (mut p, now) = panel();
    p.reveal_now();
    p.tick(now, None, false);
    let plain = p.cell_rect("plain").unwrap();
    assert_eq!(p.click(plain.x + 2.0, 20.0, now), Response::default());
    assert_eq!(p.popup_open(), None);
}

#[test]
fn popup_commands_escape_outside_click_and_quit() {
    let (mut p, now) = panel();
    p.reveal_now();
    assert!(p.open_popup("probe", now));
    p.tick(now, None, false);
    let popup = p.popup_rect().unwrap();
    let inner = (popup.x + 8.0 + 5.0, popup.y + 8.0 + 5.0);
    p.click(inner.0, inner.1, now);
    assert_eq!(p.popup_open(), None, "ClosePopup command");

    p.open_popup("probe", now);
    p.tick(now, None, false);
    assert!(p.escape(now).redraw);
    assert_eq!(p.popup_open(), None);

    p.open_popup("probe", now);
    p.tick(now, None, false);
    assert!(p.click(5.0, 700.0, now).redraw);
    assert_eq!(p.popup_open(), None, "outside click");

    p.open_popup("probe", now);
    p.tick(now, None, false);
    let r = p.click(inner.0, inner.1 + 30.0, now);
    assert!(r.quit);
}

#[test]
fn popup_scroll_is_clamped_to_content() {
    let (mut p, now) = panel();
    p.reveal_now();
    p.open_popup("probe", now);
    p.tick(now, None, false);
    let popup = p.popup_rect().unwrap();
    let over_list = (popup.x + 20.0, popup.y + 8.0 + 60.0 + 10.0);
    assert!(p.scroll(over_list.0, over_list.1, 1000.0).redraw);
    assert_eq!(p.scrolls["list"], 150.0);
    assert!(!p.scroll(over_list.0, over_list.1, 10.0).redraw);
    assert!(
        !p.scroll(popup.x + 20.0, popup.y + 10.0, 10.0).redraw,
        "not a scroll area"
    );
}

#[test]
fn popup_holds_panel_open_and_fullscreen_closes_it() {
    let (mut p, now) = panel();
    p.reveal_now();
    p.open_popup("probe", now);
    p.tick(now + 1000 * MS, None, false);
    assert_eq!(p.visibility(), Visibility::Revealed);
    p.tick(now + 1001 * MS, None, true);
    assert_eq!(p.popup_open(), None);
    assert_eq!(p.visibility(), Visibility::Collapsed);
}

#[test]
fn window_grows_to_cover_the_popup_and_render_paints_inside_it() {
    let (mut p, now) = panel();
    p.reveal_now();
    p.tick(now, None, false);
    let (w, bar_h) = p.window_size(now);
    assert_eq!((w, bar_h), (1000.0, 54.0));
    p.open_popup("probe", now);
    p.reveal_now();
    p.tick(now, None, false);
    let (_, h) = p.window_size(now);
    let popup = p.popup_rect().unwrap();
    assert_eq!(h, (popup.bottom() + 10.0).ceil());
    let mut px = Pixmap::new(w as u32, h as u32).unwrap();
    p.render(&mut px, now);
    let alpha = |x: f32, y: f32| px.pixel(x as u32, y as u32).unwrap().alpha();
    assert!(alpha(500.0, 20.0) > 200, "bar background");
    assert_eq!(alpha(popup.center_x(), popup.y + 2.0), 255, "popup background");
    assert_eq!(
        alpha(2.0, h - 1.0),
        0,
        "outside stays transparent (click-through)"
    );
}
