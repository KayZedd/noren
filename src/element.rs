//! Declarative element tree. Widgets rebuild it from their state on every frame, the way QML
//! bindings re-evaluate; layout is flexbox/grid (taffy), painting is software (tiny-skia).

use std::fmt;
use std::rc::Rc;

use taffy::prelude::*;

use crate::render::Canvas;
use crate::{Color, Rect};

/// Custom drawing, the equivalent of a QML `Canvas` with `onPaint`.
pub trait Paint {
    fn paint(&self, canvas: &mut Canvas<'_>, rect: Rect);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextSpec {
    pub text: String,
    /// Logical pixels; `None` uses the theme's default font size.
    pub size: Option<f32>,
    pub weight: u16,
    /// `None` uses the theme text color.
    pub color: Option<Color>,
    pub wrap: bool,
    pub elide: bool,
    pub align: TextAlign,
}

#[derive(Clone)]
pub(crate) enum Content {
    None,
    Text(TextSpec),
    Svg(Rc<resvg::usvg::Tree>, Option<Color>),
}

/// One node of the UI tree. `M` is the widget's message type, produced by clicks.
#[derive(Clone)]
pub struct Element<M> {
    pub(crate) style: Style,
    pub(crate) content: Content,
    pub(crate) painter: Option<Rc<dyn Paint>>,
    pub(crate) background: Option<Color>,
    pub(crate) hover_background: Option<Color>,
    pub(crate) border: Option<(Color, f32)>,
    pub(crate) radius: f32,
    pub(crate) opacity: f32,
    pub(crate) children: Vec<Element<M>>,
    pub(crate) on_click: Option<M>,
    pub(crate) scroll_key: Option<String>,
}

impl<M> fmt::Debug for Element<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("Element");
        if let Content::Text(spec) = &self.content {
            d.field("text", &spec.text);
        }
        d.field("children", &self.children).finish_non_exhaustive()
    }
}

impl<M> Element<M> {
    /// QML layouts never squeeze fixed-size items, so elements do not shrink by default
    /// (CSS would). `grow`, `spacer`, `elide` and `wrap` opt back in.
    fn with_style(mut style: Style) -> Self {
        style.flex_shrink = 0.0;
        Self {
            style,
            content: Content::None,
            painter: None,
            background: None,
            hover_background: None,
            border: None,
            radius: 0.0,
            opacity: 1.0,
            children: Vec::new(),
            on_click: None,
            scroll_key: None,
        }
    }

    // ---- children -------------------------------------------------------------------

    pub fn child(mut self, child: Element<M>) -> Self {
        self.children.push(child);
        self
    }

    pub fn children(mut self, children: impl IntoIterator<Item = Element<M>>) -> Self {
        self.children.extend(children);
        self
    }

    // ---- layout ---------------------------------------------------------------------

    /// Escape hatch to the full flexbox/grid style.
    pub fn style(mut self, edit: impl FnOnce(&mut Style)) -> Self {
        edit(&mut self.style);
        self
    }

    pub fn padding(self, all: f32) -> Self {
        self.padding_edges(all, all, all, all)
    }

    pub fn padding_xy(self, x: f32, y: f32) -> Self {
        self.padding_edges(x, y, x, y)
    }

    pub fn padding_edges(mut self, left: f32, top: f32, right: f32, bottom: f32) -> Self {
        self.style.padding = taffy::Rect {
            left: length(left),
            right: length(right),
            top: length(top),
            bottom: length(bottom),
        };
        self
    }

    pub fn margin(mut self, all: f32) -> Self {
        self.style.margin = taffy::Rect::length(all);
        self
    }

    pub fn gap(mut self, gap: f32) -> Self {
        self.style.gap = Size::length(gap);
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        self.style.size.width = length(width);
        self
    }

    pub fn height(mut self, height: f32) -> Self {
        self.style.size.height = length(height);
        self
    }

    pub fn size(self, width: f32, height: f32) -> Self {
        self.width(width).height(height)
    }

    pub fn min_width(mut self, width: f32) -> Self {
        self.style.min_size.width = length(width);
        self
    }

    pub fn min_height(mut self, height: f32) -> Self {
        self.style.min_size.height = length(height);
        self
    }

    pub fn max_width(mut self, width: f32) -> Self {
        self.style.max_size.width = length(width);
        self
    }

    pub fn max_height(mut self, height: f32) -> Self {
        self.style.max_size.height = length(height);
        self
    }

    /// `Layout.fillWidth: true`.
    pub fn fill_width(mut self) -> Self {
        self.style.size.width = percent(1.0);
        self
    }

    /// `Layout.fillHeight: true`.
    pub fn fill_height(mut self) -> Self {
        self.style.size.height = percent(1.0);
        self
    }

    /// Take the remaining space along the parent's main axis.
    pub fn grow(mut self) -> Self {
        self.style.flex_grow = 1.0;
        self.style.flex_shrink = 1.0;
        self.style.flex_basis = length(0.0);
        self.style.min_size.width = length(0.0);
        self
    }

    /// Center children on both axes.
    pub fn center(mut self) -> Self {
        self.style.align_items = AlignItems::CENTER;
        self.style.justify_content = JustifyContent::CENTER;
        self
    }

    /// Center children on the cross axis only (vertical center in a row).
    pub fn align_center(mut self) -> Self {
        self.style.align_items = AlignItems::CENTER;
        self
    }

    pub fn justify_end(mut self) -> Self {
        self.style.justify_content = JustifyContent::END;
        self
    }

    pub fn justify_between(mut self) -> Self {
        self.style.justify_content = JustifyContent::SPACE_BETWEEN;
        self
    }

    /// `Layout.alignment: Qt.AlignCenter` for this item alone.
    pub fn self_center(mut self) -> Self {
        self.style.align_self = Some(AlignSelf::CENTER);
        self
    }

    /// `Layout.columnSpan` inside a [`grid`].
    pub fn span(mut self, columns: u16) -> Self {
        self.style.grid_column = span(columns);
        self
    }

    /// Positions the element against its parent's padding box, like QML anchors.
    pub fn absolute(
        mut self,
        left: Option<f32>,
        top: Option<f32>,
        right: Option<f32>,
        bottom: Option<f32>,
    ) -> Self {
        let edge = |v: Option<f32>| v.map_or(LengthPercentageAuto::auto(), length);
        self.style.position = Position::Absolute;
        self.style.inset = taffy::Rect {
            left: edge(left),
            top: edge(top),
            right: edge(right),
            bottom: edge(bottom),
        };
        self
    }

    /// `visible: false` removes the element from layout, as in QML layouts.
    pub fn visible(mut self, visible: bool) -> Self {
        if !visible {
            self.style.display = Display::None;
        }
        self
    }

    /// Vertical scrolling. `key` keeps the scroll offset across frames; the element needs a
    /// bounded height (`height`, `max_height`, or `grow` inside a bounded parent).
    pub fn scrollable(mut self, key: impl Into<String>) -> Self {
        self.style.overflow.y = taffy::Overflow::Scroll;
        self.style.scrollbar_width = 0.0;
        self.style.min_size.height = length(0.0);
        self.scroll_key = Some(key.into());
        self
    }

    // ---- visuals --------------------------------------------------------------------

    pub fn background(mut self, color: Color) -> Self {
        self.background = Some(color);
        self
    }

    /// Background drawn while the pointer is over the element.
    pub fn hover_background(mut self, color: Color) -> Self {
        self.hover_background = Some(color);
        self
    }

    pub fn border(mut self, color: Color, width: f32) -> Self {
        self.border = Some((color, width));
        self
    }

    pub fn radius(mut self, radius: f32) -> Self {
        self.radius = radius.max(0.0);
        self
    }

    /// Multiplies the alpha of this element and all descendants.
    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity.clamp(0.0, 1.0);
        self
    }

    /// Custom painter, drawn above the background and below children.
    pub fn painter(mut self, painter: impl Paint + 'static) -> Self {
        self.painter = Some(Rc::new(painter));
        self
    }

    pub fn on_click(mut self, message: M) -> Self {
        self.on_click = Some(message);
        self
    }

    // ---- text -----------------------------------------------------------------------

    fn text_spec(&mut self) -> &mut TextSpec {
        match &mut self.content {
            Content::Text(spec) => spec,
            _ => panic!("text styling applied to a non-text element"),
        }
    }

    pub fn font_size(mut self, size: f32) -> Self {
        self.text_spec().size = Some(size);
        self
    }

    pub fn bold(self) -> Self {
        self.weight(700)
    }

    pub fn weight(mut self, weight: u16) -> Self {
        self.text_spec().weight = weight;
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        match &mut self.content {
            Content::Text(spec) => spec.color = Some(color),
            Content::Svg(_, tint) => *tint = Some(color),
            Content::None => panic!("color applied to an element without text or icon"),
        }
        self
    }

    pub fn wrap(mut self) -> Self {
        self.text_spec().wrap = true;
        self.style.flex_shrink = 1.0;
        self
    }

    /// `elide: Text.ElideRight`. The element may shrink below its text width.
    pub fn elide(mut self) -> Self {
        self.text_spec().elide = true;
        self.style.min_size.width = length(0.0);
        self.style.flex_shrink = 1.0;
        self
    }

    pub fn text_align(mut self, align: TextAlign) -> Self {
        self.text_spec().align = align;
        self
    }

    // ---- composition ----------------------------------------------------------------

    /// Converts messages, for embedding a child component with its own message type.
    pub fn map<N>(self, f: &mut impl FnMut(M) -> N) -> Element<N> {
        Element {
            style: self.style,
            content: self.content,
            painter: self.painter,
            background: self.background,
            hover_background: self.hover_background,
            border: self.border,
            radius: self.radius,
            opacity: self.opacity,
            children: self.children.into_iter().map(|c| c.map(f)).collect(),
            on_click: self.on_click.map(&mut *f),
            scroll_key: self.scroll_key,
        }
    }

    pub fn text_content(&self) -> Option<&str> {
        match &self.content {
            Content::Text(spec) => Some(&spec.text),
            _ => None,
        }
    }

    pub fn child_elements(&self) -> &[Element<M>] {
        &self.children
    }

    pub fn click_message(&self) -> Option<&M> {
        self.on_click.as_ref()
    }
}

/// Empty box (QML `Item` / `Rectangle`). Children stack as a column.
pub fn container<M>() -> Element<M> {
    column()
}

/// `ColumnLayout`.
pub fn column<M>() -> Element<M> {
    Element::with_style(Style {
        flex_direction: FlexDirection::Column,
        ..Style::default()
    })
}

/// `RowLayout`.
pub fn row<M>() -> Element<M> {
    Element::with_style(Style {
        flex_direction: FlexDirection::Row,
        ..Style::default()
    })
}

/// `GridLayout { columns: n }` with equally wide columns.
pub fn grid<M>(columns: u16) -> Element<M> {
    Element::with_style(Style {
        display: Display::Grid,
        grid_template_columns: evenly_sized_tracks(columns.max(1)),
        ..Style::default()
    })
}

/// `Item { Layout.fillWidth: true }` inside a row, or fill height inside a column.
pub fn spacer<M>() -> Element<M> {
    let mut spacer = Element::with_style(Style {
        flex_grow: 1.0,
        flex_basis: length(0.0),
        min_size: Size {
            width: length(0.0),
            height: length(0.0),
        },
        ..Style::default()
    });
    spacer.style.flex_shrink = 1.0;
    spacer
}

/// `Label` / `Text`.
pub fn text<M>(text: impl Into<String>) -> Element<M> {
    let mut element = Element::with_style(Style::default());
    element.content = Content::Text(TextSpec {
        text: text.into(),
        size: None,
        weight: 400,
        color: None,
        wrap: false,
        elide: false,
        align: TextAlign::Left,
    });
    element
}

/// SVG icon. Use `.color()` to tint monochrome (symbolic) icons and `.size()` to scale.
pub fn svg<M>(data: &[u8]) -> Result<Element<M>, String> {
    let tree = resvg::usvg::Tree::from_data(data, &resvg::usvg::Options::default())
        .map_err(|e| format!("invalid SVG: {e}"))?;
    let mut element = Element::with_style(Style::default());
    element.content = Content::Svg(Rc::new(tree), None);
    Ok(element)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_converts_every_message_depth_first() {
        let tree: Element<u8> = row()
            .on_click(1)
            .child(text("a").on_click(2))
            .child(column().child(text("b").on_click(3)));
        let mut seen = Vec::new();
        let mapped = tree.map(&mut |m| {
            seen.push(m);
            format!("m{m}")
        });
        assert_eq!(seen, vec![2, 3, 1]);
        assert_eq!(mapped.click_message().map(String::as_str), Some("m1"));
        assert_eq!(
            mapped.child_elements()[1].child_elements()[0].text_content(),
            Some("b")
        );
    }

    #[test]
    fn invisible_elements_are_display_none() {
        let hidden: Element<()> = text("x").visible(false);
        assert_eq!(hidden.style.display, Display::None);
        let shown: Element<()> = text("x").visible(true);
        assert_ne!(shown.style.display, Display::None);
    }

    #[test]
    #[should_panic(expected = "non-text")]
    fn text_styling_on_a_box_is_a_programming_error() {
        let _: Element<()> = row().bold();
    }

    #[test]
    fn svg_rejects_invalid_data() {
        assert!(svg::<()>(b"not svg").is_err());
        let ok = svg::<()>(br#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"/>"#);
        assert!(ok.is_ok());
    }
}
