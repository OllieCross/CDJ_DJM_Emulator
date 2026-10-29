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

    /// As players label it.
    pub fn label(self) -> &'static str {
        match self {
            Slot::Sd => "SD",
            Slot::Usb => "USB",
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

/// What a player tells a client that asks about a medium (media query).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediumSummary {
    pub name: String,
    /// `YYYY-MM-DD` the export database was written, as players show it.
    pub created: String,
    pub tracks: usize,
    pub playlists: usize,
    /// A "My Settings" file was exported with the collection.
    pub my_settings: bool,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

impl Medium {
    pub fn summary(&self) -> MediumSummary {
        let created = std::fs::metadata(Library::pdb_path(&self.root))
            .and_then(|m| m.created().or_else(|_| m.modified()))
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or_else(String::new, |d| civil_date(d.as_secs()));
        let (total_bytes, free_bytes) = volume_space(&self.root).unwrap_or((0, 0));
        MediumSummary {
            name: self.name.clone(),
            created,
            tracks: self.library.tracks.len(),
            playlists: self.library.playlist_count(),
            my_settings: self.root.join("PIONEER").join("MYSETTING.DAT").exists(),
            total_bytes,
            free_bytes,
        }
    }
}

/// `YYYY-MM-DD` (UTC) of a Unix time.
fn civil_date(secs: u64) -> String {
    // Howard Hinnant's days-to-civil algorithm.
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Size and free space of the volume holding `path`.
fn volume_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let frag = st.f_frsize as u64;
    Some((st.f_blocks as u64 * frag, st.f_bavail as u64 * frag))
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
    fn summary_describes_the_export() {
        let m = Medium::open(&demo_export()).unwrap();
        let s = m.summary();
        assert_eq!(s.name, "demo_tracks");
        assert_eq!(s.tracks, m.library.tracks.len());
        assert!(s.tracks > 0);
        assert_eq!(s.playlists, m.library.playlist_count());
        assert!(s.my_settings);
        assert_eq!(s.created.len(), 10, "YYYY-MM-DD: {}", s.created);
        assert!(s.total_bytes >= s.free_bytes && s.total_bytes > 0);
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(951_782_400), "2000-02-29");
        assert_eq!(civil_date(1_714_521_600), "2024-05-01");
    }

    #[test]
    fn slot_bytes_match_cdj_status() {
        assert_eq!(Slot::from_protocol(0x03), Some(Slot::Usb));
        assert_eq!(Slot::from_protocol(0x02), Some(Slot::Sd));
        assert_eq!(Slot::from_protocol(0x01), None);
        assert_eq!(Slot::Usb.source_slot().to_byte(), 0x03);
    }
}
