//! Bookkeeping for GPU textures: ids, sizes, reference counts, a memory
//! budget and least-recently-used eviction.
//!
//! The table is backend-neutral: owners create and reference textures here,
//! and the backend mirrors the [`ResourceEvent`]s into real allocations.
//! Owners keep a reference on what they need; a texture nobody references
//! stays as a cache entry until the budget needs its memory.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TextureId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureFormat {
    /// One coverage byte per texel (glyph atlas pages).
    R8,
    /// Straight RGBA, 8 bits per channel (images).
    Rgba8,
}

impl TextureFormat {
    pub fn bytes_per_texel(self) -> u64 {
        match self {
            TextureFormat::R8 => 1,
            TextureFormat::Rgba8 => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureDesc {
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
}

impl TextureDesc {
    pub fn bytes(&self) -> u64 {
        self.width as u64 * self.height as u64 * self.format.bytes_per_texel()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceEvent {
    Created(TextureId, TextureDesc),
    Destroyed(TextureId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverBudget;

#[derive(Debug)]
struct Entry {
    desc: TextureDesc,
    refs: u32,
    last_used: u64,
    /// Memory someone else owns (an imported surface): outside the budget.
    external: bool,
}

#[derive(Debug)]
pub struct Resources {
    budget: u64,
    used: u64,
    frame: u64,
    next_id: u32,
    entries: HashMap<TextureId, Entry>,
    events: Vec<ResourceEvent>,
}

impl Resources {
    /// A table allowing `budget` bytes of textures.
    pub fn new(budget: u64) -> Resources {
        Resources { budget, used: 0, frame: 0, next_id: 1, entries: HashMap::new(), events: Vec::new() }
    }

    pub fn budget(&self) -> u64 {
        self.budget
    }

    /// Bytes currently allocated.
    pub fn used(&self) -> u64 {
        self.used
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, id: TextureId) -> bool {
        self.entries.contains_key(&id)
    }

    /// Start a new frame; textures used in it are not evicted during it.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    /// A new texture with one reference, evicting unreferenced textures
    /// (least recently used first) to make room.
    pub fn create(&mut self, desc: TextureDesc) -> Result<TextureId, OverBudget> {
        let need = desc.bytes();
        if need > self.budget {
            return Err(OverBudget);
        }
        while self.used + need > self.budget {
            let victim = self
                .entries
                .iter()
                .filter(|(_, e)| e.refs == 0 && e.last_used < self.frame && !e.external)
                .min_by_key(|(id, e)| (e.last_used, **id))
                .map(|(&id, _)| id);
            match victim {
                Some(id) => self.destroy(id),
                None => return Err(OverBudget),
            }
        }
        let id = TextureId(self.next_id);
        self.next_id += 1;
        self.used += need;
        self.entries.insert(id, Entry { desc, refs: 1, last_used: self.frame, external: false });
        self.events.push(ResourceEvent::Created(id, desc));
        Ok(id)
    }

    /// An id for a texture the backend makes from memory it imports (no
    /// `Created` event, outside the budget, never evicted); destroying it
    /// still sends `Destroyed`.
    pub fn external(&mut self, desc: TextureDesc) -> TextureId {
        let id = TextureId(self.next_id);
        self.next_id += 1;
        self.entries.insert(id, Entry { desc, refs: 1, last_used: self.frame, external: true });
        id
    }

    pub fn retain(&mut self, id: TextureId) {
        if let Some(e) = self.entries.get_mut(&id) {
            e.refs += 1;
        }
    }

    /// Drop a reference; at zero the texture becomes evictable (it is kept
    /// while the budget allows, in case it is wanted again).
    pub fn release(&mut self, id: TextureId) {
        if let Some(e) = self.entries.get_mut(&id) {
            e.refs = e.refs.saturating_sub(1);
        }
    }

    /// Mark `id` as used by the current frame.
    pub fn touch(&mut self, id: TextureId) {
        if let Some(e) = self.entries.get_mut(&id) {
            e.last_used = self.frame;
        }
    }

    /// Free a texture now, whatever its references.
    pub fn destroy(&mut self, id: TextureId) {
        if let Some(e) = self.entries.remove(&id) {
            if !e.external {
                self.used -= e.desc.bytes();
            }
            self.events.push(ResourceEvent::Destroyed(id));
        }
    }

    /// Changes since the last call, for the backend to apply in order.
    pub fn take_events(&mut self) -> Vec<ResourceEvent> {
        std::mem::take(&mut self.events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: TextureDesc = TextureDesc { width: 16, height: 16, format: TextureFormat::R8 };

    #[test]
    fn budget_and_lru_eviction() {
        let mut r = Resources::new(PAGE.bytes() * 3);
        let a = r.create(PAGE).unwrap();
        let b = r.create(PAGE).unwrap();
        let c = r.create(PAGE).unwrap();
        assert_eq!(r.used(), PAGE.bytes() * 3);
        // Everything referenced: nothing can go.
        r.begin_frame();
        assert_eq!(r.create(PAGE), Err(OverBudget));
        // Unreferenced a and c; c was used more recently, so a goes first.
        r.release(a);
        r.release(c);
        r.begin_frame();
        r.touch(c);
        r.begin_frame();
        r.take_events();
        let d = r.create(PAGE).unwrap();
        assert_eq!(r.take_events(), [ResourceEvent::Destroyed(a), ResourceEvent::Created(d, PAGE)]);
        assert!(!r.contains(a) && r.contains(b) && r.contains(c));
        // Textures used this frame survive even when unreferenced.
        r.touch(c);
        assert_eq!(r.create(PAGE), Err(OverBudget));
        r.begin_frame();
        assert!(r.create(PAGE).is_ok());
        assert!(!r.contains(c));
        // Retained again: safe.
        r.retain(b);
        r.release(b);
        r.begin_frame();
        assert_eq!(r.create(PAGE), Err(OverBudget));
        // Bigger than the whole budget.
        let huge = TextureDesc { width: 64, height: 64, format: TextureFormat::Rgba8 };
        assert_eq!(r.create(huge), Err(OverBudget));
        r.destroy(b);
        assert_eq!(r.used(), PAGE.bytes() * 2);
    }

    #[test]
    fn external_textures_are_outside_the_budget() {
        let mut r = Resources::new(PAGE.bytes());
        let huge = TextureDesc { width: 4096, height: 4096, format: TextureFormat::Rgba8 };
        let e = r.external(huge);
        assert_eq!((r.used(), r.take_events()), (0, vec![]), "the backend imports it");
        r.release(e);
        r.begin_frame();
        let a = r.create(PAGE).unwrap();
        r.release(a);
        r.begin_frame();
        assert!(r.create(PAGE).is_ok() && r.contains(e), "never evicted");
        r.take_events();
        r.destroy(e);
        assert_eq!((r.used(), r.take_events()), (PAGE.bytes(), vec![ResourceEvent::Destroyed(e)]));
    }
}
