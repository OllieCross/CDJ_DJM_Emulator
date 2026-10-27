//! Rekordbox USB/SD export reader: tracks, playlists, artwork and the
//! per-track analysis files (beat grid, cues, waveforms).

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context};
use binrw::BinRead;
use rekordcrate::anlz::{Content, CueType, ANLZ};
use rekordcrate::pdb::{Header, PageType, Row};
use rekordcrate::util::ColorIndex;
use tracing::{info, warn};

#[derive(Debug, Clone, Default)]
pub struct TrackInfo {
    pub id: u32,
    pub title: String,
    pub artist_id: u32,
    pub artist: String,
    pub album_id: u32,
    pub album: String,
    pub genre_id: u32,
    pub genre: String,
    pub comment: String,
    /// Musical key name from PDB Key table (e.g. "8m", "Am"). Empty if not set.
    pub key: String,
    /// BPM x 100 (12000 = 120.00 BPM).
    pub bpm_hundredths: u16,
    pub duration_s: u32,
    /// User rating 0..=5 from PDB.
    pub rating: u8,
    /// Release year from PDB, or 0 if not set.
    pub year: u16,
    /// Date track was added to rekordbox collection (e.g. "2024-01-15"). Empty if not set.
    pub date_added: String,
    /// Rekordbox colour label, 0 (none) ..= 8 (purple).
    pub color: u8,
    pub bitrate: u32,
    /// Non-zero when the track has artwork in the PDB.
    pub artwork_id: u32,
    /// Absolute path to the ANLZ `.DAT` analysis file. `.EXT` and `.2EX`
    /// sit next to it with the same name.
    pub anlz_dat: PathBuf,
    /// Absolute path to the audio file.
    pub audio_path: PathBuf,
}

/// Which analysis file a section lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnlzFile {
    Dat,
    Ext,
    TwoEx,
}

impl AnlzFile {
    /// From the extension clients name in analysis-tag requests.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_uppercase().as_str() {
            "DAT" => Some(Self::Dat),
            "EXT" => Some(Self::Ext),
            "2EX" => Some(Self::TwoEx),
            _ => None,
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Dat => "DAT",
            Self::Ext => "EXT",
            Self::TwoEx => "2EX",
        }
    }
}

impl TrackInfo {
    pub fn anlz_path(&self, file: AnlzFile) -> PathBuf {
        self.anlz_dat.with_extension(file.extension())
    }
}

/// A playlist or folder in the export's playlist tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistNode {
    pub id: u32,
    /// 0 for nodes at the top level.
    pub parent_id: u32,
    pub name: String,
    pub is_folder: bool,
    pub sort_order: u32,
}

/// A memory point, hot cue or loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    /// 0 for memory points, 1 = hot cue A, 2 = B, ...
    pub hot_cue: u8,
    pub time_ms: u32,
    /// Set for loops.
    pub loop_end_ms: Option<u32>,
    /// Memory-point colour, rekordbox colour index (0 = none).
    pub color_id: u8,
    /// Hot-cue colour: rekordbox hot-cue palette index and RGB.
    pub hot_cue_color: Option<(u8, [u8; 3])>,
    pub comment: String,
}

#[derive(Debug)]
pub struct Library {
    pub root: PathBuf,
    /// Sorted by ID.
    pub tracks: Vec<TrackInfo>,
    index: HashMap<u32, usize>,
    pub artists: BTreeMap<u32, String>,
    pub albums: BTreeMap<u32, String>,
    pub genres: BTreeMap<u32, String>,
    playlists: Vec<PlaylistNode>,
    /// Playlist ID -> track IDs in playlist order.
    playlist_entries: HashMap<u32, Vec<u32>>,
    /// artwork_id -> absolute path to JPEG file
    artwork: HashMap<u32, PathBuf>,
}

fn color_id(c: &ColorIndex) -> u8 {
    match c {
        ColorIndex::None => 0,
        ColorIndex::Pink => 1,
        ColorIndex::Red => 2,
        ColorIndex::Orange => 3,
        ColorIndex::Yellow => 4,
        ColorIndex::Green => 5,
        ColorIndex::Aqua => 6,
        ColorIndex::Blue => 7,
        ColorIndex::Purple => 8,
    }
}

/// Resolve a path stored in the export (`/Contents/...`) against its root.
/// Rejects paths that would climb out of the export, since the files are
/// served to the network.
fn export_path(root: &Path, stored: &str) -> PathBuf {
    let rel = stored.trim_start_matches(['/', '\\']).replace('\\', "/");
    if rel.split('/').any(|part| part == "..") {
        warn!(path = stored, "library: ignoring path outside the export");
        return PathBuf::new();
    }
    root.join(rel)
}

impl Library {
    /// Whether `root` holds a rekordbox export.
    pub fn is_export(root: &Path) -> bool {
        Self::pdb_path(root).exists()
    }

    fn pdb_path(root: &Path) -> PathBuf {
        let pioneer = root.join("PIONEER");
        let legacy = pioneer.join("rekordbox.pdb");
        if legacy.exists() {
            legacy
        } else {
            pioneer.join("rekordbox").join("export.pdb")
        }
    }

    pub fn open(root: &Path) -> anyhow::Result<Arc<Self>> {
        let pdb_path = Self::pdb_path(root);
        let f = File::open(&pdb_path)
            .with_context(|| format!("cannot open {}", pdb_path.display()))?;
        let mut r = BufReader::new(f);
        let header = Header::read_le(&mut r).context("failed to parse export.pdb")?;

        let mut artists = BTreeMap::new();
        let mut albums = BTreeMap::new();
        let mut genres = BTreeMap::new();
        let mut keys: HashMap<u32, String> = HashMap::new();
        let mut artwork = HashMap::new();
        let mut playlists = Vec::new();
        let mut entries: HashMap<u32, Vec<(u32, u32)>> = HashMap::new();
        let mut raw_tracks = Vec::new();

        for table in &header.tables {
            let pages = match header.read_pages(
                &mut r,
                binrw::Endian::Little,
                (&table.first_page, &table.last_page),
            ) {
                Ok(p) => p,
                Err(e) => {
                    warn!("library: skipping table (type {:?}): {e}", table.page_type);
                    continue;
                }
            };
            for page in pages {
                for rg in &page.row_groups {
                    for row in rg.present_rows() {
                        match row {
                            Row::Artist(a) => {
                                artists.insert(a.id.0, a.name.into_string().unwrap_or_default());
                            }
                            Row::Album(a) => {
                                albums.insert(a.id.0, a.name.into_string().unwrap_or_default());
                            }
                            Row::Genre(g) => {
                                genres.insert(g.id.0, g.name.into_string().unwrap_or_default());
                            }
                            Row::Key(k) => {
                                keys.insert(k.id.0, k.name.into_string().unwrap_or_default());
                            }
                            Row::Artwork(a) => {
                                let stored = a.path.into_string().unwrap_or_default();
                                artwork.insert(a.id.0, export_path(root, &stored));
                            }
                            Row::PlaylistTreeNode(n) => playlists.push(PlaylistNode {
                                id: n.id.0,
                                parent_id: n.parent_id.0,
                                is_folder: n.is_folder(),
                                sort_order: n.sort_order,
                                name: n.name.into_string().unwrap_or_default(),
                            }),
                            Row::PlaylistEntry(e) => entries
                                .entry(e.playlist_id.0)
                                .or_default()
                                .push((e.entry_index, e.track_id.0)),
                            Row::Track(t) if table.page_type == PageType::Tracks => raw_tracks.push(t),
                            _ => {}
                        }
                    }
                }
            }
        }

        let mut tracks: Vec<TrackInfo> = raw_tracks
            .into_iter()
            .map(|t| TrackInfo {
                id: t.id.0,
                title: t.title.into_string().unwrap_or_default(),
                artist_id: t.artist_id.0,
                artist: artists.get(&t.artist_id.0).cloned().unwrap_or_default(),
                album_id: t.album_id.0,
                album: albums.get(&t.album_id.0).cloned().unwrap_or_default(),
                genre_id: t.genre_id.0,
                genre: genres.get(&t.genre_id.0).cloned().unwrap_or_default(),
                comment: t.comment.into_string().unwrap_or_default(),
                key: keys.get(&t.key_id.0).cloned().unwrap_or_default(),
                bpm_hundredths: (t.tempo as u16).max(1),
                duration_s: u32::from(t.duration),
                rating: t.rating,
                year: t.year,
                date_added: t.date_added.into_string().unwrap_or_default(),
                color: color_id(&t.color),
                bitrate: t.bitrate,
                artwork_id: t.artwork_id.0,
                anlz_dat: export_path(root, &t.analyze_path.into_string().unwrap_or_default()),
                audio_path: export_path(root, &t.file_path.into_string().unwrap_or_default()),
            })
            .collect();
        tracks.sort_by_key(|t| t.id);
        playlists.sort_by_key(|p| (p.parent_id, p.sort_order, p.id));
        let playlist_entries = entries
            .into_iter()
            .map(|(id, mut list)| {
                list.sort_unstable();
                (id, list.into_iter().map(|(_, track)| track).collect())
            })
            .collect();

        info!(
            "library: loaded {} tracks, {} playlists, {} artwork entries from {}",
            tracks.len(),
            playlists.len(),
            artwork.len(),
            pdb_path.display()
        );
        Ok(Arc::new(Self::build(root.to_path_buf(), tracks, artists, albums, genres, playlists, playlist_entries, artwork)))
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        root: PathBuf,
        tracks: Vec<TrackInfo>,
        artists: BTreeMap<u32, String>,
        albums: BTreeMap<u32, String>,
        genres: BTreeMap<u32, String>,
        playlists: Vec<PlaylistNode>,
        playlist_entries: HashMap<u32, Vec<u32>>,
        artwork: HashMap<u32, PathBuf>,
    ) -> Self {
        let index = tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        Self {
            root,
            tracks,
            index,
            artists,
            albums,
            genres,
            playlists,
            playlist_entries,
            artwork,
        }
    }

    /// A library with the given tracks and nothing else.
    #[cfg(test)]
    pub(crate) fn from_tracks(tracks: Vec<TrackInfo>) -> Self {
        Self::build(
            PathBuf::new(),
            tracks,
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            Vec::new(),
            HashMap::new(),
            HashMap::new(),
        )
    }

    pub fn track_by_id(&self, id: u32) -> Option<&TrackInfo> {
        self.index.get(&id).map(|&i| &self.tracks[i])
    }

    /// 1-based position of the track in the collection.
    pub fn track_number(&self, id: u32) -> Option<u16> {
        self.index.get(&id).map(|&i| (i + 1) as u16)
    }

    /// Folders and playlists directly inside folder `folder_id` (0 = top).
    pub fn playlist_children(&self, folder_id: u32) -> Vec<&PlaylistNode> {
        self.playlists.iter().filter(|p| p.parent_id == folder_id).collect()
    }

    pub fn playlist(&self, id: u32) -> Option<&PlaylistNode> {
        self.playlists.iter().find(|p| p.id == id)
    }

    /// Track IDs of playlist `id`, in playlist order.
    pub fn playlist_tracks(&self, id: u32) -> &[u32] {
        self.playlist_entries.get(&id).map_or(&[], Vec::as_slice)
    }

    /// Beat grid bytes in the layout beat-link's `BeatGrid` reads from the
    /// dbserver: a 20-byte header, then per beat a 16-byte little-endian
    /// record (beat within bar u16, BPM x 100 u16, time in ms u32, 8 bytes 0).
    pub fn beat_grid_for(&self, track: &TrackInfo) -> anyhow::Result<Vec<u8>> {
        let anlz = open_anlz(&track.anlz_dat)?;
        for section in &anlz.sections {
            if let Content::BeatGrid(grid) = &section.content {
                return Ok(encode_beat_grid(&grid.beats));
            }
        }
        bail!("no beat grid in {}", track.anlz_dat.display())
    }

    /// Beat times (ms), beat-within-bar values and tempos (BPM x 100) from
    /// the analysis.
    pub fn beats_for(&self, track: &TrackInfo) -> anyhow::Result<Vec<(u32, u8, u16)>> {
        let anlz = open_anlz(&track.anlz_dat)?;
        for section in &anlz.sections {
            if let Content::BeatGrid(grid) = &section.content {
                return Ok(grid.beats.iter().map(|b| (b.time, b.beat_number as u8, b.tempo)).collect());
            }
        }
        bail!("no beat grid in {}", track.anlz_dat.display())
    }

    /// Blue waveform preview as the dbserver sends it: per column one byte
    /// of height (0-31) and one of whiteness (0-7), from the `.DAT` PWAV tag.
    pub fn waveform_preview_for(&self, track: &TrackInfo) -> anyhow::Result<Vec<u8>> {
        let data = tag_data(&track.anlz_dat, *b"PWAV")?;
        Ok(data.iter().flat_map(|&b| [b & 0x1f, b >> 5]).collect())
    }

    /// Blue scrolling waveform, one byte per half-frame (height in the low 5
    /// bits, whiteness in the top 3), from the `.EXT` PWV3 tag.
    pub fn waveform_detail_for(&self, track: &TrackInfo) -> anyhow::Result<Vec<u8>> {
        tag_data(&track.anlz_path(AnlzFile::Ext), *b"PWV3")
    }

    /// Overview waveform for display: one `[height, r, g, b]` per column
    /// (height 0-255). rekordbox's colour preview (`.EXT` PWV4) if present,
    /// else the blue one (`.DAT` PWAV).
    pub fn overview_display(&self, track: &TrackInfo) -> anyhow::Result<Vec<[u8; 4]>> {
        if let Ok(data) = tag_data(&track.anlz_path(AnlzFile::Ext), *b"PWV4") {
            // Six bytes per column; bytes 3-5 are red, green, blue energy, the
            // largest of them the column's height (as beat-link draws it).
            let cols: Vec<[u8; 3]> = data.chunks_exact(6).map(|c| [c[3], c[4], c[5]]).collect();
            let peak = cols.iter().flatten().copied().max().unwrap_or(1).max(1);
            return Ok(cols
                .iter()
                .map(|&[r, g, b]| {
                    let top = r.max(g).max(b).max(1);
                    let scale = |v: u8| (u16::from(v) * 255 / u16::from(top)) as u8;
                    [(u16::from(top) * 255 / u16::from(peak)) as u8, scale(r), scale(g), scale(b)]
                })
                .collect());
        }
        Ok(tag_data(&track.anlz_dat, *b"PWAV")?.into_iter().map(blue_column).collect())
    }

    /// Scrolling waveform for display, 150 columns per second, one
    /// `[height, r, g, b]` each: rekordbox's colour detail (`.EXT` PWV5) if
    /// present, else the blue one (`.EXT` PWV3).
    pub fn detail_display(&self, track: &TrackInfo) -> anyhow::Result<Vec<[u8; 4]>> {
        let ext = track.anlz_path(AnlzFile::Ext);
        if let Ok(data) = tag_data(&ext, *b"PWV5") {
            // Two bytes per column, big-endian: red, green, blue (3 bits
            // each) and height (5 bits).
            return Ok(data
                .chunks_exact(2)
                .map(|c| {
                    let v = u16::from_be_bytes([c[0], c[1]]);
                    let channel = |shift: u16| (((v >> shift) & 7) * 36) as u8;
                    [(((v >> 2) & 31) * 8) as u8, channel(13), channel(10), channel(7)]
                })
                .collect());
        }
        Ok(tag_data(&ext, *b"PWV3")?.into_iter().map(blue_column).collect())
    }

    /// A whole analysis tag (header included), as clients request with
    /// analysis-tag requests (colour and 3-band waveforms, song structure...).
    pub fn anlz_tag(&self, track: &TrackInfo, file: AnlzFile, fourcc: [u8; 4]) -> anyhow::Result<Option<Vec<u8>>> {
        raw_anlz_section(&track.anlz_path(file), fourcc)
    }

    /// Memory points, hot cues and loops: from the `.EXT` file when it has
    /// them (colours and comments), otherwise from the `.DAT` file.
    pub fn cues_for(&self, track: &TrackInfo) -> anyhow::Result<Vec<Cue>> {
        let ext = track.anlz_path(AnlzFile::Ext);
        if ext.exists() {
            let anlz = open_anlz(&ext)?;
            let mut cues = Vec::new();
            let mut found = false;
            for section in &anlz.sections {
                if let Content::ExtendedCueList(list) = &section.content {
                    found = true;
                    cues.extend(list.cues.iter().map(|c| Cue {
                        hot_cue: c.hot_cue as u8,
                        time_ms: c.time,
                        loop_end_ms: matches!(c.cue_type, CueType::Loop).then_some(c.loop_time),
                        color_id: color_id(&c.color),
                        hot_cue_color: (c.hot_cue > 0).then(|| {
                            let (r, g, b) = c.hot_cue_color_rgb;
                            (c.hot_cue_color_index, [r, g, b])
                        }),
                        comment: c.comment.to_string(),
                    }));
                }
            }
            if found {
                return Ok(sorted(cues));
            }
        }
        let anlz = open_anlz(&track.anlz_dat)?;
        let mut cues = Vec::new();
        for section in &anlz.sections {
            if let Content::CueList(list) = &section.content {
                cues.extend(list.cues.iter().map(|c| Cue {
                    hot_cue: c.hot_cue as u8,
                    time_ms: c.time,
                    loop_end_ms: matches!(c.cue_type, CueType::Loop).then_some(c.loop_time),
                    color_id: 0,
                    hot_cue_color: None,
                    comment: String::new(),
                }));
            }
        }
        Ok(sorted(cues))
    }

    /// JPEG bytes for the given artwork ID, or `None` if not found / unreadable.
    pub fn artwork_jpeg(&self, artwork_id: u32) -> Option<Vec<u8>> {
        let path = self.artwork.get(&artwork_id)?;
        match std::fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                warn!(artwork_id, path = %path.display(), "artwork_jpeg: read failed: {e}");
                None
            }
        }
    }
}

/// A blue-waveform byte (height in the low 5 bits, whiteness in the top 3)
/// as `[height, r, g, b]`: whiter is louder.
fn blue_column(b: u8) -> [u8; 4] {
    let white = (b >> 5) * 24;
    [(b & 31) * 8, 40 + white, 110 + white, 255]
}

fn sorted(mut cues: Vec<Cue>) -> Vec<Cue> {
    cues.sort_by_key(|c| (c.hot_cue, c.time_ms));
    cues
}

// --- ANLZ helpers ---

fn open_anlz(path: &Path) -> anyhow::Result<ANLZ> {
    let f = File::open(path).with_context(|| format!("open ANLZ {}", path.display()))?;
    let mut r = BufReader::new(f);
    ANLZ::read_be(&mut r).with_context(|| format!("parse ANLZ {}", path.display()))
}

fn be_u32(b: &[u8]) -> usize {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize
}

/// The first section tagged `fourcc` in an ANLZ file, header included, or
/// `None` if the file has no such section.
///
/// ANLZ files are a `PMAI` header followed by sections of `fourcc`,
/// header length and total length (big-endian u32s), then the body.
pub fn raw_anlz_section(path: &Path, fourcc: [u8; 4]) -> anyhow::Result<Option<Vec<u8>>> {
    let data = std::fs::read(path).with_context(|| format!("open ANLZ {}", path.display()))?;
    if data.len() < 12 || &data[..4] != b"PMAI" {
        bail!("{} is not an ANLZ file", path.display());
    }
    let mut pos = be_u32(&data[4..8]);
    while pos + 12 <= data.len() {
        let len = be_u32(&data[pos + 8..pos + 12]);
        if len < 12 || pos + len > data.len() {
            break;
        }
        if data[pos..pos + 4] == fourcc {
            return Ok(Some(data[pos..pos + len].to_vec()));
        }
        pos += len;
    }
    Ok(None)
}

/// The body of a waveform section (after its header).
fn tag_data(path: &Path, fourcc: [u8; 4]) -> anyhow::Result<Vec<u8>> {
    let section = raw_anlz_section(path, fourcc)?.with_context(|| {
        format!("no {} tag in {}", String::from_utf8_lossy(&fourcc), path.display())
    })?;
    let header_len = be_u32(&section[4..8]).min(section.len());
    Ok(section[header_len..].to_vec())
}

fn encode_beat_grid(beats: &[rekordcrate::anlz::Beat]) -> Vec<u8> {
    const HEADER_LEN: usize = 20;
    const RECORD_LEN: usize = 16;
    let mut buf = vec![0u8; HEADER_LEN + beats.len() * RECORD_LEN];
    for (i, beat) in beats.iter().enumerate() {
        let base = HEADER_LEN + i * RECORD_LEN;
        buf[base..base + 2].copy_from_slice(&beat.beat_number.to_le_bytes());
        buf[base + 2..base + 4].copy_from_slice(&beat.tempo.to_le_bytes());
        buf[base + 4..base + 8].copy_from_slice(&beat.time.to_le_bytes());
    }
    buf
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A real rekordbox export (two demo tracks, analysis files, no audio)
    /// that ships with the vendored rekordcrate.
    pub(crate) fn demo_export() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor/rekordcrate/data/complete_export/demo_tracks")
    }

    pub(crate) fn demo_library() -> Arc<Library> {
        Library::open(&demo_export()).expect("demo export opens")
    }

    #[test]
    fn opens_demo_export() {
        let lib = demo_library();
        assert!(Library::is_export(&demo_export()));
        assert_eq!(lib.tracks.len(), 2);
        for t in &lib.tracks {
            assert!(!t.title.is_empty(), "track {} has a title", t.id);
            assert!(t.anlz_dat.exists(), "{} exists", t.anlz_dat.display());
            assert_eq!(lib.track_by_id(t.id).map(|x| x.id), Some(t.id));
        }
        assert!(lib.track_by_id(999_999).is_none());
    }

    #[test]
    fn reads_beat_grid_cues_and_waveforms() {
        let lib = demo_library();
        let t = &lib.tracks[0];
        let grid = lib.beat_grid_for(t).unwrap();
        assert!(grid.len() > 20 && (grid.len() - 20) % 16 == 0);
        let beats = lib.beats_for(t).unwrap();
        assert!(beats.iter().all(|&(_, bar, _)| (1..=4).contains(&bar)));
        assert!(beats.iter().all(|&(_, _, bpm)| bpm == t.bpm_hundredths), "stored tempo matches the track's");
        assert!(beats.windows(2).all(|w| w[0].0 < w[1].0), "beat times increase");

        let preview = lib.waveform_preview_for(t).unwrap();
        assert_eq!(preview.len(), 800, "400 columns x 2 bytes");
        assert!(preview.chunks(2).all(|c| c[0] < 32 && c[1] < 8));
        assert!(!lib.waveform_detail_for(t).unwrap().is_empty());

        // Cues load (the demo tracks may or may not have any).
        lib.cues_for(t).unwrap();
    }

    #[test]
    fn display_waveforms_from_the_analysis() {
        let lib = demo_library();
        let t = &lib.tracks[0];
        let overview = lib.overview_display(t).unwrap();
        assert!(overview.len() >= 400);
        assert!(overview.iter().any(|c| c[0] == 255), "scaled to the loudest column");
        let detail = lib.detail_display(t).unwrap();
        // 150 columns per second of track.
        let expected = f64::from(t.duration_s) * 150.0;
        assert!((detail.len() as f64 - expected).abs() < 300.0, "{} columns for {} s", detail.len(), t.duration_s);
    }

    #[test]
    fn serves_raw_analysis_tags() {
        let lib = demo_library();
        let t = &lib.tracks[0];
        let pwv4 = lib.anlz_tag(t, AnlzFile::Ext, *b"PWV4").unwrap().expect("colour preview");
        assert_eq!(&pwv4[..4], b"PWV4");
        assert_eq!(be_u32(&pwv4[8..12]), pwv4.len(), "section length field covers the tag");
        assert!(lib.anlz_tag(t, AnlzFile::Ext, *b"ZZZZ").unwrap().is_none());
        assert_eq!(AnlzFile::from_extension("2ex"), Some(AnlzFile::TwoEx));
    }

    #[test]
    fn export_paths_stay_inside_the_export() {
        let root = Path::new("/Volumes/USB");
        assert_eq!(export_path(root, "/Contents/a.mp3"), root.join("Contents/a.mp3"));
        assert_eq!(export_path(root, "/../../etc/passwd"), PathBuf::new());
    }
}
