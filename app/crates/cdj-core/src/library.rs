//! Rekordbox USB export reader.

use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use binrw::BinRead;
use rekordcrate::anlz::{Content, ANLZ};
use rekordcrate::pdb::{Header, PageType, Row};
use tracing::{debug, info, warn};

#[derive(Debug, Clone)]
pub struct TrackInfo {
    pub id: u32,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub comment: String,
    /// BPM x 100 (12000 = 120.00 BPM).
    pub bpm_hundredths: u16,
    pub duration_s: u32,
    /// Non-zero when the track has artwork in the PDB.
    pub artwork_id: u32,
    /// Absolute path to the ANLZ `.DAT` analysis file.
    pub anlz_dat: PathBuf,
    /// Absolute path to the ANLZ `.EXT` analysis file (color waveform, song structure).
    pub anlz_ext: PathBuf,
    /// Audio file path relative to the USB root (e.g. `/Contents/12345.mp3`).
    pub audio_path: PathBuf,
}

#[derive(Debug)]
pub struct Library {
    pub tracks: Vec<TrackInfo>,
    /// artwork_id -> absolute path to JPEG file
    artwork: HashMap<u32, PathBuf>,
}

impl Library {
    pub fn open(root: &Path) -> anyhow::Result<Arc<Self>> {
        let pioneer = root.join("PIONEER");
        let pdb_path = {
            let v5 = pioneer.join("rekordbox.pdb");
            let v6 = pioneer.join("rekordbox").join("export.pdb");
            if v5.exists() { v5 } else { v6 }
        };
        let f = File::open(&pdb_path)
            .with_context(|| format!("cannot open {}", pdb_path.display()))?;
        let mut r = BufReader::new(f);
        let header = Header::read_le(&mut r).context("failed to parse rekordbox.pdb")?;

        let mut artists: HashMap<u32, String> = HashMap::new();
        let mut albums: HashMap<u32, String> = HashMap::new();
        let mut genres: HashMap<u32, String> = HashMap::new();
        let mut artwork: HashMap<u32, PathBuf> = HashMap::new();

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
                            Row::Artist(a) => { artists.insert(a.id.0, a.name.into_string().unwrap_or_default()); }
                            Row::Album(a)  => { albums.insert(a.id.0, a.name.into_string().unwrap_or_default()); }
                            Row::Genre(g)  => { genres.insert(g.id.0, g.name.into_string().unwrap_or_default()); }
                            Row::Artwork(a) => {
                                let raw = a.path.into_string().unwrap_or_default();
                                let rel = raw.trim_start_matches('/').trim_start_matches('\\').replace('\\', "/");
                                artwork.insert(a.id.0, root.join(rel));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        let mut tracks: Vec<TrackInfo> = Vec::new();
        for table in &header.tables {
            if table.page_type != PageType::Tracks { continue; }
            let pages = match header.read_pages(&mut r, binrw::Endian::Little, (&table.first_page, &table.last_page)) {
                Ok(p) => p,
                Err(e) => { warn!("library: failed to read Tracks table: {e}"); continue; }
            };
            for page in pages {
                for rg in &page.row_groups {
                    for row in rg.present_rows() {
                        if let Row::Track(t) = row {
                            let to_path = |s: String| -> PathBuf {
                                let rel = s.trim_start_matches('/').trim_start_matches('\\').replace('\\', "/");
                                root.join(rel)
                            };

                            let anlz_dat = to_path(t.analyze_path.into_string().unwrap_or_default());
                            // Derive .EXT path from .DAT path (same name, different extension)
                            let anlz_ext = anlz_dat.with_extension("EXT");
                            let audio_path = to_path(t.file_path.into_string().unwrap_or_default());

                            let bpm_hundredths = if t.tempo > 0 { (t.tempo as u16).max(1) } else { 12000 };

                            tracks.push(TrackInfo {
                                id: t.id.0,
                                title: t.title.into_string().unwrap_or_default(),
                                artist: artists.get(&t.artist_id.0).cloned().unwrap_or_default(),
                                album: albums.get(&t.album_id.0).cloned().unwrap_or_default(),
                                genre: genres.get(&t.genre_id.0).cloned().unwrap_or_default(),
                                comment: t.comment.into_string().unwrap_or_default(),
                                bpm_hundredths,
                                duration_s: t.duration as u32,
                                artwork_id: t.artwork_id.0,
                                anlz_dat,
                                anlz_ext,
                                audio_path,
                            });
                        }
                    }
                }
            }
        }

        tracks.sort_by_key(|t| t.id);
        info!("library: loaded {} tracks, {} artwork entries from {}", tracks.len(), artwork.len(), pdb_path.display());
        Ok(Arc::new(Self { tracks, artwork }))
    }

    pub fn track_by_id(&self, id: u32) -> Option<&TrackInfo> {
        self.tracks.iter().find(|t| t.id == id)
    }

    /// Beat grid bytes in the PQTZ section format beat-link's BeatGrid.java expects.
    /// Layout: 24-byte synthetic header + 8 bytes/beat (all big-endian):
    ///   [0x00]: "PQTZ"  [0x0C]: unknown=0  [0x10]: unknown=0x00800000
    ///   [0x14]: beat_count  [0x18+i*8]: tempo(2) beat_in_bar(2) time_ms(4)
    pub fn beat_grid_for(&self, track: &TrackInfo) -> anyhow::Result<Vec<u8>> {
        debug!(path = %track.anlz_dat.display(), "beat_grid_for: opening DAT file");
        let anlz = open_anlz(&track.anlz_dat)?;
        let section_kinds: Vec<_> = anlz.sections.iter().map(|s| format!("{:?}", s.content)).collect();
        debug!(sections = ?section_kinds, "beat_grid_for: sections in DAT");
        for section in &anlz.sections {
            if let Content::BeatGrid(grid) = &section.content {
                debug!(beats = grid.beats.len(), "beat_grid_for: found BeatGrid");
                return Ok(encode_beat_grid(&grid.beats));
            }
        }
        anyhow::bail!("no BeatGrid section in {}", track.anlz_dat.display())
    }

    /// Waveform preview bytes. Returns RGB color (3 bytes/col) from the .EXT file
    /// (PWV4 section) if available, otherwise monochrome (2 bytes/col) from the .DAT file.
    /// Beat-link detects color when data.length % 3 == 0 and data.length % 2 != 0.
    pub fn waveform_preview_for(&self, track: &TrackInfo) -> anyhow::Result<Vec<u8>> {
        if track.anlz_ext.exists() {
            match open_anlz(&track.anlz_ext) {
                Ok(anlz) => {
                    for section in &anlz.sections {
                        if let Content::WaveformColorPreview(wfm) = &section.content {
                            debug!(
                                cols = wfm.data.len(),
                                "waveform_preview_for: using color preview (PWV4) from EXT"
                            );
                            return Ok(encode_color_waveform_preview(&wfm.data));
                        }
                    }
                    debug!("waveform_preview_for: no PWV4 in EXT, falling back to mono");
                }
                Err(e) => {
                    warn!(
                        path = %track.anlz_ext.display(),
                        "waveform_preview_for: failed to parse EXT file: {e}"
                    );
                }
            }
        }
        // Fall back to monochrome from DAT
        let anlz = open_anlz(&track.anlz_dat)?;
        for section in &anlz.sections {
            match &section.content {
                Content::WaveformPreview(wfm) => {
                    return Ok(encode_waveform_preview(&wfm.data));
                }
                Content::TinyWaveformPreview(wfm) => {
                    return Ok(encode_tiny_waveform_preview(&wfm.data));
                }
                _ => {}
            }
        }
        anyhow::bail!("no WaveformPreview section in {}", track.anlz_dat.display())
    }

    /// Full-resolution waveform detail bytes for the scrollable waveform in beat-link.
    /// Returns color (3 bytes/col) from .EXT (PWV5) if available, mono (2 bytes/col) from .DAT.
    pub fn waveform_detail_for(&self, track: &TrackInfo) -> anyhow::Result<Vec<u8>> {
        if track.anlz_ext.exists() {
            match open_anlz(&track.anlz_ext) {
                Ok(anlz) => {
                    for section in &anlz.sections {
                        if let Content::WaveformColorDetail(wfm) = &section.content {
                            debug!(
                                cols = wfm.data.len(),
                                "waveform_detail_for: using color detail (PWV5) from EXT"
                            );
                            return Ok(encode_color_waveform_detail(&wfm.data));
                        }
                    }
                    debug!("waveform_detail_for: no PWV5 in EXT, falling back to mono PWV3");
                }
                Err(e) => {
                    warn!(
                        path = %track.anlz_ext.display(),
                        "waveform_detail_for: failed to parse EXT file: {e}"
                    );
                }
            }
        }
        let anlz = open_anlz(&track.anlz_dat)?;
        for section in &anlz.sections {
            if let Content::WaveformDetail(wfm) = &section.content {
                return Ok(encode_waveform_detail(&wfm.data));
            }
        }
        anyhow::bail!("no WaveformDetail section in {}", track.anlz_dat.display())
    }

    /// JPEG bytes for the given artwork ID, or `None` if not found / unreadable.
    pub fn artwork_jpeg(&self, artwork_id: u32) -> Option<Vec<u8>> {
        let path = self.artwork.get(&artwork_id)?;
        std::fs::read(path).ok()
    }
}

// --- ANLZ helpers ---

fn open_anlz(path: &Path) -> anyhow::Result<ANLZ> {
    let f = File::open(path).with_context(|| format!("open ANLZ {}", path.display()))?;
    let mut r = BufReader::new(f);
    ANLZ::read_be(&mut r).context("parse ANLZ file")
}

/// Construct a PQTZ-shaped binary buffer that beat-link's BeatGrid.java can parse.
///
/// Section layout (all big-endian):
///   [0x00-0x03] "PQTZ"
///   [0x04-0x07] header_size = 24
///   [0x08-0x0B] total_size = 24 + n*8
///   [0x0C-0x0F] unknown1 = 0
///   [0x10-0x13] unknown2 = 0x00800000
///   [0x14-0x17] beat_count      <- BLT reads here (DEVICE_BEAT_GRID_HEADER = 0x14)
///   [0x18 + i*8] per beat:
///     [0-1] beat.beat_number    <- BLT tempos[i] (stored first in ANLZ)
///     [2-3] beat.tempo          <- BLT beatWithin[i] (stored second in ANLZ)
///     [4-7] beat.time           <- BLT times[i]
fn encode_beat_grid(beats: &[rekordcrate::anlz::Beat]) -> Vec<u8> {
    let n = beats.len();
    let total = 24 + n * 8;
    let mut buf = vec![0u8; total];
    buf[0..4].copy_from_slice(b"PQTZ");
    buf[4..8].copy_from_slice(&24u32.to_be_bytes());
    buf[8..12].copy_from_slice(&(total as u32).to_be_bytes());
    // [12-15] unknown1 = 0 (already zeroed)
    buf[16..20].copy_from_slice(&0x00800000u32.to_be_bytes());
    buf[20..24].copy_from_slice(&(n as u32).to_be_bytes());
    for (i, beat) in beats.iter().enumerate() {
        let base = 24 + i * 8;
        buf[base..base + 2].copy_from_slice(&beat.beat_number.to_be_bytes());
        buf[base + 2..base + 4].copy_from_slice(&beat.tempo.to_be_bytes());
        buf[base + 4..base + 8].copy_from_slice(&beat.time.to_be_bytes());
    }
    buf
}

/// Color waveform preview: 3 bytes per column (beat-link KnownType.WAVE_PREVIEW color path).
///
/// Wire format expected by beat-link WaveformPreview.java:
///   byte 0: whiteness[7:5] | height[4:0]
///   byte 1: red[7:3]    | green[4:2] (green high 3 bits)
///   byte 2: green[1:0]  | blue[6:2]  | 0 (1 bit pad)  (blue in bits 6-2)
///
/// Mapping from rekordcrate WaveformColorPreviewColumn (6 bytes per column):
///   unknown1/2   -> whiteness (unknown1 >> 5 gives 3-bit whiteness)
///   energy_top_third_freq   -> red   (high freq)
///   energy_mid_third_freq   -> green (mid freq)
///   energy_bottom_half_freq -> blue  (low freq / bass)
fn encode_color_waveform_preview(cols: &[rekordcrate::anlz::WaveformColorPreviewColumn]) -> Vec<u8> {
    let mut data = Vec::with_capacity(cols.len() * 3);
    for col in cols {
        // Height: max amplitude across all energy bands, scaled to 5 bits (0-31).
        let max_energy = col.energy_bottom_half_freq
            .max(col.energy_bottom_third_freq)
            .max(col.energy_mid_third_freq)
            .max(col.energy_top_third_freq);
        let height = (max_energy >> 3).min(31u8);

        // unknown1 top 3 bits carry whiteness (how "white"/bright the column is).
        let whiteness = (col.unknown1 >> 5) & 0x07;

        // Map energy bands to 5-bit RGB (0-31).
        let red   = col.energy_top_third_freq >> 3;
        let green = col.energy_mid_third_freq >> 3;
        let blue  = col.energy_bottom_half_freq >> 3;

        data.push((whiteness << 5) | (height & 0x1f));
        data.push(((red & 0x1f) << 3) | ((green >> 2) & 0x07));
        data.push(((green & 0x03) << 6) | ((blue & 0x1f) << 1));
    }
    data
}

fn encode_waveform_preview(cols: &[rekordcrate::anlz::WaveformPreviewColumn]) -> Vec<u8> {
    let mut data = Vec::with_capacity(cols.len() * 2);
    for col in cols {
        data.push((col.height() & 0x1f) | ((col.whiteness() & 0x7) << 5));
        data.push(0u8);
    }
    data
}

fn encode_tiny_waveform_preview(cols: &[rekordcrate::anlz::TinyWaveformPreviewColumn]) -> Vec<u8> {
    let mut data = Vec::with_capacity(cols.len() * 2);
    for col in cols {
        let height = (col.height() as u8 * 2).min(31);
        data.push(height & 0x1f);
        data.push(0u8);
    }
    data
}

// PWVD: 1 byte per column in file; BLT WaveformDetail expects 2 bytes per column (mono path).
fn encode_waveform_detail(cols: &[rekordcrate::anlz::WaveformPreviewColumn]) -> Vec<u8> {
    let mut data = Vec::with_capacity(cols.len() * 2);
    for col in cols {
        data.push((col.height() & 0x1f) | ((col.whiteness() & 0x7) << 5));
        data.push(0u8);
    }
    data
}

/// Color waveform detail (PWV5): 2 bytes per column in file (red:3,green:3,blue:3,height:5,unk:2).
/// Encodes to same 3-byte-per-column wire format as the preview (beat-link WAVE_DETAIL color path):
///   byte 0: 0[7:5]     | height[4:0]
///   byte 1: red[7:3]   | green_hi[2:0]
///   byte 2: green_lo[7:6] | blue[5:1] | 0
fn encode_color_waveform_detail(cols: &[rekordcrate::anlz::WaveformColorDetailColumn]) -> Vec<u8> {
    let mut data = Vec::with_capacity(cols.len() * 3);
    for col in cols {
        // Scale 3-bit values (0-7) to 5-bit (0-31) by shifting left 2.
        let red:   u8 = (col.red()   as u8) << 2;
        let green: u8 = (col.green() as u8) << 2;
        let blue:  u8 = (col.blue()  as u8) << 2;
        let height: u8 = col.height() & 0x1f;
        data.push(height);
        data.push(((red & 0x1f) << 3) | ((green >> 2) & 0x07));
        data.push(((green & 0x03) << 6) | ((blue & 0x1f) << 1));
    }
    data
}
