//! Rekordbox exports the virtual players read from, and which player slot
//! each one is in.
//!
//! A [`Medium`] is an export on this Mac: a mounted USB stick or SD card, or
//! a folder. Inserting it into a player's USB or SD slot is what makes its
//! tracks loadable, on that player or (over the link) on any other running
//! player, as with real CDJs. A medium can only be in one slot at a time.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use cdj_proto::TrackSourceSlot;
use serde::{Deserialize, Serialize};

use crate::library::Library;

/// A player's media slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Slot {
    Sd,
    Usb,
}

impl Slot {
    pub fn source_slot(self) -> TrackSourceSlot {
        match self {
            Slot::Sd => TrackSourceSlot::Sd,
            Slot::Usb => TrackSourceSlot::Usb,
        }
    }

    /// From the slot byte clients put in dbserver requests.
    pub fn from_protocol(b: u8) -> Option<Self> {
        match TrackSourceSlot::from_byte(b) {
            TrackSourceSlot::Sd => Some(Slot::Sd),
            TrackSourceSlot::Usb => Some(Slot::Usb),
            _ => None,
        }
    }
}

/// A rekordbox export available on this Mac.
#[derive(Debug)]
pub struct Medium {
    pub root: PathBuf,
    /// Volume or folder name, shown as the media name.
    pub name: String,
    pub library: Arc<Library>,
}

impl Medium {
    pub fn open(root: &Path) -> anyhow::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            root: root.to_path_buf(),
            name: root
                .file_name()
                .map_or_else(|| root.display().to_string(), |n| n.to_string_lossy().into_owned()),
            library: Library::open(root)?,
        }))
    }
}

/// Folders directly under `dir` (normally `/Volumes`) that hold a rekordbox
/// export, sorted by path.
pub fn find_exports(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| Library::is_export(p))
        .collect();
    found.sort();
    found
}

/// Exports opened so far, so each is parsed once.
#[derive(Default)]
pub struct MediaCatalog {
    opened: Mutex<BTreeMap<PathBuf, Arc<Medium>>>,
}

impl MediaCatalog {
    pub fn open(&self, root: &Path) -> anyhow::Result<Arc<Medium>> {
        if let Some(m) = self.opened.lock().unwrap().get(root) {
            return Ok(m.clone());
        }
        let medium = Medium::open(root)?;
        self.opened
            .lock()
            .unwrap()
            .insert(root.to_path_buf(), medium.clone());
        Ok(medium)
    }

    /// Forget exports whose files are gone (unmounted volumes).
    pub fn prune(&self) {
        self.opened.lock().unwrap().retain(|root, _| Library::is_export(root));
    }
}

/// The media inserted in one player's slots.
#[derive(Debug, Default)]
pub struct PlayerMedia {
    slots: RwLock<BTreeMap<Slot, Arc<Medium>>>,
}

impl PlayerMedia {
    pub fn get(&self, slot: Slot) -> Option<Arc<Medium>> {
        self.slots.read().unwrap().get(&slot).cloned()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.read().unwrap().is_empty()
    }

    pub fn contents(&self) -> Vec<(Slot, Arc<Medium>)> {
        self.slots
            .read()
            .unwrap()
            .iter()
            .map(|(&s, m)| (s, m.clone()))
            .collect()
    }

    pub(crate) fn insert(&self, slot: Slot, medium: Arc<Medium>) {
        self.slots.write().unwrap().insert(slot, medium);
    }

    pub(crate) fn remove(&self, slot: Slot) -> Option<Arc<Medium>> {
        self.slots.write().unwrap().remove(&slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::tests::demo_export;

    #[test]
    fn finds_exports_in_a_volumes_folder() {
        let dir = demo_export().parent().unwrap().to_path_buf();
        let found = find_exports(&dir);
        assert!(found.contains(&demo_export()));
        assert!(find_exports(Path::new("/nonexistent")).is_empty());
    }

    #[test]
    fn catalog_opens_each_export_once() {
        let catalog = MediaCatalog::default();
        let a = catalog.open(&demo_export()).unwrap();
        let b = catalog.open(&demo_export()).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.name, "demo_tracks");
    }

    #[test]
    fn slot_bytes_match_cdj_status() {
        assert_eq!(Slot::from_protocol(0x03), Some(Slot::Usb));
        assert_eq!(Slot::from_protocol(0x02), Some(Slot::Sd));
        assert_eq!(Slot::from_protocol(0x01), None);
        assert_eq!(Slot::Usb.source_slot().to_byte(), 0x03);
    }
}
