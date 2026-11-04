//! The widget contract: a compact view for the panel, an optional popup, and messages.

use std::time::Instant;

use crate::{Element, Theme};

/// What a widget sees while building its views.
pub struct Context<'a> {
    pub theme: &'a Theme,
    pub now: Instant,
    /// This widget's popup is open (`plasmoid.expanded`).
    pub popup_open: bool,
    /// Panel thickness in logical pixels (the compact view's height).
    pub panel_height: f32,
}

/// Side effects a widget asks the panel for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Command {
    #[default]
    None,
    OpenPopup,
    ClosePopup,
    TogglePopup,
    /// Exit the host event loop.
    Quit,
}

/// One plasmoid. Clicking the compact view toggles the popup unless an element inside the
/// compact view handles the click itself.
pub trait Widget {
    /// Cloned when the same element is clicked again before the next frame.
    type Message: Clone + 'static;

    /// Stable identifier, e.g. `"systempulse"`.
    fn id(&self) -> &str;

    /// Pull new data (from a [`crate::Sampler`], files, ...). Return `true` if anything changed.
    fn poll(&mut self, _now: Instant) -> bool {
        false
    }

    /// `true` while the widget runs its own animation and needs frames.
    fn animating(&self, _now: Instant) -> bool {
        false
    }

    /// `compactRepresentation`. Laid out with the panel height as its height.
    fn compact(&self, ctx: &Context<'_>) -> Element<Self::Message>;

    /// `fullRepresentation`. `None` disables the popup.
    fn popup(&self, _ctx: &Context<'_>) -> Option<Element<Self::Message>> {
        None
    }

    fn update(&mut self, _message: Self::Message, _now: Instant) -> Command {
        Command::None
    }
}

/// Type-erased widget: messages become indices into the last built views.
pub(crate) trait AnyWidget {
    fn id(&self) -> &str;
    fn poll(&mut self, now: Instant) -> bool;
    fn animating(&self, now: Instant) -> bool;
    fn begin_frame(&mut self);
    fn compact(&mut self, ctx: &Context<'_>) -> Element<usize>;
    fn popup(&mut self, ctx: &Context<'_>) -> Option<Element<usize>>;
    fn update(&mut self, message: usize, now: Instant) -> Command;
}

pub(crate) struct Erased<W: Widget> {
    widget: W,
    messages: Vec<W::Message>,
}

impl<W: Widget> Erased<W> {
    pub(crate) fn new(widget: W) -> Self {
        Self {
            widget,
            messages: Vec::new(),
        }
    }

    fn erase(messages: &mut Vec<W::Message>, element: Element<W::Message>) -> Element<usize> {
        element.map(&mut |m| {
            messages.push(m);
            messages.len() - 1
        })
    }
}

impl<W: Widget> AnyWidget for Erased<W> {
    fn id(&self) -> &str {
        self.widget.id()
    }

    fn poll(&mut self, now: Instant) -> bool {
        self.widget.poll(now)
    }

    fn animating(&self, now: Instant) -> bool {
        self.widget.animating(now)
    }

    fn begin_frame(&mut self) {
        self.messages.clear();
    }

    fn compact(&mut self, ctx: &Context<'_>) -> Element<usize> {
        let element = self.widget.compact(ctx);
        Self::erase(&mut self.messages, element)
    }

    fn popup(&mut self, ctx: &Context<'_>) -> Option<Element<usize>> {
        let element = self.widget.popup(ctx)?;
        Some(Self::erase(&mut self.messages, element))
    }

    /// An index that is not part of the last built views is ignored.
    fn update(&mut self, message: usize, now: Instant) -> Command {
        match self.messages.get(message).cloned() {
            Some(m) => self.widget.update(m, now),
            None => Command::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{column, text};

    struct Counter(u32);

    #[derive(Clone, Debug, PartialEq)]
    enum Msg {
        Add(u32),
        Quit,
    }

    impl Widget for Counter {
        type Message = Msg;
        fn id(&self) -> &str {
            "counter"
        }
        fn compact(&self, _: &Context<'_>) -> Element<Msg> {
            column()
                .child(text("+1").on_click(Msg::Add(1)))
                .child(text("+5").on_click(Msg::Add(5)))
        }
        fn popup(&self, _: &Context<'_>) -> Option<Element<Msg>> {
            Some(text("quit").on_click(Msg::Quit))
        }
        fn update(&mut self, m: Msg, _: Instant) -> Command {
            match m {
                Msg::Add(n) => {
                    self.0 += n;
                    Command::None
                }
                Msg::Quit => Command::Quit,
            }
        }
    }

    #[test]
    fn erased_messages_route_back_to_the_typed_widget() {
        let theme = Theme::kanagawa_wave();
        let now = Instant::now();
        let ctx = Context {
            theme: &theme,
            now,
            popup_open: true,
            panel_height: 36.0,
        };
        let mut w = Erased::new(Counter(0));
        w.begin_frame();
        let compact = w.compact(&ctx);
        let popup = w.popup(&ctx).unwrap();
        let plus5 = *compact.child_elements()[1].click_message().unwrap();
        let quit = *popup.click_message().unwrap();
        assert_eq!(w.update(plus5, now), Command::None);
        assert_eq!(w.widget.0, 5);
        // Second click before the next frame still works.
        assert_eq!(w.update(plus5, now), Command::None);
        assert_eq!(w.widget.0, 10);
        assert_eq!(w.update(quit, now), Command::Quit);
        assert_eq!(w.update(99, now), Command::None);
        w.begin_frame();
        assert_eq!(w.update(0, now), Command::None);
    }
}
