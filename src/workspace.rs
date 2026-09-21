//! Local workspace state.
//!
//! The broker owns balances, holdings, orders and its own watchlist. This owns
//! everything the broker has no concept of: custom lists, their ordering, and
//! later tags and notes. Kept separate so a broker refresh can never discard
//! local work, and local edits can never be mistaken for account state.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Which list the strip is showing.
///
/// Two of these are derived rather than stored: the broker list comes from the
/// account, and Positions is whatever is currently held. Only `Local` is ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ListId {
    Broker,
    Positions,
    Local(usize),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WatchList {
    pub name: String,
    /// Ordered, deduplicated. Order is meaningful: it is the user's ranking.
    pub isins: Vec<String>,
}

impl WatchList {
    pub fn new(name: impl Into<String>) -> Self {
        WatchList {
            name: name.into(),
            isins: Vec::new(),
        }
    }

    /// Returns false when the instrument was already present.
    pub fn add(&mut self, isin: &str) -> bool {
        let isin = isin.trim().to_uppercase();
        if isin.is_empty() || self.isins.iter().any(|i| i == &isin) {
            return false;
        }
        self.isins.push(isin);
        true
    }

    pub fn remove(&mut self, isin: &str) {
        self.isins.retain(|i| i != isin);
    }

    /// Move an entry by `delta` places, clamped. Ranking is the point of a
    /// custom list, so reordering has to survive as data.
    pub fn move_by(&mut self, isin: &str, delta: isize) {
        let Some(from) = self.isins.iter().position(|i| i == isin) else {
            return;
        };
        let to = (from as isize + delta).clamp(0, self.isins.len() as isize - 1) as usize;
        if to != from {
            let item = self.isins.remove(from);
            self.isins.insert(to, item);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub lists: Vec<WatchList>,
    pub active: ListId,
}

impl Default for Workspace {
    fn default() -> Self {
        // Start on the broker list, which is what existed before local lists.
        Workspace {
            lists: Vec::new(),
            active: ListId::Broker,
        }
    }
}

impl Workspace {
    pub fn path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".config/scalable-terminal/workspace.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|t| serde_json::from_str::<Workspace>(&t).ok())
            .map(|mut w| {
                w.repair();
                w
            })
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(t) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, t);
        }
    }

    /// A stored file can point at a list that no longer exists, which would
    /// otherwise show an empty strip with no way back.
    pub fn repair(&mut self) {
        if let ListId::Local(i) = self.active
            && i >= self.lists.len()
        {
            self.active = ListId::Broker;
        }
    }

    pub fn create(&mut self, name: impl Into<String>) -> ListId {
        self.lists.push(WatchList::new(name));
        ListId::Local(self.lists.len() - 1)
    }

    /// Indices shift when a list is removed, so the active selection has to be
    /// rewritten rather than left pointing at whatever slid into the slot.
    pub fn delete(&mut self, index: usize) {
        if index >= self.lists.len() {
            return;
        }
        self.lists.remove(index);
        self.active = match self.active {
            ListId::Local(a) if a == index => ListId::Broker,
            ListId::Local(a) if a > index => ListId::Local(a - 1),
            other => other,
        };
        self.repair();
    }

    pub fn local(&self, id: ListId) -> Option<&WatchList> {
        match id {
            ListId::Local(i) => self.lists.get(i),
            _ => None,
        }
    }

    pub fn local_mut(&mut self, id: ListId) -> Option<&mut WatchList> {
        match id {
            ListId::Local(i) => self.lists.get_mut(i),
            _ => None,
        }
    }

    pub fn name_of(&self, id: ListId) -> String {
        match id {
            ListId::Broker => "Scalable account".into(),
            ListId::Positions => "Positions".into(),
            ListId::Local(i) => self
                .lists
                .get(i)
                .map(|l| l.name.clone())
                .unwrap_or_else(|| "(missing)".into()),
        }
    }

    /// The rows the strip should show, given live broker state.
    pub fn rows(&self, broker: &[String], held: &[String]) -> Vec<String> {
        match self.active {
            ListId::Broker => {
                // Positions cannot live on the broker list, so show them here
                // too rather than leaving holdings invisible.
                let mut v = broker.to_vec();
                for h in held {
                    if !v.iter().any(|i| i == h) {
                        v.push(h.clone());
                    }
                }
                v
            }
            ListId::Positions => held.to_vec(),
            ListId::Local(i) => self
                .lists
                .get(i)
                .map(|l| l.isins.clone())
                .unwrap_or_default(),
        }
    }

    /// Everything worth polling: what is on screen, plus holdings, which need a
    /// live mid to mark profit whichever list is showing.
    pub fn poll_set(&self, broker: &[String], held: &[String]) -> Vec<String> {
        let mut v = self.rows(broker, held);
        for h in held {
            if !v.iter().any(|i| i == h) {
                v.push(h.clone());
            }
        }
        v
    }
}
