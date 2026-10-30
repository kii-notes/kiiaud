// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::state::AudioSource;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Unique identifier for an item in the runtime queue (starts at 1, never 0, matching MPD semantics).
pub type QueueId = u32;

/// An item in the in-memory runtime playback queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackItem {
    pub id: QueueId,
    pub position: u32,
    pub source: AudioSource,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<Duration>,
}

impl PlaybackItem {
    pub fn display_title(&self) -> String {
        if let Some(title) = &self.title {
            return title.clone();
        }
        match &self.source {
            AudioSource::LocalFile(p) => p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Unknown")
                .to_owned(),
            AudioSource::HttpStream { url, .. } => url.clone(),
            AudioSource::TcpStream { host, port, .. } => format!("tcp://{host}:{port}"),
        }
    }
}

/// Pure in-memory runtime playback queue (`R-003`, `R-005`, `D-010`).
///
/// Requires zero database or disk I/O.
#[derive(Debug, Clone, Default)]
pub struct Queue {
    items: Vec<PlaybackItem>,
    next_id: QueueId,
    version: u32,
    current_index: Option<usize>,
}

impl Queue {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            next_id: 1,
            version: 0,
            current_index: None,
        }
    }

    fn allocate_id(&mut self) -> QueueId {
        if self.next_id == 0 {
            self.next_id = 1;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    fn reindex(&mut self) {
        for (idx, item) in self.items.iter_mut().enumerate() {
            item.position = idx as u32;
        }
    }

    /// Add a new source to the end of the queue and return its assigned `QueueId`.
    pub fn enqueue(
        &mut self,
        source: AudioSource,
        title: Option<String>,
        artist: Option<String>,
    ) -> QueueId {
        let id = self.allocate_id();
        let position = self.items.len() as u32;
        self.items.push(PlaybackItem {
            id,
            position,
            source,
            title,
            artist,
            album: None,
            duration: None,
        });
        self.version = self.version.wrapping_add(1);
        id
    }

    /// Replace the entire queue with a single item and select it as current.
    pub fn load_single(
        &mut self,
        source: AudioSource,
        title: Option<String>,
        artist: Option<String>,
    ) -> PlaybackItem {
        self.items.clear();
        let id = self.allocate_id();
        let item = PlaybackItem {
            id,
            position: 0,
            source,
            title,
            artist,
            album: None,
            duration: None,
        };
        self.items.push(item.clone());
        self.current_index = Some(0);
        self.version = self.version.wrapping_add(1);
        item
    }

    /// Clear all items from the queue.
    pub fn clear(&mut self) {
        self.items.clear();
        self.current_index = None;
        self.version = self.version.wrapping_add(1);
    }

    /// Remove an item by its 0-based position.
    pub fn remove_pos(&mut self, position: u32) -> Option<PlaybackItem> {
        let idx = position as usize;
        if idx >= self.items.len() {
            return None;
        }
        let removed = self.items.remove(idx);
        self.reindex();
        self.version = self.version.wrapping_add(1);

        if let Some(cur) = self.current_index {
            if self.items.is_empty() {
                self.current_index = None;
            } else if idx < cur {
                self.current_index = Some(cur - 1);
            } else if idx == cur {
                if cur >= self.items.len() {
                    self.current_index = Some(self.items.len() - 1);
                }
            }
        }
        Some(removed)
    }

    /// Remove an item by its unique `QueueId`.
    pub fn remove_id(&mut self, id: QueueId) -> Option<PlaybackItem> {
        let pos = self.items.iter().position(|item| item.id == id)?;
        self.remove_pos(pos as u32)
    }

    /// Select track at 0-based position.
    pub fn select_pos(&mut self, position: u32) -> Option<&PlaybackItem> {
        let idx = position as usize;
        if idx < self.items.len() {
            self.current_index = Some(idx);
            self.items.get(idx)
        } else {
            None
        }
    }

    /// Select track by `QueueId`.
    pub fn select_id(&mut self, id: QueueId) -> Option<&PlaybackItem> {
        let idx = self.items.iter().position(|item| item.id == id)?;
        self.current_index = Some(idx);
        self.items.get(idx)
    }

    /// Current selected track, or select the first track if none is selected and queue is non-empty.
    pub fn current_or_first(&mut self) -> Option<&PlaybackItem> {
        if self.items.is_empty() {
            self.current_index = None;
            return None;
        }
        let idx = self.current_index.unwrap_or(0).min(self.items.len() - 1);
        self.current_index = Some(idx);
        self.items.get(idx)
    }

    pub fn current(&self) -> Option<&PlaybackItem> {
        self.current_index.and_then(|idx| self.items.get(idx))
    }

    pub fn current_index(&self) -> Option<usize> {
        self.current_index
    }

    /// Peek at the next track without mutating the current cursor (used for gapless pre-buffering).
    pub fn peek_next(&self, repeat: bool, single: bool) -> Option<&PlaybackItem> {
        if self.items.is_empty() {
            return None;
        }
        let cur = self.current_index?;
        if single {
            return if repeat { self.items.get(cur) } else { None };
        }
        if cur + 1 < self.items.len() {
            self.items.get(cur + 1)
        } else if repeat {
            self.items.first()
        } else {
            None
        }
    }

    /// Advance to the next track in the queue according to `repeat`, `single`, and `consume` options.
    pub fn advance_next(
        &mut self,
        repeat: bool,
        single: bool,
        consume: bool,
    ) -> Option<PlaybackItem> {
        if self.items.is_empty() {
            self.current_index = None;
            return None;
        }
        let cur = self.current_index?;

        if single && repeat {
            return self.items.get(cur).cloned();
        }

        if consume {
            self.items.remove(cur);
            self.reindex();
            self.version = self.version.wrapping_add(1);
            if self.items.is_empty() || single {
                self.current_index = None;
                return None;
            }
            let next_idx = if cur < self.items.len() {
                cur
            } else if repeat {
                0
            } else {
                self.current_index = None;
                return None;
            };
            self.current_index = Some(next_idx);
            return self.items.get(next_idx).cloned();
        }

        if single {
            self.current_index = None;
            return None;
        }

        if cur + 1 < self.items.len() {
            self.current_index = Some(cur + 1);
            self.items.get(cur + 1).cloned()
        } else if repeat {
            self.current_index = Some(0);
            self.items.first().cloned()
        } else {
            None
        }
    }

    /// Step back to the previous track in the queue.
    pub fn step_previous(&mut self, repeat: bool) -> Option<PlaybackItem> {
        if self.items.is_empty() {
            self.current_index = None;
            return None;
        }
        let cur = self.current_index.unwrap_or(0);
        if cur > 0 {
            self.current_index = Some(cur - 1);
            self.items.get(cur - 1).cloned()
        } else if repeat {
            let last = self.items.len() - 1;
            self.current_index = Some(last);
            self.items.get(last).cloned()
        } else {
            self.current_index = Some(0);
            self.items.first().cloned()
        }
    }

    /// Update duration/metadata of an item in the queue once probed by the decoder.
    pub fn update_item_metadata(
        &mut self,
        id: QueueId,
        duration: Option<Duration>,
        title: Option<String>,
        artist: Option<String>,
        album: Option<String>,
    ) {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            if duration.is_some() {
                item.duration = duration;
            }
            if item.title.is_none() && title.is_some() {
                item.title = title;
            }
            if item.artist.is_none() && artist.is_some() {
                item.artist = artist;
            }
            if item.album.is_none() && album.is_some() {
                item.album = album;
            }
        }
    }

    pub fn items(&self) -> &[PlaybackItem] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    /// Enqueue multiple items in a single batch, returning their allocated `QueueId`s.
    pub fn enqueue_batch(
        &mut self,
        items: Vec<(AudioSource, Option<String>, Option<String>)>,
    ) -> Vec<QueueId> {
        let mut ids = Vec::with_capacity(items.len());
        if items.is_empty() {
            return ids;
        }
        for (source, title, artist) in items {
            let id = self.allocate_id();
            let position = self.items.len() as u32;
            self.items.push(PlaybackItem {
                id,
                position,
                source,
                title,
                artist,
                album: None,
                duration: None,
            });
            ids.push(id);
        }
        self.version = self.version.wrapping_add(1);
        ids
    }

    /// Move track from position `from` to position `to`.
    pub fn move_pos(&mut self, from: u32, to: u32) -> bool {
        let len = self.items.len();
        let from_idx = from as usize;
        let to_idx = to as usize;
        if from_idx >= len || to_idx >= len {
            return false;
        }
        if from_idx == to_idx {
            return true;
        }

        let cur_id = self.current().map(|i| i.id);
        let item = self.items.remove(from_idx);
        self.items.insert(to_idx, item);
        self.reindex();
        self.version = self.version.wrapping_add(1);

        if let Some(cid) = cur_id {
            self.current_index = self.items.iter().position(|i| i.id == cid);
        }
        true
    }

    /// Move track by `QueueId` to position `to`.
    pub fn move_id(&mut self, id: QueueId, to: u32) -> bool {
        let Some(pos) = self.items.iter().position(|i| i.id == id) else {
            return false;
        };
        self.move_pos(pos as u32, to)
    }

    /// Swap two tracks by 0-based position.
    pub fn swap_pos(&mut self, pos1: u32, pos2: u32) -> bool {
        let len = self.items.len();
        let idx1 = pos1 as usize;
        let idx2 = pos2 as usize;
        if idx1 >= len || idx2 >= len {
            return false;
        }
        if idx1 == idx2 {
            return true;
        }

        let cur_id = self.current().map(|i| i.id);
        self.items.swap(idx1, idx2);
        self.reindex();
        self.version = self.version.wrapping_add(1);

        if let Some(cid) = cur_id {
            self.current_index = self.items.iter().position(|i| i.id == cid);
        }
        true
    }

    /// Swap two tracks by `QueueId`.
    pub fn swap_id(&mut self, id1: QueueId, id2: QueueId) -> bool {
        let Some(p1) = self.items.iter().position(|i| i.id == id1) else {
            return false;
        };
        let Some(p2) = self.items.iter().position(|i| i.id == id2) else {
            return false;
        };
        self.swap_pos(p1 as u32, p2 as u32)
    }

    /// Shuffle the queue items using Fisher-Yates, preserving the active track's pointer.
    pub fn shuffle(&mut self) {
        if self.items.len() <= 1 {
            return;
        }

        let cur_id = self.current().map(|i| i.id);
        let mut seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(123_456_789)
            ^ 0x9e37_79b9_7f4a_7c15;

        let mut next_rand = move || {
            seed ^= seed >> 12;
            seed ^= seed << 25;
            seed ^= seed >> 27;
            seed.wrapping_mul(0x2545_f491_4f6c_dd1d)
        };

        for i in (1..self.items.len()).rev() {
            let j = (next_rand() % ((i + 1) as u64)) as usize;
            self.items.swap(i, j);
        }

        self.reindex();
        self.version = self.version.wrapping_add(1);

        if let Some(cid) = cur_id {
            self.current_index = self.items.iter().position(|i| i.id == cid);
        }
    }
}
