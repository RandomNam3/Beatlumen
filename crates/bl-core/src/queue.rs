//! Play queues (audio tracks and background images) with shuffle and
//! repeat.

use crate::settings::Repeat;
use bl_fx::Rng;

#[derive(Debug, Clone, Default)]
pub struct Queue {
    /// Items in insertion order.
    pub items: Vec<String>,
    /// Playback order (indices into `items`).
    order: Vec<usize>,
    /// Position in `order`.
    pos: Option<usize>,
    pub shuffle: bool,
}

impl Queue {
    pub fn new(items: Vec<String>, current: Option<usize>, shuffle: bool, rng: &mut Rng) -> Queue {
        let mut q = Queue {
            order: (0..items.len()).collect(),
            items,
            pos: None,
            shuffle: false,
        };
        if let Some(c) = current.filter(|&c| c < q.items.len()) {
            q.pos = Some(c);
        }
        if shuffle {
            q.set_shuffle(true, rng);
        }
        q
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Index (into `items`) of the current item.
    pub fn current_index(&self) -> Option<usize> {
        self.pos.and_then(|p| self.order.get(p).copied())
    }

    pub fn current(&self) -> Option<&String> {
        self.current_index().and_then(|i| self.items.get(i))
    }

    /// Items in playback order with their item indices.
    pub fn ordered(&self) -> impl Iterator<Item = (usize, &String)> {
        self.order.iter().map(move |&i| (i, &self.items[i]))
    }

    /// Position of the current item in playback order.
    pub fn position(&self) -> Option<usize> {
        self.pos
    }

    pub fn set_current(&mut self, item_index: usize) {
        if let Some(p) = self.order.iter().position(|&i| i == item_index) {
            self.pos = Some(p);
        }
    }

    pub fn set_position(&mut self, pos: usize) {
        if pos < self.order.len() {
            self.pos = Some(pos);
        }
    }

    /// Advances; returns the new current item (None when the end was
    /// reached without repeat).
    pub fn next(&mut self, repeat: Repeat) -> Option<&String> {
        if self.order.is_empty() {
            return None;
        }
        let n = self.order.len();
        let p = match self.pos {
            None => 0,
            Some(p) if p + 1 < n => p + 1,
            Some(_) if repeat == Repeat::Queue => 0,
            Some(_) => return None,
        };
        self.pos = Some(p);
        self.current()
    }

    /// The item `next` would advance to, without advancing.
    pub fn peek_next(&self, repeat: Repeat) -> Option<&String> {
        let n = self.order.len();
        let p = match self.pos {
            _ if n == 0 => return None,
            None => 0,
            Some(p) if p + 1 < n => p + 1,
            Some(_) if repeat == Repeat::Queue => 0,
            Some(_) => return None,
        };
        self.items.get(self.order[p])
    }

    pub fn prev(&mut self, repeat: Repeat) -> Option<&String> {
        if self.order.is_empty() {
            return None;
        }
        let n = self.order.len();
        let p = match self.pos {
            None => 0,
            Some(0) if repeat == Repeat::Queue => n - 1,
            Some(0) => 0,
            Some(p) => p - 1,
        };
        self.pos = Some(p);
        self.current()
    }

    pub fn has_next(&self, repeat: Repeat) -> bool {
        match self.pos {
            None => !self.order.is_empty(),
            Some(p) => {
                p + 1 < self.order.len() || (repeat == Repeat::Queue && !self.order.is_empty())
            }
        }
    }

    pub fn set_shuffle(&mut self, on: bool, rng: &mut Rng) {
        let cur = self.current_index();
        self.shuffle = on;
        self.order = (0..self.items.len()).collect();
        if on {
            for i in (1..self.order.len()).rev() {
                let j = (rng.next_u64() % (i as u64 + 1)) as usize;
                self.order.swap(i, j);
            }
            if let Some(c) = cur {
                if let Some(p) = self.order.iter().position(|&i| i == c) {
                    self.order.swap(0, p);
                }
            }
        }
        self.pos = cur.and_then(|c| self.order.iter().position(|&i| i == c));
    }

    /// Appends items; returns the item index of the first appended item.
    pub fn push(&mut self, items: impl IntoIterator<Item = String>, rng: &mut Rng) -> usize {
        let first = self.items.len();
        for it in items {
            self.items.push(it);
            let idx = self.items.len() - 1;
            if self.shuffle && self.pos.is_some() {
                // Insert at a random position after the current one.
                let p = self.pos.unwrap_or(0);
                let span = self.order.len() - p;
                let at = p + 1 + (rng.next_u64() % span as u64) as usize;
                self.order.insert(at.min(self.order.len()), idx);
            } else {
                self.order.push(idx);
            }
        }
        first
    }

    /// Inserts items to play right after the current one.
    pub fn play_next(&mut self, items: Vec<String>) {
        let mut at = self.pos.map(|p| p + 1).unwrap_or(0);
        for it in items {
            self.items.push(it);
            let idx = self.items.len() - 1;
            self.order.insert(at.min(self.order.len()), idx);
            at += 1;
        }
    }

    /// Removes the item at playback position `pos`.
    pub fn remove_at(&mut self, pos: usize) {
        if pos >= self.order.len() {
            return;
        }
        let idx = self.order.remove(pos);
        self.items.remove(idx);
        for o in &mut self.order {
            if *o > idx {
                *o -= 1;
            }
        }
        self.pos = match self.pos {
            Some(p) if p > pos => Some(p - 1),
            Some(p) if p == pos => {
                if self.order.is_empty() {
                    None
                } else {
                    Some(p.min(self.order.len() - 1))
                }
            }
            other => other,
        };
    }

    /// Moves the entry at playback position `from` to `to`.
    pub fn move_entry(&mut self, from: usize, to: usize) {
        if from >= self.order.len() || to >= self.order.len() || from == to {
            return;
        }
        let cur = self.current_index();
        let v = self.order.remove(from);
        self.order.insert(to, v);
        self.pos = cur.and_then(|c| self.order.iter().position(|&i| i == c));
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.order.clear();
        self.pos = None;
    }

    /// Replaces all items and starts at item `start`.
    pub fn replace(&mut self, items: Vec<String>, start: usize, rng: &mut Rng) {
        self.items = items;
        self.order = (0..self.items.len()).collect();
        self.pos = if self.items.is_empty() {
            None
        } else {
            Some(start.min(self.items.len() - 1))
        };
        if self.shuffle {
            self.set_shuffle(true, rng);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(n: usize) -> Queue {
        Queue::new(
            (0..n).map(|i| i.to_string()).collect(),
            None,
            false,
            &mut Rng::new(1),
        )
    }

    #[test]
    fn next_prev_repeat() {
        let mut a = q(3);
        assert_eq!(a.next(Repeat::None).unwrap(), "0");
        assert_eq!(a.next(Repeat::None).unwrap(), "1");
        assert_eq!(a.next(Repeat::None).unwrap(), "2");
        assert!(a.next(Repeat::None).is_none());
        assert_eq!(a.next(Repeat::Queue).unwrap(), "0");
        assert_eq!(a.prev(Repeat::Queue).unwrap(), "2");
    }

    #[test]
    fn shuffle_keeps_current_first() {
        let mut rng = Rng::new(4);
        let mut a = q(20);
        a.set_current(7);
        a.set_shuffle(true, &mut rng);
        assert_eq!(a.position(), Some(0));
        assert_eq!(a.current().unwrap(), "7");
        let mut seen: Vec<usize> = a.ordered().map(|(i, _)| i).collect();
        seen.sort();
        assert_eq!(seen, (0..20).collect::<Vec<_>>());
        a.set_shuffle(false, &mut rng);
        assert_eq!(a.position(), Some(7));
    }

    #[test]
    fn remove_and_move() {
        let mut a = q(5);
        a.set_current(2);
        a.remove_at(0);
        assert_eq!(a.current().unwrap(), "2");
        a.move_entry(1, 3);
        assert_eq!(a.current().unwrap(), "2");
        a.remove_at(a.position().unwrap());
        assert_eq!(a.len(), 3);
        a.play_next(vec!["x".into()]);
        assert!(a.ordered().any(|(_, s)| s == "x"));
    }
}
