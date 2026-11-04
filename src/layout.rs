//! Element tree → absolute rectangles. Taffy does flexbox/grid; text and SVG leaves are
//! measured here. The result is a flat, paint-ordered list of nodes.

use std::collections::HashMap;
use std::rc::Rc;

use taffy::prelude::*;
use taffy::{LayoutInput, LayoutOutput, compute_leaf_layout};

use crate::element::{Content, Element, Paint, TextSpec};
use crate::text::TextSystem;
use crate::{Color, Rect};

enum Ctx {
    Text(TextSpec),
    Svg(f32, f32),
}

pub(crate) struct Node<A> {
    pub rect: Rect,
    pub clip: Rect,
    /// `clip` is narrower than the frame (inside a scroll area).
    pub clipped: bool,
    /// Product of this node's and all ancestors' opacity.
    pub opacity: f32,
    pub content: Content,
    pub painter: Option<Rc<dyn Paint>>,
    pub background: Option<Color>,
    pub hover_background: Option<Color>,
    pub border: Option<(Color, f32)>,
    pub radius: f32,
    pub action: Option<A>,
    /// Scroll key and maximum offset of a scroll area.
    pub scroll: Option<(String, f32)>,
}

/// Laid-out element tree in logical pixels, origin at the top-left of the root.
pub struct Frame<A> {
    pub(crate) nodes: Vec<Node<A>>,
    pub size: (f32, f32),
}

impl<A> Frame<A> {
    /// Topmost clickable node under the point.
    pub fn hit(&self, x: f32, y: f32) -> Option<&A> {
        self.nodes
            .iter()
            .rev()
            .filter(|n| n.rect.contains(x, y) && n.clip.contains(x, y))
            .find_map(|n| n.action.as_ref())
    }

    /// Innermost scroll area under the point: its key and maximum offset.
    pub fn scroll_area(&self, x: f32, y: f32) -> Option<(&str, f32)> {
        self.nodes
            .iter()
            .rev()
            .filter(|n| n.rect.contains(x, y) && n.clip.contains(x, y))
            .find_map(|n| n.scroll.as_ref().map(|(k, max)| (k.as_str(), *max)))
    }

    /// Whether any node under the point reacts to hover or clicks (cursor shape, repaint).
    pub fn interactive_at(&self, x: f32, y: f32) -> bool {
        self.nodes.iter().any(|n| {
            (n.action.is_some() || n.hover_background.is_some())
                && n.rect.contains(x, y)
                && n.clip.contains(x, y)
        })
    }

    /// Rectangle of the first text node with exactly this content (tests, screenshots).
    pub fn find_text(&self, needle: &str) -> Option<Rect> {
        self.nodes.iter().find_map(|n| match &n.content {
            Content::Text(spec) if spec.text == needle => Some(n.rect),
            _ => None,
        })
    }

    /// All visible text in paint order.
    pub fn texts(&self) -> Vec<&str> {
        self.nodes
            .iter()
            .filter_map(|n| match &n.content {
                Content::Text(spec) => Some(spec.text.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// Lays out `root`. `width`/`height` of `None` size the root to its content.
/// `scrolls` holds scroll offsets by key; offsets are clamped to the scrollable range.
pub fn layout<A>(
    root: Element<A>,
    width: Option<f32>,
    height: Option<f32>,
    text: &mut TextSystem,
    default_font_size: f32,
    scrolls: &HashMap<String, f32>,
) -> Frame<A> {
    let mut tree: TaffyTree<Ctx> = TaffyTree::new();
    let mut payloads = HashMap::new();
    let root_id = build(&mut tree, root, &mut payloads);
    let space = |v: Option<f32>| v.map_or(AvailableSpace::MaxContent, AvailableSpace::Definite);
    {
        let mut style = tree.style(root_id).expect("root").clone();
        if let Some(w) = width {
            style.size.width = length(w);
        }
        if let Some(h) = height {
            style.size.height = length(h);
        }
        tree.set_style(root_id, style).expect("root style");
    }
    tree.compute_layout_with_measure(
        root_id,
        Size {
            width: space(width),
            height: space(height),
        },
        |inputs: LayoutInput, _id, ctx: Option<&mut Ctx>, style: &Style| -> LayoutOutput {
            compute_leaf_layout(
                inputs,
                style,
                |_, _| 0.0,
                |known, available| measure(ctx.as_deref(), known, available, text, default_font_size),
            )
        },
    )
    .expect("layout");

    let root_layout = *tree.layout(root_id).expect("root layout");
    let size = (root_layout.size.width, root_layout.size.height);
    let frame_rect = Rect::new(0.0, 0.0, size.0, size.1);
    let mut nodes = Vec::with_capacity(payloads.len());
    flatten(
        &tree,
        root_id,
        (0.0, 0.0),
        frame_rect,
        false,
        1.0,
        &mut payloads,
        scrolls,
        &mut nodes,
    );
    Frame { nodes, size }
}

fn measure(
    ctx: Option<&Ctx>,
    known: Size<Option<f32>>,
    available: Size<AvailableSpace>,
    text: &mut TextSystem,
    default_font_size: f32,
) -> Size<f32> {
    if let (Some(width), Some(height)) = (known.width, known.height) {
        return Size { width, height };
    }
    let (w, h) = match ctx {
        None => (0.0, 0.0),
        Some(Ctx::Svg(w, h)) => match (known.width, known.height) {
            (Some(kw), None) => (kw, kw * h / w.max(f32::EPSILON)),
            (None, Some(kh)) => (kh * w / h.max(f32::EPSILON), kh),
            _ => (*w, *h),
        },
        Some(Ctx::Text(spec)) => {
            if spec.wrap {
                let wrap_width = known.width.or(match available.width {
                    AvailableSpace::Definite(w) => Some(w),
                    AvailableSpace::MinContent => Some(0.0),
                    AvailableSpace::MaxContent => None,
                });
                let (w, h) = match wrap_width {
                    Some(ww) => text.measure(spec, default_font_size, Some(ww)),
                    None => {
                        let mut single = spec.clone();
                        single.wrap = false;
                        text.measure(&single, default_font_size, None)
                    }
                };
                (w, h)
            } else {
                let (w, h) = text.measure(spec, default_font_size, None);
                if spec.elide && available.width == AvailableSpace::MinContent {
                    (0.0, h)
                } else {
                    (w, h)
                }
            }
        }
    };
    Size {
        width: known.width.unwrap_or(w),
        height: known.height.unwrap_or(h),
    }
}

struct Payload<A> {
    content: Content,
    painter: Option<Rc<dyn Paint>>,
    background: Option<Color>,
    hover_background: Option<Color>,
    border: Option<(Color, f32)>,
    radius: f32,
    opacity: f32,
    action: Option<A>,
    scroll_key: Option<String>,
    hidden: bool,
}

fn build<A>(
    tree: &mut TaffyTree<Ctx>,
    element: Element<A>,
    payloads: &mut HashMap<NodeId, Payload<A>>,
) -> NodeId {
    let Element {
        style,
        content,
        painter,
        background,
        hover_background,
        border,
        radius,
        opacity,
        children,
        on_click,
        scroll_key,
    } = element;
    let hidden = style.display == Display::None;
    let ctx = match &content {
        Content::None => None,
        Content::Text(spec) => Some(Ctx::Text(spec.clone())),
        Content::Svg(t, _) => Some(Ctx::Svg(t.size().width(), t.size().height())),
    };
    let id = match ctx {
        Some(ctx) => tree.new_leaf_with_context(style, ctx),
        None => tree.new_leaf(style),
    }
    .expect("taffy node");
    payloads.insert(
        id,
        Payload {
            content,
            painter,
            background,
            hover_background,
            border,
            radius,
            opacity,
            action: on_click,
            scroll_key,
            hidden,
        },
    );
    let child_ids: Vec<NodeId> = children.into_iter().map(|c| build(tree, c, payloads)).collect();
    tree.set_children(id, &child_ids).expect("taffy children");
    id
}

#[allow(clippy::too_many_arguments)]
fn flatten<A>(
    tree: &TaffyTree<Ctx>,
    id: NodeId,
    origin: (f32, f32),
    clip: Rect,
    clipped: bool,
    opacity: f32,
    payloads: &mut HashMap<NodeId, Payload<A>>,
    scrolls: &HashMap<String, f32>,
    out: &mut Vec<Node<A>>,
) {
    let Some(payload) = payloads.remove(&id) else {
        return;
    };
    if payload.hidden {
        return;
    }
    let layout = tree.layout(id).expect("layout");
    let rect = Rect::new(
        origin.0 + layout.location.x,
        origin.1 + layout.location.y,
        layout.size.width,
        layout.size.height,
    );
    let opacity = opacity * payload.opacity;
    let children = tree.children(id).expect("children");
    let (scroll, child_clip, child_clipped, offset) = match payload.scroll_key {
        Some(key) => {
            let content_bottom = children
                .iter()
                .map(|&c| {
                    let l = tree.layout(c).expect("layout");
                    l.location.y + l.size.height + l.margin.bottom
                })
                .fold(0.0f32, f32::max)
                + layout.padding.bottom
                + layout.border.bottom;
            let max = (content_bottom - layout.size.height).max(0.0);
            let offset = scrolls.get(&key).copied().unwrap_or(0.0).clamp(0.0, max);
            (Some((key, max)), clip.intersect(rect), true, offset)
        }
        None => (None, clip, clipped, 0.0),
    };
    out.push(Node {
        rect,
        clip,
        clipped,
        opacity,
        content: payload.content,
        painter: payload.painter,
        background: payload.background,
        hover_background: payload.hover_background,
        border: payload.border,
        radius: payload.radius,
        action: payload.action,
        scroll,
    });
    for child in children {
        flatten(
            tree,
            child,
            (rect.x, rect.y - offset),
            child_clip,
            child_clipped,
            opacity,
            payloads,
            scrolls,
            out,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::{column, container, grid, row, spacer, text};
    use crate::text::tests::test_text;

    fn run<A>(root: Element<A>, w: Option<f32>, h: Option<f32>) -> Option<Frame<A>> {
        let mut ts = test_text()?;
        Some(layout(root, w, h, &mut ts, 13.0, &HashMap::new()))
    }

    #[test]
    fn row_with_spacer_pushes_last_child_to_the_end() {
        let tree: Element<()> = row()
            .child(container().size(10.0, 10.0))
            .child(spacer())
            .child(container().size(20.0, 10.0).on_click(()));
        let mut ts = TextSystem::from_font_data(Vec::new(), &[]);
        let f = layout(tree, Some(100.0), Some(10.0), &mut ts, 13.0, &HashMap::new());
        assert_eq!(f.size, (100.0, 10.0));
        assert_eq!(f.nodes.last().unwrap().rect, Rect::new(80.0, 0.0, 20.0, 10.0));
        assert!(f.hit(85.0, 5.0).is_some());
        assert!(f.hit(50.0, 5.0).is_none());
    }

    #[test]
    fn grid_columns_share_width_and_span_covers_both() {
        let tree: Element<()> = grid(2)
            .gap(10.0)
            .child(container().height(5.0))
            .child(container().height(5.0))
            .child(container().height(5.0).span(2));
        let mut ts = TextSystem::from_font_data(Vec::new(), &[]);
        let f = layout(tree, Some(110.0), None, &mut ts, 13.0, &HashMap::new());
        let r: Vec<Rect> = f.nodes.iter().skip(1).map(|n| n.rect).collect();
        assert_eq!(r[0], Rect::new(0.0, 0.0, 50.0, 5.0));
        assert_eq!(r[1], Rect::new(60.0, 0.0, 50.0, 5.0));
        assert_eq!(r[2], Rect::new(0.0, 15.0, 110.0, 5.0));
        assert_eq!(f.size, (110.0, 20.0));
    }

    #[test]
    fn hidden_elements_take_no_space_and_are_not_painted() {
        let tree: Element<()> = column()
            .child(container().height(10.0).visible(false))
            .child(container().height(7.0));
        let mut ts = TextSystem::from_font_data(Vec::new(), &[]);
        let f = layout(tree, Some(10.0), None, &mut ts, 13.0, &HashMap::new());
        assert_eq!(f.nodes.len(), 2);
        assert_eq!(f.nodes[1].rect.y, 0.0);
        assert_eq!(f.size.1, 7.0);
    }

    #[test]
    fn scroll_offset_is_clamped_and_children_are_clipped() {
        let tree: Element<u8> = column()
            .height(50.0)
            .scrollable("list")
            .children((0..10).map(|i| container().height(20.0).on_click(i)));
        let mut ts = TextSystem::from_font_data(Vec::new(), &[]);
        let mut scrolls = HashMap::new();
        scrolls.insert("list".to_owned(), 1000.0);
        let f = layout(tree, Some(10.0), None, &mut ts, 13.0, &scrolls);
        assert_eq!(f.scroll_area(5.0, 5.0), Some(("list", 150.0)));
        // Bottom of the list is visible, item 9 sits at the bottom edge.
        assert_eq!(f.hit(5.0, 45.0), Some(&9));
        // Items scrolled above the area cannot be hit from outside the clip.
        assert_eq!(f.hit(5.0, -5.0), None);
        assert!(f.nodes[1].clipped);
    }

    #[test]
    fn opacity_multiplies_down_the_tree() {
        let tree: Element<()> = column()
            .opacity(0.5)
            .child(container().opacity(0.5).size(1.0, 1.0));
        let mut ts = TextSystem::from_font_data(Vec::new(), &[]);
        let f = layout(tree, None, None, &mut ts, 13.0, &HashMap::new());
        assert_eq!(f.nodes[1].opacity, 0.25);
    }

    #[test]
    fn text_is_measured_and_wraps_inside_a_narrow_column() {
        let long = "word ".repeat(20);
        let Some(single) = run::<()>(row().child(text(long.trim())), None, None) else {
            return;
        };
        let Some(wrapped) = run::<()>(column().child(text(long.trim()).wrap()), Some(80.0), None) else {
            return;
        };
        assert!(single.size.0 > 80.0);
        assert!(wrapped.size.1 > single.size.1 * 2.0);
        assert!(wrapped.nodes[1].rect.w <= 80.0);
    }

    #[test]
    fn elided_text_shrinks_to_its_row_instead_of_overflowing() {
        let Some(f) = run::<()>(
            row()
                .child(text("a fairly long label that does not fit").elide())
                .child(container().size(30.0, 10.0)),
            Some(100.0),
            None,
        ) else {
            return;
        };
        assert!(f.nodes[1].rect.w <= 70.0 + 0.5);
        assert_eq!(f.nodes[2].rect.x + f.nodes[2].rect.w, 100.0);
        assert_eq!(f.texts(), vec!["a fairly long label that does not fit"]);
    }
}
