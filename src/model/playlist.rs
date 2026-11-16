//! Playlists: ordered scenes with optional per-item durations.

use serde::{Deserialize, Serialize};

use crate::model::new_id;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaylistItem {
    pub scene_id: String,
    /// Seconds before auto-advance; 0 means "hold" (or media end).
    pub duration_secs: u32,
}

impl Default for PlaylistItem {
    fn default() -> Self {
        Self {
            scene_id: String::new(),
            duration_secs: 0,
        }
    }
}

impl PlaylistItem {
    pub fn new(scene_id: impl Into<String>, duration_secs: u32) -> Self {
        Self {
            scene_id: scene_id.into(),
            duration_secs,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub r#loop: bool,
    pub items: Vec<PlaylistItem>,
}

impl Default for Playlist {
    fn default() -> Self {
        Self {
            id: new_id(),
            name: String::new(),
            r#loop: true,
            items: Vec::new(),
        }
    }
}

impl Playlist {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn scene_id_at(&self, index: usize) -> Option<&str> {
        self.items.get(index).map(|item| item.scene_id.as_str())
    }

    pub fn duration_at(&self, index: usize) -> u32 {
        self.items.get(index).map_or(0, |item| item.duration_secs)
    }

    /// Next index honouring loop; `None` when already at the end without loop.
    pub fn advance_index(&self, index: usize) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        if index + 1 < self.items.len() {
            Some(index + 1)
        } else if self.r#loop {
            Some(0)
        } else {
            None
        }
    }

    /// Previous index honouring loop.
    pub fn retreat_index(&self, index: usize) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        if index > 0 {
            Some(index - 1)
        } else if self.r#loop {
            Some(self.items.len() - 1)
        } else {
            None
        }
    }

    /// Drop items whose scene no longer exists; returns how many were removed.
    pub fn retain_existing_scenes(&mut self, exists: impl Fn(&str) -> bool) -> usize {
        let before = self.items.len();
        self.items.retain(|item| exists(&item.scene_id));
        before - self.items.len()
    }

    pub fn remove_scene(&mut self, scene_id: &str) -> usize {
        let before = self.items.len();
        self.items.retain(|item| item.scene_id != scene_id);
        before - self.items.len()
    }

    pub fn move_item(&mut self, from: usize, to: usize) -> bool {
        if from >= self.items.len() || to >= self.items.len() || from == to {
            return false;
        }
        let item = self.items.remove(from);
        self.items.insert(to, item);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playlist(items: &[(&str, u32)], r#loop: bool) -> Playlist {
        Playlist {
            items: items
                .iter()
                .map(|(id, d)| PlaylistItem::new(*id, *d))
                .collect(),
            r#loop,
            ..Playlist::new("p")
        }
    }

    #[test]
    fn advance_and_retreat_respect_loop() {
        let looping = playlist(&[("a", 0), ("b", 5)], true);
        assert_eq!(looping.advance_index(0), Some(1));
        assert_eq!(looping.advance_index(1), Some(0));
        assert_eq!(looping.retreat_index(0), Some(1));
        assert_eq!(looping.duration_at(1), 5);

        let single = playlist(&[("a", 0), ("b", 5)], false);
        assert_eq!(single.advance_index(1), None);
        assert_eq!(single.retreat_index(0), None);
        assert_eq!(single.advance_index(0), Some(1));

        let empty = playlist(&[], true);
        assert_eq!(empty.advance_index(0), None);
    }

    #[test]
    fn removing_scenes_and_reordering() {
        let mut p = playlist(&[("a", 0), ("b", 0), ("c", 0)], true);
        assert_eq!(p.remove_scene("b"), 1);
        assert_eq!(p.items.len(), 2);
        assert!(p.move_item(0, 1));
        assert_eq!(p.scene_id_at(0), Some("c"));
        assert!(!p.move_item(0, 9));
        assert_eq!(p.retain_existing_scenes(|id| id == "c"), 1);
    }

    #[test]
    fn playlist_json_uses_loop_key() {
        let p = Playlist::new("Show");
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"loop\":true"));
        let back: Playlist = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }
}
