use std::collections::VecDeque;

/// Bounded sample history for sparklines. Non-finite samples count as 0 so the graph keeps
/// scrolling while a sensor is unavailable (same rule as the KDE widgets).
#[derive(Clone, Debug)]
pub struct History {
    capacity: usize,
    values: VecDeque<f32>,
}

impl History {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            values: VecDeque::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, value: f32) {
        if self.values.len() == self.capacity {
            self.values.pop_front();
        }
        self.values.push_back(if value.is_finite() { value } else { 0.0 });
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn values(&self) -> Vec<f32> {
        self.values.iter().copied().collect()
    }

    /// History maximum, never below `floor` (QML `Pulse.scaleMax`).
    pub fn scale_max(&self, floor: f32) -> f32 {
        self.values.iter().copied().fold(floor, f32::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_newest_values_up_to_capacity() {
        let mut h = History::new(3);
        for v in [1.0, 2.0, f32::NAN, 4.0] {
            h.push(v);
        }
        assert_eq!(h.values(), vec![2.0, 0.0, 4.0]);
        assert_eq!(h.len(), 3);
        assert_eq!(h.scale_max(10.0), 10.0);
        assert_eq!(h.scale_max(1.0), 4.0);
        assert_eq!(History::new(0).capacity(), 1);
    }
}
