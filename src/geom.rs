/// Rectangle in logical pixels (device-independent, 96 DPI = 1.0 scale).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(self) -> f32 {
        self.y + self.h
    }

    pub fn center_x(self) -> f32 {
        self.x + self.w / 2.0
    }

    /// Right and bottom edges are exclusive.
    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    pub fn translate(self, dx: f32, dy: f32) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            ..self
        }
    }

    pub fn inset(self, by: f32) -> Self {
        Self {
            x: self.x + by,
            y: self.y + by,
            w: (self.w - 2.0 * by).max(0.0),
            h: (self.h - 2.0 * by).max(0.0),
        }
    }

    pub fn intersect(self, other: Self) -> Self {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        Self {
            x,
            y,
            w: (self.right().min(other.right()) - x).max(0.0),
            h: (self.bottom().min(other.bottom()) - y).max(0.0),
        }
    }

    pub fn is_empty(self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_half_open() {
        let r = Rect::new(-2.0, 1.0, 4.0, 2.0);
        assert!(r.contains(-2.0, 1.0));
        assert!(r.contains(1.9, 2.9));
        assert!(!r.contains(2.0, 2.0));
        assert!(!r.contains(0.0, 3.0));
    }

    #[test]
    fn intersect_and_inset_never_go_negative() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert_eq!(
            a.intersect(Rect::new(5.0, -5.0, 10.0, 10.0)),
            Rect::new(5.0, 0.0, 5.0, 5.0)
        );
        assert!(a.intersect(Rect::new(20.0, 0.0, 1.0, 1.0)).is_empty());
        assert_eq!(a.inset(8.0), Rect::new(8.0, 8.0, 0.0, 0.0));
    }
}
