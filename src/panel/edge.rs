//! Auto-hide state machine for an edge panel (KDE "Auto Hide" visibility).
//!
//! Collapsed: only the hot edge (top `hot_edge` physical pixels of the monitor) reveals the
//! panel. Revealed: the panel stays while the pointer is over the bar or popup, or a popup is
//! open; leaving starts `hide_delay`. A fullscreen foreground window suppresses everything.

use std::time::Duration;

use crate::Rect;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Visibility {
    #[default]
    Collapsed,
    Revealed,
}

#[derive(Debug)]
pub struct Controller {
    hide_delay: Duration,
    visibility: Visibility,
    left_at: Option<Duration>,
}

impl Controller {
    pub fn new(hide_delay: Duration) -> Self {
        Self {
            hide_delay,
            visibility: Visibility::Collapsed,
            left_at: None,
        }
    }

    pub fn visibility(&self) -> Visibility {
        self.visibility
    }

    /// Force a state (tests, screenshots, "always visible" mode).
    pub fn set(&mut self, visibility: Visibility) {
        self.visibility = visibility;
        self.left_at = None;
    }

    /// `now` is any monotonic clock; `hot` is the reveal strip, `active` the areas that keep a
    /// revealed panel open (bar, popup). All in the same coordinate space as `pointer`.
    pub fn update(
        &mut self,
        now: Duration,
        pointer: Option<(f32, f32)>,
        hot: Rect,
        active: &[Rect],
        popup_open: bool,
        fullscreen: bool,
    ) -> Visibility {
        if fullscreen {
            self.set(Visibility::Collapsed);
            return self.visibility;
        }
        let hovered = pointer.is_some_and(|(x, y)| match self.visibility {
            Visibility::Collapsed => hot.contains(x, y),
            Visibility::Revealed => hot.contains(x, y) || active.iter().any(|r| r.contains(x, y)),
        });
        if popup_open || hovered {
            self.visibility = Visibility::Revealed;
            self.left_at = None;
        } else if self.visibility == Visibility::Revealed {
            let left_at = *self.left_at.get_or_insert(now);
            if now.saturating_sub(left_at) >= self.hide_delay {
                self.set(Visibility::Collapsed);
            }
        }
        self.visibility
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOT: Rect = Rect::new(0.0, 0.0, 1920.0, 2.0);
    const BAR: Rect = Rect::new(8.0, 8.0, 1904.0, 36.0);
    const POPUP: Rect = Rect::new(760.0, 50.0, 400.0, 300.0);

    fn tick(
        c: &mut Controller,
        ms: u64,
        pointer: Option<(f32, f32)>,
        popup: bool,
        fullscreen: bool,
    ) -> Visibility {
        let active: &[Rect] = if popup { &[BAR, POPUP] } else { &[BAR] };
        c.update(Duration::from_millis(ms), pointer, HOT, active, popup, fullscreen)
    }

    fn controller() -> Controller {
        Controller::new(Duration::from_millis(350))
    }

    #[test]
    fn only_the_hot_edge_reveals_a_collapsed_panel() {
        let mut c = controller();
        assert_eq!(c.visibility(), Visibility::Collapsed);
        // Over where the bar would be, but collapsed: nothing.
        assert_eq!(
            tick(&mut c, 0, Some((960.0, 20.0)), false, false),
            Visibility::Collapsed
        );
        assert_eq!(
            tick(&mut c, 1, Some((960.0, 2.0)), false, false),
            Visibility::Collapsed
        );
        assert_eq!(
            tick(&mut c, 2, Some((960.0, 1.0)), false, false),
            Visibility::Revealed
        );
    }

    #[test]
    fn floating_margin_between_edge_and_bar_keeps_the_panel_open() {
        let mut c = controller();
        tick(&mut c, 0, Some((960.0, 0.0)), false, false);
        // Moving down through the gap (y 2..8) toward the bar: the hide delay starts but
        // reaching the bar within it cancels the hide.
        assert_eq!(
            tick(&mut c, 50, Some((960.0, 5.0)), false, false),
            Visibility::Revealed
        );
        assert_eq!(
            tick(&mut c, 100, Some((960.0, 20.0)), false, false),
            Visibility::Revealed
        );
        assert_eq!(
            tick(&mut c, 1000, Some((960.0, 20.0)), false, false),
            Visibility::Revealed
        );
    }

    #[test]
    fn leaving_hides_at_the_exact_delay_boundary() {
        let mut c = controller();
        tick(&mut c, 0, Some((960.0, 0.0)), false, false);
        assert_eq!(tick(&mut c, 10, None, false, false), Visibility::Revealed);
        assert_eq!(tick(&mut c, 359, None, false, false), Visibility::Revealed);
        assert_eq!(tick(&mut c, 360, None, false, false), Visibility::Collapsed);
    }

    #[test]
    fn reentering_cancels_the_previous_leave_deadline() {
        let mut c = controller();
        tick(&mut c, 0, Some((960.0, 0.0)), false, false);
        tick(&mut c, 10, None, false, false);
        tick(&mut c, 300, Some((960.0, 20.0)), false, false);
        assert_eq!(tick(&mut c, 400, None, false, false), Visibility::Revealed);
        assert_eq!(tick(&mut c, 749, None, false, false), Visibility::Revealed);
        assert_eq!(tick(&mut c, 750, None, false, false), Visibility::Collapsed);
    }

    #[test]
    fn popup_holds_panel_open_and_closing_starts_a_new_delay() {
        let mut c = controller();
        assert_eq!(tick(&mut c, 0, None, true, false), Visibility::Revealed);
        assert_eq!(tick(&mut c, 10_000, None, true, false), Visibility::Revealed);
        assert_eq!(tick(&mut c, 10_001, None, false, false), Visibility::Revealed);
        assert_eq!(tick(&mut c, 10_351, None, false, false), Visibility::Collapsed);
    }

    #[test]
    fn pointer_over_open_popup_counts_as_inside() {
        let mut c = controller();
        tick(&mut c, 0, None, true, false);
        // Popup still listed as active while closing animation runs.
        let v = c.update(
            Duration::from_millis(5000),
            Some((900.0, 200.0)),
            HOT,
            &[BAR, POPUP],
            false,
            false,
        );
        assert_eq!(v, Visibility::Revealed);
    }

    #[test]
    fn fullscreen_suppresses_the_hot_edge_and_popup() {
        let mut c = controller();
        tick(&mut c, 0, Some((960.0, 0.0)), false, false);
        assert_eq!(
            tick(&mut c, 1, Some((960.0, 0.0)), true, true),
            Visibility::Collapsed
        );
        assert_eq!(
            tick(&mut c, 2, Some((960.0, 0.0)), false, true),
            Visibility::Collapsed
        );
        assert_eq!(
            tick(&mut c, 3, Some((960.0, 0.0)), false, false),
            Visibility::Revealed
        );
    }
}
