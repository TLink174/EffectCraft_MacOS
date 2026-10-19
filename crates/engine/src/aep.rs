//! Adobe After Effects project (`.aep` binary RIFX and `.aepx` XML) importer.
//!
//! # Clean-Room Specification Notes (`AGENTS.md` §2, Owner Decision 2026-10-16)
//!
//! Every chunk tag and byte offset in this module was learned strictly by inspecting the bytes of
//! contributor-created After Effects project files (`test_simple.aep`), without reading any
//! third-party `.aep` parser or Adobe SDK/application binaries:
//!
//! * **Container header (`0..12`):** Big-endian RIFF (`"RIFX"` at `0..4`, `u32` BE payload size at
//!   `4..8`, form type `"Egg!"` at `8..12`). Every subchunk has `[tag: 4B][len: u32 BE][payload]`,
//!   padded to an even byte boundary (`len + (len & 1)`). `"LIST"` chunks begin their payload with
//!   a 4-byte list type followed by child chunks.
//! * **Project items (`LIST Fold` / `LIST Sfdr` → `LIST Item`):**
//!   - `idta` (84 bytes): `u16` BE at `0..2` = item kind (`1` = Folder, `4` = Composition,
//!     `7` = Footage/Solid); `u32` BE at `16..20` = item ID.
//!   - `Utf8`: item display name.
//!   - `LIST Sfdr`: child items when the item is a folder.
//!   - `LIST Pin ` → `sspc` / `opti`: solid/footage dimensions (`u32` BE width at `30..34`, height
//!     at `34..38` in `sspc`) and solid colour (`4 × f32` BE ARGB at `10..26` and null-terminated
//!     fallback name at `26..` in `opti` when `opti[0..4] == b"Soli"`).
//! * **Composition header (`cdta`, 204 bytes, observed at `@2136` in `test_simple.aep`):**
//!   - `u32` BE at `4..8`: frame tick divisor (`1024`).
//!   - `u32` BE at `8..12`: `time_scale` (`30720` = `30 * 1024` ticks/second).
//!   - `u32` BE at `44..48`: composition duration in `time_scale` units (`92160` → `3.0` seconds).
//!   - `u8` × 3 at `52..55`: background RGB colour.
//!   - `u16` BE at `140..142`: `width` in pixels (`1080`).
//!   - `u16` BE at `142..144`: `height` in pixels (`1920`).
//!   - `u32` BE at `144..148` and `148..152`: pixel aspect ratio numerator and denominator (`1:1`).
//!   - `u16` BE at `156..158`: nominal integer frame rate (`30`).
//!   - `u16` BE at `174..176`: shutter angle in degrees (`180`).
//! * **Timeline layers (`LIST Layr` inside a composition's `LIST Item`):**
//!   - Internal view cameras (`LIST CLay`) and comp marker layers (`LIST SecL`) are skipped.
//!   - `ldta` (164 bytes, observed at `@2524` in `test_simple.aep`):
//!     - `u32` BE at `0..4`: layer ID (`18`).
//!     - `i32` BE at `12..16` and `u32` BE at `16..20`: `start_time` and its `time_scale`.
//!     - `i32` BE at `20..24` and `u32` BE at `24..28`: `in_point` and its `time_scale`.
//!     - `i32` BE at `28..32` and `u32` BE at `32..36`: `out_point` and its `time_scale`.
//!     - `u32` BE at `36..40`: layer switches (`bit 0` = video active, `bit 12` = 3D layer).
//!     - `u32` BE at `56..60`: source item ID (`0` for shape/text/null/camera/light layers).
//!     - `u16` BE at `60..62`: layer kind flags (`0x0008` = Shape layer).
//!     - `u32` BE at `108..112`: parent layer ID (`0` = none).
//!   - `Utf8`: layer name (`"Shape Layer 1"`).
//!   - `LIST tdgp`: root property group containing `tdmn` (40-byte null-padded ASCII match name)
//!     followed by child `LIST tdgp` (subgroups) or `LIST tdbs` (property streams), terminated by
//!     `tdmn == "ADBE Group End"`.
//! * **Property streams (`LIST tdbs`):**
//!   - `tdb4` (124 bytes):
//!     - `u16` BE at `2..4`: dimension count (`1`, `2`, `3`, `4`).
//!     - `u16` BE at `4..6`: spatial/type flags (`flags & 0x0002 != 0` indicates a spatial point
//!       stream such as `ADBE Position` or `ADBE Anchor Point`).
//!     - `u16` BE at `8..10`: unit-ratio flag (`0x0001` on `ADBE Scale` and `ADBE Opacity` means
//!       `1.0` in `cdat`/`ldat` represents `100%`).
//!     - `u32` BE at `12..16`: stream `time_scale` (`30720`).
//!   - `cdat`: static `f64` BE values (`dim × 8` bytes).
//!   - `LIST list` → `lhd3` (52 bytes: `u32` BE `key_count` at `8..12`, `u32` BE `key_stride` at
//!     `16..20`) + `ldat` (`key_count × key_stride` bytes):
//!     - Per keyframe at `base = k * key_stride`:
//!       - `i32` BE at `base..base+4`: key time in `time_scale` ticks.
//!       - `u8` at `base+4`: `in_interp` (`1` = Linear, `2` = Bezier, `3` = Hold).
//!       - `u8` at `base+5`: `out_interp` (`1` = Linear, `2` = Bezier, `3` = Hold).
//!       - Spatial (`key_stride >= 128` and `spatial == true`):
//!         - `f64` BE at `base+16`, `base+32`: `in_speed`, `in_influence` (`0..1`).
//!         - `f64` BE at `base+40`, `base+48`: `out_speed`, `out_influence` (`0..1`).
//!         - `3 × f64` BE at `base+56..base+80`: `[x, y, z]` position/anchor value.
//!         - `3 × f64` BE at `base+80..base+104`: `spatial_in` tangent `[ix, iy, iz]`.
//!         - `3 × f64` BE at `base+104..base+128`: `spatial_out` tangent `[ox, oy, oz]`.
//!       - Non-spatial (`spatial == false`):
//!         - `dim × f64` BE at `base+8..base+8+dim*8`: value components.
//!         - Followed by `dim` `in_speed`, `dim` `in_influence`, `dim` `out_speed`, and `dim`
//!           `out_influence` `f64` BE values when `base + 8 + dim * 40 <= base + key_stride`.

use std::collections::BTreeMap;
use std::sync::Arc;

use effectcraft_color::{BlendMode, Label};
use effectcraft_keyframe::{Ease, Gradient, Interp, Justify, Keyframe, ShapePath, TextDoc, Value};
use effectcraft_project::build::{self, Ids};
use effectcraft_project::{
    AlphaMode, Comp, FieldOrder, Footage, FootageKind, GroupKind, ItemId, ItemKind, Layer, LayerId, LayerSource, LightKind, Marker, MaskMode,
    MatteKind, Node, Project, PropGroup, Property, Solid, TrackMatte,
};
use effectcraft_time::{FrameRate, Tick};

/// Maximum nesting depth for RIFX chunks, property groups and XML nodes (`AGENTS.md` bound recursion).
const MAX_DEPTH: usize = 64;

/// Maximum number of keyframes per property stream to cap hostile allocations.
const MAX_KEYS: usize = 100_000;

/// Whether `path` has an After Effects project extension (`.aep` or `.aepx`).
pub fn is_aep_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".aep") || lower.ends_with(".aepx")
}

/// Whether `bytes` begins with an After Effects RIFX `"Egg!"` header or `<AfterEffectsProject` XML.
pub fn is_aep_bytes(bytes: &[u8]) -> bool {
    if bytes.len() >= 12 && bytes.get(0..4) == Some(b"RIFX") && bytes.get(8..12) == Some(b"Egg!") {
        return true;
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        let trimmed = text.trim_start_matches('\u{feff}').trim_start();
        if trimmed.contains("<AfterEffectsProject") {
            return true;
        }
    }
    false
}

/// Resolves a footage file path, prioritizing the exact path recorded in the AEP file,
/// with automatic cross-platform mapping (Windows UNC <-> macOS /Volumes/) and project-relative fallback.
pub fn resolve_footage_path(raw: &mut String, dir: Option<&std::path::Path>, services: &dyn crate::Services) -> bool {
    if raw.is_empty() {
        return true;
    }
    let check = |p: &str| -> Option<String> {
        if services.exists(p) {
            return Some(p.to_string());
        }
        let slash = p.replace('\\', "/");
        if services.exists(&slash) {
            return Some(slash);
        }
        let bslash = p.replace('/', "\\");
        if services.exists(&bslash) {
            return Some(bslash);
        }
        None
    };

    // 1. Highest priority: exact path as recorded in the AEP file.
    if let Some(found) = check(raw) {
        *raw = found;
        return true;
    }

    // 2. Cross-platform network / volume mapping
    // Windows UNC on Unix/macOS: `\\server\share\subpath` -> `/Volumes/share/subpath`
    if raw.starts_with(r"\\") || raw.starts_with("//") {
        let normalized = raw.replace('\\', "/");
        let trimmed = normalized.trim_start_matches('/');
        let mut parts = trimmed.splitn(3, '/');
        let server = parts.next();
        if let Some(share) = parts.next() {
            let subpath = parts.next().unwrap_or("");
            let cand1 = if subpath.is_empty() {
                format!("/Volumes/{share}")
            } else {
                format!("/Volumes/{share}/{subpath}")
            };
            if let Some(found) = check(&cand1) {
                *raw = found;
                return true;
            }
            if let Some(srv) = server {
                let cand2 = if subpath.is_empty() {
                    format!("/Volumes/{srv}/{share}")
                } else {
                    format!("/Volumes/{srv}/{share}/{subpath}")
                };
                if let Some(found) = check(&cand2) {
                    *raw = found;
                    return true;
                }
            }
        }
    }

    // Windows drive letter on Unix/macOS: `D:\folder\file` -> `/Volumes/D/folder/file`
    if raw.len() >= 3 && raw.as_bytes().get(1) == Some(&b':') && (raw.as_bytes().get(2) == Some(&b'\\') || raw.as_bytes().get(2) == Some(&b'/')) {
        let drive = &raw[..1];
        let subpath = raw[3..].replace('\\', "/");
        let cand = format!("/Volumes/{drive}/{subpath}");
        if let Some(found) = check(&cand) {
            *raw = found;
            return true;
        }
        let cand_lower = format!("/Volumes/{}/{subpath}", drive.to_ascii_lowercase());
        if let Some(found) = check(&cand_lower) {
            *raw = found;
            return true;
        }
    }

    // Unix `/Volumes/share/subpath` on Windows: check `\\share\subpath`
    if let Some(vol) = raw.strip_prefix("/Volumes/") {
        let win_unc = format!(r"\\{}", vol.replace('/', r"\"));
        if let Some(found) = check(&win_unc) {
            *raw = found;
            return true;
        }
    }

    // 3. Project-relative fallback (if project was copied/moved together with its assets)
    if let Some(dir) = dir {
        let joined = dir.join(raw.as_str()).to_string_lossy().to_string();
        if let Some(found) = check(&joined) {
            *raw = found;
            return true;
        }

        let filename = std::path::Path::new(raw.as_str())
            .file_name()
            .map(|n| n.to_string_lossy().to_string());
        if let Some(name) = filename {
            let beside = dir.join(&name).to_string_lossy().to_string();
            if let Some(found) = check(&beside) {
                *raw = found;
                return true;
            }

            // Suffix check: try last 2, 3 path components (e.g. "Footer/Comp 1.mov")
            let norm = raw.replace('\\', "/");
            let segments: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
            for take_len in 2..=4 {
                if segments.len() >= take_len {
                    let mut cand_buf = dir.to_path_buf();
                    for seg in &segments[segments.len().saturating_sub(take_len)..] {
                        cand_buf.push(seg);
                    }
                    let cand_str = cand_buf.to_string_lossy().to_string();
                    if let Some(found) = check(&cand_str) {
                        *raw = found;
                        return true;
                    }
                }
            }

            // Check common footage subdirectories next to project file
            const COMMON_MEDIA_DIRS: &[&str] = &["Footage", "(Footage)", "Assets", "Media", "Source", "01-INPUT", "02-SOURCE", "Video", "Audio"];
            for sub in COMMON_MEDIA_DIRS {
                let sub_cand = dir.join(sub).join(&name).to_string_lossy().to_string();
                if let Some(found) = check(&sub_cand) {
                    *raw = found;
                    return true;
                }
            }
        }
    }

    // Not found anywhere on disk; retain original AEP path for user reporting & manual relink
    false
}

// ----------------------------------------------------------------------------
// Step 2: Coordinate System & Unit Conversion Helpers (AE -> EffectCraft)
// ----------------------------------------------------------------------------

/// Convert an After Effects composition-space 3D position `(x, y, z)` (Top-Left origin `(0, 0)`,
/// +X right, +Y down, +Z into screen) into EffectCraft's internal composition coordinate system.
///
/// Both After Effects and EffectCraft place `(0, 0)` at the top-left corner of the composition
/// with +X pointing right and +Y pointing down (`crates/geom`, `crates/render`). Non-finite
/// coordinates fall back to the composition centre so a corrupt file never poisons rendering.
pub fn aep_position_to_ec(aep_pos: [f64; 3], comp_width: u32, comp_height: u32) -> [f64; 3] {
    let cx = f64::from(comp_width) * 0.5;
    let cy = f64::from(comp_height) * 0.5;
    [
        if aep_pos[0].is_finite() { aep_pos[0] } else { cx },
        if aep_pos[1].is_finite() { aep_pos[1] } else { cy },
        if aep_pos[2].is_finite() { aep_pos[2] } else { 0.0 },
    ]
}

/// Convert an After Effects layer anchor point `(ax, ay, az)` into EffectCraft's internal anchor
/// point coordinates.
pub fn aep_anchor_to_ec(aep_anchor: [f64; 3]) -> [f64; 3] {
    [
        if aep_anchor[0].is_finite() { aep_anchor[0] } else { 0.0 },
        if aep_anchor[1].is_finite() { aep_anchor[1] } else { 0.0 },
        if aep_anchor[2].is_finite() { aep_anchor[2] } else { 0.0 },
    ]
}

/// Convert an After Effects scale vector into EffectCraft's percentage scale (`100.0` = 100%).
///
/// In binary `.aep` files, layer-level `ADBE Scale` stores unit ratios (`1.0` = `100%`,
/// `0.46` = `46%`, `1.2` = `120%`) when `tdb4` unit flag `0x0001` is set (`unit_ratio == true`),
/// whereas vector shape group `ADBE Vector Scale` stores percentages directly (`unit_ratio == false`).
pub fn aep_scale_to_ec(aep_scale: [f64; 3], unit_ratio: bool) -> [f64; 3] {
    let factor = if unit_ratio { 100.0 } else { 1.0 };
    let conv = |v: f64| if v.is_finite() { v * factor } else { 100.0 };
    [conv(aep_scale[0]), conv(aep_scale[1]), conv(aep_scale[2])]
}

/// Convert an After Effects rotation angle in degrees (clockwise positive in Y-down screen space)
/// into EffectCraft's rotation angle in degrees.
pub fn aep_rotation_to_ec(aep_deg: f64) -> f64 {
    if aep_deg.is_finite() { aep_deg } else { 0.0 }
}

/// Convert a 2D vector shape group position relative to the layer anchor point `(0, 0)` (Y-down).
pub fn aep_vector_pos_to_ec(aep_vec_pos: [f64; 2]) -> [f64; 2] {
    [
        if aep_vec_pos[0].is_finite() { aep_vec_pos[0] } else { 0.0 },
        if aep_vec_pos[1].is_finite() { aep_vec_pos[1] } else { 0.0 },
    ]
}

// ----------------------------------------------------------------------------
// Step 1: Parsed AEP Data Structures
// ----------------------------------------------------------------------------

/// Parsed After Effects project model before mapping to EffectCraft's [`Project`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AepProject {
    pub folders: Vec<AepFolder>,
    pub solids: Vec<AepSolid>,
    pub footages: Vec<AepFootage>,
    pub comps: Vec<AepComp>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AepFolder {
    pub id: u32,
    pub name: String,
    pub parent_id: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AepSolid {
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub color: [f32; 3],
    pub parent_id: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AepFootage {
    pub id: u32,
    pub name: String,
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration_seconds: f64,
    pub has_video: bool,
    pub has_audio: bool,
    pub parent_id: Option<u32>,
}

/// Basic Composition parameters and Layer list parsed from `.aep` / `.aepx`.
#[derive(Clone, Debug, PartialEq)]
pub struct AepComp {
    pub id: u32,
    pub name: String,
    pub parent_id: Option<u32>,
    pub width: u32,
    pub height: u32,
    pub pixel_aspect: f64,
    pub fps: f64,
    pub duration_seconds: f64,
    pub background: [f32; 3],
    pub shutter_angle: f64,
    pub markers: Vec<AepMarker>,
    pub layers: Vec<AepLayer>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AepLayerKind {
    Shape,
    Text,
    Null,
    Solid(u32),
    Comp(u32),
    Footage(u32),
    Camera,
    Light(LightKind),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AepText {
    pub text: String,
    pub font_family: String,
    pub font_size: f64,
    pub fill_color: [f32; 4],
    pub stroke_color: [f32; 4],
    pub stroke_width: f64,
    pub justification: u8,
    pub tracking: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AepCamera {
    pub zoom: Option<AepProperty>,
    pub focus_distance: Option<AepProperty>,
    pub aperture: Option<AepProperty>,
    pub blur_level: Option<AepProperty>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AepLight {
    pub kind: LightKind,
    pub intensity: Option<AepProperty>,
    pub color: Option<AepProperty>,
    pub cone_angle: Option<AepProperty>,
    pub cone_feather: Option<AepProperty>,
    pub casts_shadows: Option<AepProperty>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AepMask {
    pub name: String,
    pub mode: MaskMode,
    pub inverted: bool,
    pub path: Option<AepShapePath>,
    pub feather: Option<AepProperty>,
    pub opacity: Option<AepProperty>,
    pub expansion: Option<AepProperty>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AepEffect {
    pub name: String,
    pub match_name: String,
    pub enabled: bool,
    pub properties: Vec<AepProperty>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AepMarker {
    pub time_seconds: f64,
    pub duration_seconds: f64,
    pub comment: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AepLayer {
    pub id: u32,
    pub name: String,
    pub kind: AepLayerKind,
    pub source_id: u32,
    pub start_time_seconds: f64,
    pub in_point_seconds: f64,
    pub out_point_seconds: f64,
    pub stretch: f64,
    pub video_active: bool,
    pub audio_active: bool,
    pub solo: bool,
    pub locked: bool,
    pub shy: bool,
    pub collapse: bool,
    pub motion_blur: bool,
    pub adjustment: bool,
    pub three_d: bool,
    pub blend_mode: BlendMode,
    pub track_matte_kind: Option<MatteKind>,
    pub parent_id: Option<u32>,
    pub transform: AepTransform,
    pub text: Option<AepText>,
    pub camera: Option<AepCamera>,
    pub light: Option<AepLight>,
    pub shape_groups: Vec<AepShapeGroup>,
    pub masks: Vec<AepMask>,
    pub effects: Vec<AepEffect>,
    pub markers: Vec<AepMarker>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AepTransform {
    pub anchor_point: Option<AepProperty>,
    pub position: Option<AepProperty>,
    pub poi: Option<AepProperty>,
    pub scale: Option<AepProperty>,
    pub orientation: Option<AepProperty>,
    pub rotate_x: Option<AepProperty>,
    pub rotate_y: Option<AepProperty>,
    pub rotate_z: Option<AepProperty>,
    pub opacity: Option<AepProperty>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AepProperty {
    pub match_name: String,
    pub name: String,
    pub dims: u16,
    pub spatial: bool,
    pub unit_ratio: bool,
    pub static_value: Vec<f64>,
    pub keyframes: Vec<AepKeyframe>,
    pub expression: Option<String>,
}

impl Default for AepProperty {
    fn default() -> Self {
        Self {
            match_name: String::new(),
            name: String::new(),
            dims: 1,
            spatial: false,
            unit_ratio: false,
            static_value: Vec::new(),
            keyframes: Vec::new(),
            expression: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AepKeyframe {
    pub time_seconds: f64,
    pub value: Vec<f64>,
    pub in_interp: Interp,
    pub out_interp: Interp,
    pub in_ease: Vec<Ease>,
    pub out_ease: Vec<Ease>,
    pub spatial_in: [f64; 3],
    pub spatial_out: [f64; 3],
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AepShapePath {
    pub closed: bool,
    pub vertices: Vec<[f64; 2]>,
    pub in_tangents: Vec<[f64; 2]>,
    pub out_tangents: Vec<[f64; 2]>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AepShapeGroup {
    pub name: String,
    pub rect_size: Option<AepProperty>,
    pub rect_position: Option<AepProperty>,
    pub rect_roundness: Option<AepProperty>,
    pub ellipse_size: Option<AepProperty>,
    pub ellipse_position: Option<AepProperty>,
    pub star_type: Option<AepProperty>,
    pub star_points: Option<AepProperty>,
    pub star_position: Option<AepProperty>,
    pub star_rotation: Option<AepProperty>,
    pub star_inner_radius: Option<AepProperty>,
    pub star_outer_radius: Option<AepProperty>,
    pub star_inner_roundness: Option<AepProperty>,
    pub star_outer_roundness: Option<AepProperty>,
    pub custom_path: Option<AepShapePath>,
    pub stroke_color: Option<AepProperty>,
    pub stroke_width: Option<AepProperty>,
    pub stroke_opacity: Option<AepProperty>,
    pub fill_color: Option<AepProperty>,
    pub fill_opacity: Option<AepProperty>,
    pub gfill_type: Option<AepProperty>,
    pub gfill_start: Option<AepProperty>,
    pub gfill_end: Option<AepProperty>,
    pub gfill_colors: Option<AepProperty>,
    pub gfill_opacity: Option<AepProperty>,
    pub gstroke_type: Option<AepProperty>,
    pub gstroke_start: Option<AepProperty>,
    pub gstroke_end: Option<AepProperty>,
    pub gstroke_colors: Option<AepProperty>,
    pub gstroke_width: Option<AepProperty>,
    pub gstroke_opacity: Option<AepProperty>,
    pub trim_start: Option<AepProperty>,
    pub trim_end: Option<AepProperty>,
    pub trim_offset: Option<AepProperty>,
    pub repeater_copies: Option<AepProperty>,
    pub repeater_offset: Option<AepProperty>,
    pub transform: AepTransform,
}

// ----------------------------------------------------------------------------
// Binary RIFX Reader
// ----------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Chunk<'a> {
    Leaf { tag: [u8; 4], data: &'a [u8] },
    List { list_type: [u8; 4], children: Vec<Chunk<'a>> },
}

fn u16_be(data: &[u8], offset: usize) -> Option<u16> {
    let bytes: [u8; 2] = data.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_be_bytes(bytes))
}

fn u32_be(data: &[u8], offset: usize) -> Option<u32> {
    let bytes: [u8; 4] = data.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(bytes))
}

fn i32_be(data: &[u8], offset: usize) -> Option<i32> {
    let bytes: [u8; 4] = data.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(i32::from_be_bytes(bytes))
}

fn f32_be(data: &[u8], offset: usize) -> Option<f32> {
    let bytes: [u8; 4] = data.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(f32::from_be_bytes(bytes))
}

fn f64_be(data: &[u8], offset: usize) -> Option<f64> {
    let bytes: [u8; 8] = data.get(offset..offset.checked_add(8)?)?.try_into().ok()?;
    Some(f64::from_be_bytes(bytes))
}

fn parse_chunks(data: &[u8], depth: usize) -> Result<Vec<Chunk<'_>>, String> {
    if depth > MAX_DEPTH {
        return Err(format!("AEP chunk hierarchy nested deeper than {MAX_DEPTH} levels"));
    }
    let mut out = Vec::new();
    let mut pos: usize = 0;
    while let Some(&[t0, t1, t2, t3, l0, l1, l2, l3]) = data.get(pos..pos.saturating_add(8)) {
        let tag = [t0, t1, t2, t3];
        let len = u32::from_be_bytes([l0, l1, l2, l3]) as usize;
        let payload_start = pos.saturating_add(8);
        let payload_end = payload_start.saturating_add(len).min(data.len());
        let payload = data.get(payload_start..payload_end).unwrap_or(&[]);
        if &tag == b"LIST"
            && let Some(&[lt0, lt1, lt2, lt3]) = payload.get(0..4)
        {
            let list_type = [lt0, lt1, lt2, lt3];
            let body = payload.get(4..).unwrap_or(&[]);
            let children = parse_chunks(body, depth + 1)?;
            out.push(Chunk::List { list_type, children });
        } else {
            out.push(Chunk::Leaf { tag, data: payload });
        }
        let padded = len.saturating_add(len & 1);
        let next = payload_start.saturating_add(padded);
        if next <= pos {
            break;
        }
        pos = next;
    }
    Ok(out)
}

fn utf8_from_chunk(data: &[u8]) -> String {
    let slice = if data.len() >= 8 && data.get(0..4) == Some(b"Utf8") {
        let n = u32_be(data, 4).unwrap_or(0) as usize;
        let end = 8usize.saturating_add(n).min(data.len());
        data.get(8..end).unwrap_or(&[])
    } else {
        data
    };
    let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    String::from_utf8_lossy(slice.get(..end).unwrap_or(&[])).trim().to_string()
}

fn match_name_from_tdmn(data: &[u8]) -> String {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    String::from_utf8_lossy(data.get(..end).unwrap_or(&[])).trim().to_string()
}

/// Parse an After Effects `.aep` (binary RIFX) or `.aepx` (XML) byte slice into [`AepProject`].
pub fn parse_aep(bytes: &[u8], fallback_name: &str) -> Result<AepProject, String> {
    if bytes.len() >= 12 && bytes.get(0..4) == Some(b"RIFX") && bytes.get(8..12) == Some(b"Egg!") {
        let file_end = u32_be(bytes, 4).map(|n| (n as usize).saturating_add(8).min(bytes.len())).unwrap_or(bytes.len());
        let body = bytes.get(12..file_end).unwrap_or(&[]);
        let chunks = parse_chunks(body, 0)?;
        let mut proj = AepProject::default();
        for chunk in &chunks {
            if let Chunk::List { list_type: [b'F', b'o', b'l', b'd'], children } = chunk {
                parse_folder_items(children, None, &mut proj, 0)?;
            }
        }
        return Ok(proj);
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        let trimmed = text.trim_start_matches('\u{feff}').trim_start();
        if trimmed.starts_with("<?xml") || trimmed.starts_with("<AfterEffectsProject") {
            return parse_aepx_xml(trimmed, fallback_name);
        }
    }
    Err("not a valid After Effects .aep (RIFX Egg!) or .aepx (XML) file".into())
}

fn parse_folder_items(children: &[Chunk<'_>], parent_id: Option<u32>, proj: &mut AepProject, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!("AEP folder hierarchy nested deeper than {MAX_DEPTH} levels"));
    }
    for chunk in children {
        let Chunk::List { list_type: [b'I', b't', b'e', b'm'], children: item_children } = chunk else {
            continue;
        };
        let mut item_type: u16 = 0;
        let mut item_id: u32 = 0;
        let mut name = String::new();
        let mut cdta: Option<&[u8]> = None;
        for sub in item_children {
            match sub {
                Chunk::Leaf { tag: [b'i', b'd', b't', b'a'], data } => {
                    item_type = u16_be(data, 0).unwrap_or(0);
                    item_id = u32_be(data, 16).unwrap_or(0);
                }
                Chunk::Leaf { tag: [b'U', b't', b'f', b'8'], data } if name.is_empty() => {
                    name = utf8_from_chunk(data);
                }
                Chunk::Leaf { tag: [b'c', b'd', b't', b'a'], data } => {
                    cdta = Some(data);
                }
                _ => {}
            }
        }
        match item_type {
            // Folder item
            1 => {
                let folder_name = if name.is_empty() { format!("Folder {item_id}") } else { name };
                // The root folder (item_type == 1 named "Untitled 1" or similar with parent_id == None)
                // contains the top-level project items in its `LIST Sfdr`.
                let next_parent = if parent_id.is_none() && folder_name.starts_with("Untitled") {
                    None
                } else {
                    proj.folders.push(AepFolder { id: item_id, name: folder_name, parent_id });
                    Some(item_id)
                };
                for sub in item_children {
                    if let Chunk::List { list_type: [b'S', b'f', b'd', b'r'], children: sfdr_children } = sub {
                        parse_folder_items(sfdr_children, next_parent, proj, depth + 1)?;
                    }
                }
            }
            // Composition item
            4 => {
                if let Some(cdta_bytes) = cdta {
                    let comp_name = if name.is_empty() { format!("Comp {item_id}") } else { name };
                    let comp = parse_comp_item(item_id, comp_name, parent_id, cdta_bytes, item_children)?;
                    proj.comps.push(comp);
                }
            }
            // Footage / Solid item
            7 => {
                if let Some(solid) = parse_solid_item(item_id, &name, parent_id, item_children) {
                    proj.solids.push(solid);
                } else if let Some(footage) = parse_footage_item(item_id, &name, parent_id, item_children) {
                    proj.footages.push(footage);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn extract_fullpath_from_alas(data: &[u8]) -> Option<String> {
    let clean = data.split(|&b| b == 0).next().unwrap_or(data);
    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(clean) {
        if let Some(fp) = val.get("fullpath").and_then(|v| v.as_str()) {
            if !fp.is_empty() {
                return Some(fp.to_string());
            }
        }
    }
    let s = std::str::from_utf8(clean).ok()?;
    let pat = "\"fullpath\":\"";
    let start = s.find(pat)?.saturating_add(pat.len());
    let rest = s.get(start..)?;
    let end = rest.find('"')?;
    let path = rest.get(..end)?;
    Some(path.replace("\\\\", "\\"))
}

fn parse_footage_item(item_id: u32, name: &str, parent_id: Option<u32>, item_children: &[Chunk<'_>]) -> Option<AepFootage> {
    let mut width: u32 = 1920;
    let mut height: u32 = 1080;
    let mut fps: f64 = 30.0;
    let mut duration_seconds: f64 = 0.0;
    let mut path = String::new();
    let mut has_video = true;
    let mut has_audio = false;

    for sub in item_children {
        match sub {
            Chunk::Leaf { tag: [b'f', b'i', b'l', b'p'] | [b'f', b'n', b'a', b'm'] | [b'p', b'a', b't', b'h'], data } if path.is_empty() => {
                path = utf8_from_chunk(data);
            }
            Chunk::List { list_type: [b'P', b'i', b'n', b' '], children: pin_children } => {
                for psub in pin_children {
                    match psub {
                        Chunk::Leaf { tag: [b's', b's', b'p', b'c'], data } => {
                            if let Some(w) = u32_be(data, 30).filter(|&w| w > 0) {
                                width = w;
                            }
                            if let Some(h) = u32_be(data, 34).filter(|&h| h > 0) {
                                height = h;
                            }
                            let dur_raw = u32_be(data, 48).unwrap_or(0);
                            let dur_ts = u32_be(data, 52).filter(|&ts| ts > 0).unwrap_or(30720);
                            if dur_raw > 0 {
                                duration_seconds = f64::from(dur_raw) / f64::from(dur_ts);
                            }
                            let scale = u32_be(data, 8).unwrap_or(0);
                            let rate_raw = u32_be(data, 12).unwrap_or(0);
                            if scale > 0 && rate_raw > 0 {
                                let f = f64::from(rate_raw) / f64::from(scale);
                                if f > 0.1 && f <= 999.0 {
                                    fps = f;
                                }
                            }
                        }
                        Chunk::Leaf { tag: [b'A', b'L', b'F', b'P'] | [b'o', b'p', b't', b'i'], data } if path.is_empty() => {
                            let s = utf8_from_chunk(data);
                            if s.contains('.') || s.contains('/') || s.contains('\\') {
                                path = s;
                            }
                        }
                        Chunk::Leaf { tag: [b'a', b'l', b'a', b's'], data } if path.is_empty() => {
                            if let Some(fp) = extract_fullpath_from_alas(data) {
                                path = fp;
                            }
                        }
                        Chunk::List { list_type: [b'A', b'l', b's', b'2'], children: als2_children } => {
                            for asub in als2_children {
                                if let Chunk::Leaf { tag: [b'a', b'l', b'a', b's'], data } = asub {
                                    if path.is_empty() {
                                        if let Some(fp) = extract_fullpath_from_alas(data) {
                                            path = fp;
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Chunk::List { list_type: [b'A', b'l', b's', b'2'], children: als2_children } => {
                for asub in als2_children {
                    if let Chunk::Leaf { tag: [b'a', b'l', b'a', b's'], data } = asub {
                        if path.is_empty() {
                            if let Some(fp) = extract_fullpath_from_alas(data) {
                                path = fp;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let final_name = if !name.is_empty() {
        name.to_string()
    } else if !path.is_empty() {
        std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("Footage {item_id}"))
    } else {
        format!("Footage {item_id}")
    };

    if path.is_empty() {
        path = final_name.clone();
    }

    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".wav") || lower.ends_with(".mp3") || lower.ends_with(".aac") || lower.ends_with(".m4a") {
        has_video = false;
        has_audio = true;
    }

    Some(AepFootage {
        id: item_id,
        name: final_name,
        path,
        width,
        height,
        fps,
        duration_seconds,
        has_video,
        has_audio,
        parent_id,
    })
}

fn parse_solid_item(item_id: u32, name: &str, parent_id: Option<u32>, item_children: &[Chunk<'_>]) -> Option<AepSolid> {
    for sub in item_children {
        let Chunk::List { list_type: [b'P', b'i', b'n', b' '], children: pin_children } = sub else {
            continue;
        };
        let mut width: u32 = 100;
        let mut height: u32 = 100;
        let mut color = [1.0f32, 1.0, 1.0];
        let mut opti_name = String::new();
        let mut is_solid = false;
        for psub in pin_children {
            match psub {
                Chunk::Leaf { tag: [b's', b's', b'p', b'c'], data } => {
                    if data.get(22..26) == Some(b"Soli") {
                        is_solid = true;
                    }
                    if let Some(w) = u32_be(data, 30).filter(|&w| w > 0) {
                        width = w;
                    }
                    if let Some(h) = u32_be(data, 34).filter(|&h| h > 0) {
                        height = h;
                    }
                }
                Chunk::Leaf { tag: [b'o', b'p', b't', b'i'], data } => {
                    if data.get(0..4) == Some(b"Soli") {
                        is_solid = true;
                        let r = f32_be(data, 14).unwrap_or(1.0).clamp(0.0, 1.0);
                        let g = f32_be(data, 18).unwrap_or(1.0).clamp(0.0, 1.0);
                        let b = f32_be(data, 22).unwrap_or(1.0).clamp(0.0, 1.0);
                        color = [r, g, b];
                        if let Some(tail) = data.get(26..) {
                            opti_name = utf8_from_chunk(tail);
                        }
                    }
                }
                _ => {}
            }
        }
        if is_solid {
            let final_name = if !name.is_empty() {
                name.to_string()
            } else if !opti_name.is_empty() {
                opti_name
            } else {
                format!("Solid {item_id}")
            };
            return Some(AepSolid { id: item_id, name: final_name, width, height, color, parent_id });
        }
    }
    None
}

fn parse_comp_item(
    item_id: u32,
    name: String,
    parent_id: Option<u32>,
    cdta: &[u8],
    item_children: &[Chunk<'_>],
) -> Result<AepComp, String> {
    let divisor = u32_be(cdta, 4).filter(|&d| d > 0).unwrap_or(1024);
    let time_scale = u32_be(cdta, 8).filter(|&ts| ts > 0).unwrap_or(30 * divisor);
    let duration_raw = u32_be(cdta, 44).unwrap_or(time_scale);
    let duration_ts = u32_be(cdta, 48).filter(|&ts| ts > 0).unwrap_or(time_scale);
    let width = u32::from(u16_be(cdta, 140).filter(|&w| w > 0).unwrap_or(1920));
    let height = u32::from(u16_be(cdta, 142).filter(|&h| h > 0).unwrap_or(1080));
    let par_num = u32_be(cdta, 144).filter(|&n| n > 0).unwrap_or(1);
    let par_den = u32_be(cdta, 148).filter(|&d| d > 0).unwrap_or(1);
    let pixel_aspect = f64::from(par_num) / f64::from(par_den);
    let fps_from_scale = f64::from(time_scale) / f64::from(divisor);
    let fps_int = u16_be(cdta, 156).unwrap_or(30);
    let fps = if fps_from_scale > 0.1 && fps_from_scale <= 999.0 {
        fps_from_scale
    } else if fps_int > 0 {
        f64::from(fps_int)
    } else {
        30.0
    };
    let duration_seconds = (f64::from(duration_raw) / f64::from(duration_ts)).max(1.0 / fps);
    let bg_r = cdta.get(52).copied().unwrap_or(0) as f32 / 255.0;
    let bg_g = cdta.get(53).copied().unwrap_or(0) as f32 / 255.0;
    let bg_b = cdta.get(54).copied().unwrap_or(0) as f32 / 255.0;
    let shutter_angle = f64::from(u16_be(cdta, 174).unwrap_or(180));

    let mut layers = Vec::new();
    let mut markers = Vec::new();
    for sub in item_children {
        match sub {
            Chunk::List { list_type: [b'L', b'a', b'y', b'r'], children: layr_children } => {
                if let Some(layer) = parse_layer_chunk(layr_children, fps, duration_seconds)? {
                    layers.push(layer);
                }
            }
            Chunk::List { list_type: [b'S', b'e', b'c', b'L'], children: secl_children } => {
                markers.extend(parse_secl_markers(secl_children, fps));
            }
            _ => {}
        }
    }

    Ok(AepComp {
        id: item_id,
        name,
        parent_id,
        width,
        height,
        pixel_aspect,
        fps,
        duration_seconds,
        background: [bg_r, bg_g, bg_b],
        shutter_angle,
        markers,
        layers,
    })
}

fn parse_secl_markers(children: &[Chunk<'_>], _fps: f64) -> Vec<AepMarker> {
    let mut markers = Vec::new();
    for c in children {
        match c {
            Chunk::Leaf { tag: [b's', b'e', b'c', b'l'] | [b'm', b'a', b'r', b'k'] | [b'c', b'u', b'e', b'p'], data } => {
                let time_raw = i32_be(data, 0).unwrap_or(0);
                let time_scale = u32_be(data, 4).filter(|&ts| ts > 0).unwrap_or(30720);
                let time_seconds = f64::from(time_raw) / f64::from(time_scale);
                let dur_raw = u32_be(data, 8).unwrap_or(0);
                let duration_seconds = f64::from(dur_raw) / f64::from(time_scale);
                let comment = if let Some(str_bytes) = data.get(16..) {
                    utf8_from_chunk(str_bytes)
                } else {
                    String::new()
                };
                markers.push(AepMarker { time_seconds, duration_seconds, comment });
            }
            _ => {}
        }
    }
    markers
}

/// Decode an After Effects binary blend mode byte into EffectCraft's [`BlendMode`].
pub fn decode_blend_mode(code: u8) -> BlendMode {
    match code {
        0 => BlendMode::Normal,
        1 => BlendMode::Dissolve,
        2 => BlendMode::DancingDissolve,
        3 => BlendMode::Darken,
        4 => BlendMode::Multiply,
        5 => BlendMode::ColorBurn,
        6 => BlendMode::ClassicColorBurn,
        7 => BlendMode::LinearBurn,
        8 => BlendMode::DarkerColor,
        9 => BlendMode::Add,
        10 => BlendMode::Lighten,
        11 => BlendMode::Screen,
        12 => BlendMode::ColorDodge,
        13 => BlendMode::ClassicColorDodge,
        14 => BlendMode::LinearDodge,
        15 => BlendMode::LighterColor,
        16 => BlendMode::Overlay,
        17 => BlendMode::SoftLight,
        18 => BlendMode::HardLight,
        19 => BlendMode::VividLight,
        20 => BlendMode::LinearLight,
        21 => BlendMode::PinLight,
        22 => BlendMode::HardMix,
        23 => BlendMode::Difference,
        24 => BlendMode::Exclusion,
        25 => BlendMode::Subtract,
        26 => BlendMode::Divide,
        27 => BlendMode::Hue,
        28 => BlendMode::Saturation,
        29 => BlendMode::Color,
        30 => BlendMode::Luminosity,
        _ => BlendMode::Normal,
    }
}

/// Decode an After Effects track matte byte into EffectCraft's [`MatteKind`].
pub fn decode_track_matte(code: u8) -> Option<MatteKind> {
    match code {
        1 => Some(MatteKind::Alpha),
        2 => Some(MatteKind::AlphaInverted),
        3 => Some(MatteKind::Luma),
        4 => Some(MatteKind::LumaInverted),
        _ => None,
    }
}

fn parse_layer_chunk(layr_children: &[Chunk<'_>], comp_fps: f64, comp_duration: f64) -> Result<Option<AepLayer>, String> {
    let mut ldta: Option<&[u8]> = None;
    let mut name = String::new();
    let mut root_tdgp: Option<&[Chunk<'_>]> = None;
    let mut markers = Vec::new();
    for sub in layr_children {
        match sub {
            Chunk::Leaf { tag: [b'l', b'd', b't', b'a'], data } => ldta = Some(data),
            Chunk::Leaf { tag: [b'U', b't', b'f', b'8'], data } if name.is_empty() => name = utf8_from_chunk(data),
            Chunk::List { list_type: [b't', b'd', b'g', b'p'], children } if root_tdgp.is_none() => root_tdgp = Some(children),
            Chunk::List { list_type: [b'S', b'e', b'c', b'L'], children } => markers.extend(parse_secl_markers(children, comp_fps)),
            _ => {}
        }
    }
    let Some(ldta) = ldta else { return Ok(None) };
    let layer_id = u32_be(ldta, 0).unwrap_or(1);
    let start_raw = i32_be(ldta, 12).unwrap_or(0);
    let start_ts = u32_be(ldta, 16).filter(|&ts| ts > 0).unwrap_or((comp_fps * 1024.0).round() as u32);
    let in_raw = i32_be(ldta, 20).unwrap_or(0);
    let in_ts = u32_be(ldta, 24).filter(|&ts| ts > 0).unwrap_or(start_ts);
    let out_raw = i32_be(ldta, 28).unwrap_or(0);
    let out_ts = u32_be(ldta, 32).filter(|&ts| ts > 0).unwrap_or(start_ts);
    let switches_bits = u32_be(ldta, 36).unwrap_or(1);
    let source_item_id = u32_be(ldta, 40).filter(|&id| id > 0).or_else(|| u32_be(ldta, 56)).unwrap_or(0);
    let layer_type_flags = u16_be(ldta, 60).unwrap_or(0);
    let parent_raw = u32_be(ldta, 108).unwrap_or(0);

    let start_time_seconds = f64::from(start_raw) / f64::from(start_ts.max(1));
    let in_point_seconds = (f64::from(in_raw) / f64::from(in_ts.max(1))).max(0.0);
    let raw_out = f64::from(out_raw) / f64::from(out_ts.max(1));
    let out_point_seconds = if raw_out > in_point_seconds { raw_out } else { comp_duration };

    let stretch_val = 100.0;

    let video_active = (switches_bits & 0x0001) != 0;
    let audio_active = (switches_bits & 0x0002) != 0;
    let solo = (switches_bits & 0x0004) != 0;
    let locked = (switches_bits & 0x0008) != 0;
    let shy = (switches_bits & 0x0010) != 0;
    let collapse = (switches_bits & 0x0020) != 0;
    let motion_blur = (switches_bits & 0x0080) != 0;
    let adjustment = (switches_bits & 0x0100) != 0;
    let three_d = (switches_bits & 0x1000) != 0;

    let blend_mode = ldta.get(72).copied().map(decode_blend_mode).unwrap_or(BlendMode::Normal);
    let track_matte_kind = ldta.get(73).copied().and_then(decode_track_matte);

    let mut transform = AepTransform::default();
    let mut shape_groups = Vec::new();
    let mut masks = Vec::new();
    let mut effects = Vec::new();
    let mut text: Option<AepText> = None;
    let mut camera: Option<AepCamera> = None;
    let mut light: Option<AepLight> = None;
    let mut has_vectors = false;

    if let Some(tdgp) = root_tdgp {
        let mut pending_match = String::new();
        for child in tdgp {
            match child {
                Chunk::Leaf { tag: [b't', b'd', b'm', b'n'], data } => {
                    pending_match = match_name_from_tdmn(data);
                }
                Chunk::List { list_type: [b't', b'd', b'g', b'p'], children } => {
                    match pending_match.as_str() {
                        "ADBE Transform Group" => {
                            transform = parse_transform_tdgp(children)?;
                        }
                        "ADBE Root Vectors Group" => {
                            has_vectors = true;
                            shape_groups = parse_root_vectors_tdgp(children, 0)?;
                        }
                        "ADBE Mask Parade" => {
                            masks = parse_masks_tdgp(children)?;
                        }
                        "ADBE Effect Parade" => {
                            effects = parse_effects_tdgp(children)?;
                        }
                        "ADBE Text Properties" => {
                            text = Some(parse_text_properties_tdgp(children)?);
                        }
                        "ADBE Camera Options Group" => {
                            camera = Some(parse_camera_options_tdgp(children)?);
                        }
                        "ADBE Light Options Group" => {
                            light = Some(parse_light_options_tdgp(children)?);
                        }
                        _ => {}
                    }
                    pending_match.clear();
                }
                _ => {}
            }
        }
    }

    let kind = if camera.is_some() || (layer_type_flags == 0x0004 && source_item_id == 0) {
        AepLayerKind::Camera
    } else if let Some(ref l) = light {
        AepLayerKind::Light(l.kind)
    } else if layer_type_flags == 0x0002 {
        AepLayerKind::Light(LightKind::Point)
    } else if text.is_some() {
        AepLayerKind::Text
    } else if has_vectors || layer_type_flags == 0x0008 {
        AepLayerKind::Shape
    } else if source_item_id != 0 {
        AepLayerKind::Footage(source_item_id)
    } else {
        AepLayerKind::Null
    };

    if name.is_empty() {
        if let Some(slice) = ldta.get(64..) {
            if let Some(pos) = slice.iter().position(|&b| b == 0) {
                if pos > 0 && pos < 64 {
                    if let Ok(s) = std::str::from_utf8(&slice[..pos]) {
                        let trimmed = s.trim();
                        if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                            name = trimmed.to_string();
                        }
                    }
                }
            }
        }
    }

    let final_name = if name.is_empty() { format!("Layer {layer_id}") } else { name };

    Ok(Some(AepLayer {
        id: layer_id,
        name: final_name,
        kind,
        source_id: source_item_id,
        start_time_seconds,
        in_point_seconds,
        out_point_seconds,
        stretch: stretch_val,
        video_active,
        audio_active,
        solo,
        locked,
        shy,
        collapse,
        motion_blur,
        adjustment,
        three_d,
        blend_mode,
        track_matte_kind,
        parent_id: (parent_raw != 0).then_some(parent_raw),
        transform,
        text,
        camera,
        light,
        shape_groups,
        masks,
        effects,
        markers,
    }))
}

fn parse_masks_tdgp(children: &[Chunk<'_>]) -> Result<Vec<AepMask>, String> {
    let mut masks = Vec::new();
    let mut pending_match = String::new();
    let mut mask_idx = 1;
    for child in children {
        match child {
            Chunk::Leaf { tag: [b't', b'd', b'm', b'n'], data } => {
                pending_match = match_name_from_tdmn(data);
            }
            Chunk::List { list_type: [b't', b'd', b'g', b'p'], children: mask_children } => {
                if pending_match == "ADBE Mask Atom" || pending_match.starts_with("ADBE Mask") {
                    let mut mask = AepMask {
                        name: group_custom_name(mask_children).unwrap_or_else(|| format!("Mask {mask_idx}")),
                        mode: MaskMode::Add,
                        inverted: false,
                        path: None,
                        feather: None,
                        opacity: None,
                        expansion: None,
                    };
                    mask_idx += 1;

                    let mut inner_match = String::new();
                    for m_sub in mask_children {
                        match m_sub {
                            Chunk::Leaf { tag: [b't', b'd', b'm', b'n'], data } => {
                                inner_match = match_name_from_tdmn(data);
                            }
                            Chunk::Leaf { tag: [b'm', b'o', b'd', b'e'] | [b'm', b's', b'k', b'm'], data } => {
                                if let Some(&b) = data.first() {
                                    mask.mode = match b {
                                        0 => MaskMode::None,
                                        1 => MaskMode::Add,
                                        2 => MaskMode::Subtract,
                                        3 => MaskMode::Intersect,
                                        4 => MaskMode::Lighten,
                                        5 => MaskMode::Darken,
                                        6 => MaskMode::Difference,
                                        _ => MaskMode::Add,
                                    };
                                }
                                if let Some(&inv) = data.get(1) {
                                    mask.inverted = inv != 0;
                                }
                            }
                            Chunk::List { list_type: [b't', b'd', b'b', b's'], children: tdbs } => {
                                if !inner_match.is_empty() {
                                    match inner_match.as_str() {
                                        "ADBE Mask Shape" => {
                                            let mut verts = Vec::new();
                                            let mut in_tans = Vec::new();
                                            let mut out_tans = Vec::new();
                                            let mut closed = true;
                                            for schunk in tdbs {
                                                if let Chunk::Leaf { tag: [b's', b'h', b'p', b'd'] | [b'c', b'd', b'a', b't'], data } = schunk {
                                                    let count = (data.len() / 16).min(1024);
                                                    for i in 0..count {
                                                        if let (Some(x), Some(y)) = (f64_be(data, i * 16), f64_be(data, i * 16 + 8)) {
                                                            verts.push([x, y]);
                                                            in_tans.push([0.0, 0.0]);
                                                            out_tans.push([0.0, 0.0]);
                                                        }
                                                    }
                                                    if let Some(&c) = data.get(count * 16) {
                                                        closed = c != 0;
                                                    }
                                                }
                                            }
                                            if !verts.is_empty() {
                                                mask.path = Some(AepShapePath { closed, vertices: verts, in_tangents: in_tans, out_tangents: out_tans });
                                            }
                                        }
                                        "ADBE Mask Feather" => {
                                            mask.feather = Some(parse_tdbs_stream(&inner_match, tdbs)?);
                                        }
                                        "ADBE Mask Opacity" => {
                                            mask.opacity = Some(parse_tdbs_stream(&inner_match, tdbs)?);
                                        }
                                        "ADBE Mask Offset" => {
                                            mask.expansion = Some(parse_tdbs_stream(&inner_match, tdbs)?);
                                        }
                                        _ => {}
                                    }
                                    inner_match.clear();
                                }
                            }
                            _ => {}
                        }
                    }
                    masks.push(mask);
                    pending_match.clear();
                }
            }
            _ => {}
        }
    }
    Ok(masks)
}

fn parse_effects_tdgp(children: &[Chunk<'_>]) -> Result<Vec<AepEffect>, String> {
    let mut effects = Vec::new();
    let mut pending_match = String::new();
    for child in children {
        match child {
            Chunk::Leaf { tag: [b't', b'd', b'm', b'n'], data } => {
                pending_match = match_name_from_tdmn(data);
            }
            Chunk::List { list_type: [b't', b'd', b'g', b'p'], children: fx_children } => {
                let match_name = if !pending_match.is_empty() && pending_match != "ADBE Effect Atom" {
                    pending_match.clone()
                } else {
                    fx_children.iter().find_map(|c| match c {
                        Chunk::Leaf { tag: [b't', b'd', b'm', b'n'], data } => {
                            let m = match_name_from_tdmn(data);
                            (!m.is_empty() && m != "ADBE Effect Atom").then_some(m)
                        }
                        _ => None,
                    }).unwrap_or_else(|| "ADBE Effect Unknown".into())
                };

                let name = group_custom_name(fx_children)
                    .unwrap_or_else(|| match_name.strip_prefix("ADBE ").unwrap_or(&match_name).to_string());
                let mut properties = Vec::new();
                let mut prop_match = String::new();

                for fsub in fx_children {
                    match fsub {
                        Chunk::Leaf { tag: [b't', b'd', b'm', b'n'], data } => {
                            prop_match = match_name_from_tdmn(data);
                        }
                        Chunk::List { list_type: [b't', b'd', b'b', b's'], children: tdbs } => {
                            if !prop_match.is_empty() {
                                let prop = parse_tdbs_stream(&prop_match, tdbs)?;
                                properties.push(prop);
                                prop_match.clear();
                            }
                        }
                        _ => {}
                    }
                }

                effects.push(AepEffect {
                    name,
                    match_name,
                    enabled: true,
                    properties,
                });
                pending_match.clear();
            }
            _ => {}
        }
    }
    Ok(effects)
}

fn parse_camera_options_tdgp(children: &[Chunk<'_>]) -> Result<AepCamera, String> {
    let mut cam = AepCamera::default();
    let mut pending_match = String::new();
    for child in children {
        match child {
            Chunk::Leaf { tag: [b't', b'd', b'm', b'n'], data } => {
                pending_match = match_name_from_tdmn(data);
            }
            Chunk::List { list_type: [b't', b'd', b'b', b's'], children: tdbs } => {
                if !pending_match.is_empty() {
                    let prop = parse_tdbs_stream(&pending_match, tdbs)?;
                    match pending_match.as_str() {
                        "ADBE Camera Zoom" => cam.zoom = Some(prop),
                        "ADBE Camera Focus Distance" => cam.focus_distance = Some(prop),
                        "ADBE Camera Aperture" => cam.aperture = Some(prop),
                        "ADBE Camera Blur Level" => cam.blur_level = Some(prop),
                        _ => {}
                    }
                    pending_match.clear();
                }
            }
            _ => {}
        }
    }
    Ok(cam)
}

fn parse_light_options_tdgp(children: &[Chunk<'_>]) -> Result<AepLight, String> {
    let mut intensity: Option<AepProperty> = None;
    let mut color: Option<AepProperty> = None;
    let mut cone_angle: Option<AepProperty> = None;
    let mut cone_feather: Option<AepProperty> = None;
    let mut casts_shadows: Option<AepProperty> = None;
    let mut pending_match = String::new();
    for child in children {
        match child {
            Chunk::Leaf { tag: [b't', b'd', b'm', b'n'], data } => {
                pending_match = match_name_from_tdmn(data);
            }
            Chunk::List { list_type: [b't', b'd', b'b', b's'], children: tdbs } => {
                if !pending_match.is_empty() {
                    let prop = parse_tdbs_stream(&pending_match, tdbs)?;
                    match pending_match.as_str() {
                        "ADBE Light Intensity" => intensity = Some(prop),
                        "ADBE Light Color" => color = Some(prop),
                        "ADBE Light Cone Angle" => cone_angle = Some(prop),
                        "ADBE Light Cone Feather" => cone_feather = Some(prop),
                        "ADBE Light Casts Shadows" => casts_shadows = Some(prop),
                        _ => {}
                    }
                    pending_match.clear();
                }
            }
            _ => {}
        }
    }
    let kind = if cone_angle.is_some() {
        LightKind::Spot
    } else {
        LightKind::Point
    };
    Ok(AepLight {
        kind,
        intensity,
        color,
        cone_angle,
        cone_feather,
        casts_shadows,
    })
}

fn parse_text_properties_tdgp(children: &[Chunk<'_>]) -> Result<AepText, String> {
    let mut text_doc = AepText {
        text: String::new(),
        font_family: "Arial".into(),
        font_size: 48.0,
        fill_color: [1.0, 1.0, 1.0, 1.0],
        stroke_color: [0.0, 0.0, 0.0, 1.0],
        stroke_width: 0.0,
        justification: 0,
        tracking: 0.0,
    };

    fn extract_text_from_chunks(chunks: &[Chunk<'_>], target: &mut AepText) {
        for c in chunks {
            match c {
                Chunk::Leaf { tag: [b'U', b't', b'f', b'8'] | [b't', b'd', b's', b't'], data } => {
                    let s = utf8_from_chunk(data);
                    if !s.is_empty() && s != "-_0_/-" && target.text.is_empty() {
                        target.text = s;
                    }
                }
                Chunk::Leaf { tag: [b'f', b'o', b'n', b't'], data } => {
                    let s = utf8_from_chunk(data);
                    if !s.is_empty() {
                        target.font_family = s;
                    }
                }
                Chunk::List { children: sub, .. } => {
                    extract_text_from_chunks(sub, target);
                }
                _ => {}
            }
        }
    }

    extract_text_from_chunks(children, &mut text_doc);
    Ok(text_doc)
}

fn group_custom_name(children: &[Chunk<'_>]) -> Option<String> {
    for c in children {
        if let Chunk::Leaf { tag: [b't', b'd', b's', b'n'], data } = c {
            let s = utf8_from_chunk(data);
            if !s.is_empty() && s != "-_0_/-" {
                return Some(s);
            }
        }
    }
    None
}

fn parse_transform_tdgp(children: &[Chunk<'_>]) -> Result<AepTransform, String> {
    let mut tr = AepTransform::default();
    let mut pending_match = String::new();
    for child in children {
        match child {
            Chunk::Leaf { tag: [b't', b'd', b'm', b'n'], data } => {
                pending_match = match_name_from_tdmn(data);
            }
            Chunk::List { list_type: [b't', b'd', b'b', b's'], children: tdbs } => {
                if !pending_match.is_empty() {
                    let prop = parse_tdbs_stream(&pending_match, tdbs)?;
                    match pending_match.as_str() {
                        "ADBE Anchor Point" | "ADBE Vector Anchor" => tr.anchor_point = Some(prop),
                        "ADBE Position" | "ADBE Vector Position" => tr.position = Some(prop),
                        "ADBE Point of Interest" => tr.poi = Some(prop),
                        "ADBE Scale" | "ADBE Vector Scale" => tr.scale = Some(prop),
                        "ADBE Orientation" => tr.orientation = Some(prop),
                        "ADBE Rotate X" => tr.rotate_x = Some(prop),
                        "ADBE Rotate Y" => tr.rotate_y = Some(prop),
                        "ADBE Rotate Z" | "ADBE Vector Rotation" => tr.rotate_z = Some(prop),
                        "ADBE Opacity" | "ADBE Vector Group Opacity" => tr.opacity = Some(prop),
                        _ => {}
                    }
                    pending_match.clear();
                }
            }
            Chunk::List { list_type: [b'o', b't', b's', b't'], children: otst } => {
                if pending_match == "ADBE Orientation" {
                    for sub in otst {
                        if let Chunk::List { list_type: [b't', b'd', b'b', b's'], children: tdbs } = sub {
                            tr.orientation = Some(parse_tdbs_stream(&pending_match, tdbs)?);
                            break;
                        }
                    }
                    pending_match.clear();
                }
            }
            _ => {}
        }
    }
    Ok(tr)
}

fn parse_root_vectors_tdgp(children: &[Chunk<'_>], depth: usize) -> Result<Vec<AepShapeGroup>, String> {
    if depth > MAX_DEPTH {
        return Err(format!("AEP vector group nested deeper than {MAX_DEPTH} levels"));
    }
    let mut groups = Vec::new();
    let mut pending_match = String::new();
    for child in children {
        match child {
            Chunk::Leaf { tag: [b't', b'd', b'm', b'n'], data } => {
                pending_match = match_name_from_tdmn(data);
            }
            Chunk::List { list_type: [b't', b'd', b'g', b'p'], children: sub_children } => {
                if pending_match == "ADBE Vector Group" {
                    groups.push(parse_vector_group_tdgp(sub_children, groups.len() + 1, depth + 1)?);
                }
                pending_match.clear();
            }
            _ => {}
        }
    }
    Ok(groups)
}

fn parse_vector_group_tdgp(children: &[Chunk<'_>], index: usize, depth: usize) -> Result<AepShapeGroup, String> {
    if depth > MAX_DEPTH {
        return Err(format!("AEP vector group nested deeper than {MAX_DEPTH} levels"));
    }
    let mut group = AepShapeGroup {
        name: group_custom_name(children).unwrap_or_else(|| format!("Group {index}")),
        ..Default::default()
    };
    let mut pending_match = String::new();
    for child in children {
        match child {
            Chunk::Leaf { tag: [b't', b'd', b'm', b'n'], data } => {
                pending_match = match_name_from_tdmn(data);
            }
            Chunk::List { list_type: [b't', b'd', b'g', b'p'], children: sub } => {
                match pending_match.as_str() {
                    "ADBE Vectors Group" => parse_vectors_items_tdgp(sub, &mut group, depth + 1)?,
                    "ADBE Vector Transform Group" => group.transform = parse_transform_tdgp(sub)?,
                    _ => {}
                }
                pending_match.clear();
            }
            _ => {}
        }
    }
    Ok(group)
}

fn parse_vectors_items_tdgp(children: &[Chunk<'_>], group: &mut AepShapeGroup, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!("AEP vector items nested deeper than {MAX_DEPTH} levels"));
    }
    let mut pending_match = String::new();
    for child in children {
        match child {
            Chunk::Leaf { tag: [b't', b'd', b'm', b'n'], data } => {
                pending_match = match_name_from_tdmn(data);
            }
            Chunk::List { list_type: [b't', b'd', b'g', b'p'], children: item_children } => {
                match pending_match.as_str() {
                    "ADBE Vector Shape - Rect" => parse_vector_leaf_props(item_children, group)?,
                    "ADBE Vector Shape - Ellipse" => parse_vector_leaf_props(item_children, group)?,
                    "ADBE Vector Shape - Star" => parse_vector_leaf_props(item_children, group)?,
                    "ADBE Vector Shape - Group" => parse_vector_leaf_props(item_children, group)?,
                    "ADBE Vector Graphic - Stroke" => parse_vector_leaf_props(item_children, group)?,
                    "ADBE Vector Graphic - Fill" => parse_vector_leaf_props(item_children, group)?,
                    "ADBE Vector Graphic - G-Fill" => parse_vector_leaf_props(item_children, group)?,
                    "ADBE Vector Graphic - G-Stroke" => parse_vector_leaf_props(item_children, group)?,
                    "ADBE Vector Filter - Trim" => parse_vector_leaf_props(item_children, group)?,
                    "ADBE Vector Filter - Repeater" => parse_vector_leaf_props(item_children, group)?,
                    _ => {}
                }
                pending_match.clear();
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_vector_leaf_props(children: &[Chunk<'_>], group: &mut AepShapeGroup) -> Result<(), String> {
    let mut pending_match = String::new();
    for child in children {
        match child {
            Chunk::Leaf { tag: [b't', b'd', b'm', b'n'], data } => {
                pending_match = match_name_from_tdmn(data);
            }
            Chunk::List { list_type: [b't', b'd', b'b', b's'], children: tdbs } => {
                if !pending_match.is_empty() {
                    let prop = parse_tdbs_stream(&pending_match, tdbs)?;
                    match pending_match.as_str() {
                        "ADBE Vector Rect Size" => group.rect_size = Some(prop),
                        "ADBE Vector Rect Position" => group.rect_position = Some(prop),
                        "ADBE Vector Rect Roundness" => group.rect_roundness = Some(prop),
                        "ADBE Vector Ellipse Size" => group.ellipse_size = Some(prop),
                        "ADBE Vector Ellipse Position" => group.ellipse_position = Some(prop),
                        "ADBE Vector Star Type" => group.star_type = Some(prop),
                        "ADBE Vector Star Points" => group.star_points = Some(prop),
                        "ADBE Vector Star Position" => group.star_position = Some(prop),
                        "ADBE Vector Star Rotation" => group.star_rotation = Some(prop),
                        "ADBE Vector Star Inner Radius" => group.star_inner_radius = Some(prop),
                        "ADBE Vector Star Outer Radius" => group.star_outer_radius = Some(prop),
                        "ADBE Vector Star Inner Roundness" => group.star_inner_roundness = Some(prop),
                        "ADBE Vector Star Outer Roundness" => group.star_outer_roundness = Some(prop),
                        "ADBE Vector Stroke Color" => group.stroke_color = Some(prop),
                        "ADBE Vector Stroke Width" => group.stroke_width = Some(prop),
                        "ADBE Vector Stroke Opacity" => group.stroke_opacity = Some(prop),
                        "ADBE Vector Fill Color" => group.fill_color = Some(prop),
                        "ADBE Vector Fill Opacity" => group.fill_opacity = Some(prop),
                        "ADBE Vector Grad Type" => {
                            if group.gfill_type.is_none() {
                                group.gfill_type = Some(prop);
                            } else {
                                group.gstroke_type = Some(prop);
                            }
                        }
                        "ADBE Vector Grad Start Pt" => {
                            if group.gfill_start.is_none() {
                                group.gfill_start = Some(prop);
                            } else {
                                group.gstroke_start = Some(prop);
                            }
                        }
                        "ADBE Vector Grad End Pt" => {
                            if group.gfill_end.is_none() {
                                group.gfill_end = Some(prop);
                            } else {
                                group.gstroke_end = Some(prop);
                            }
                        }
                        "ADBE Vector Grad Colors" => {
                            if group.gfill_colors.is_none() {
                                group.gfill_colors = Some(prop);
                            } else {
                                group.gstroke_colors = Some(prop);
                            }
                        }
                        "ADBE Vector Grad Opacity" => {
                            if group.gfill_opacity.is_none() {
                                group.gfill_opacity = Some(prop);
                            } else {
                                group.gstroke_opacity = Some(prop);
                            }
                        }
                        "ADBE Vector Trim Start" => group.trim_start = Some(prop),
                        "ADBE Vector Trim End" => group.trim_end = Some(prop),
                        "ADBE Vector Trim Offset" => group.trim_offset = Some(prop),
                        "ADBE Vector Repeater Copies" => group.repeater_copies = Some(prop),
                        "ADBE Vector Repeater Offset" => group.repeater_offset = Some(prop),
                        _ => {}
                    }
                    pending_match.clear();
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_tdbs_stream(match_name: &str, tdbs: &[Chunk<'_>]) -> Result<AepProperty, String> {
    let name = group_custom_name(tdbs).unwrap_or_else(|| match_name.to_string());
    let mut dims: u16 = 1;
    let mut spatial = false;
    let mut unit_ratio = false;
    let mut time_scale: u32 = 30720;
    let mut static_value = Vec::new();
    let mut keyframes = Vec::new();
    let mut expression: Option<String> = None;

    for chunk in tdbs {
        match chunk {
            Chunk::Leaf { tag: [b't', b'd', b'b', b'4'], data } => {
                dims = u16_be(data, 2).filter(|&d| (1..=4).contains(&d)).unwrap_or(1);
                let flags = u16_be(data, 4).unwrap_or(0);
                spatial = (flags & 0x0002) != 0;
                let unit_flag = u16_be(data, 8).unwrap_or(0);
                unit_ratio = unit_flag == 0x0001 && matches!(match_name, "ADBE Scale" | "ADBE Opacity");
                if let Some(ts) = u32_be(data, 12).filter(|&ts| ts > 0) {
                    time_scale = ts;
                }
            }
            Chunk::Leaf { tag: [b'e', b'x', b'p', b'r'] | [b't', b'd', b'e', b'x'] | [b'E', b'X', b'P', b'R'], data } => {
                let s = utf8_from_chunk(data);
                if !s.is_empty() {
                    expression = Some(s);
                }
            }
            Chunk::Leaf { tag: [b'c', b'd', b'a', b't'], data } => {
                let count = usize::from(dims);
                for d in 0..count {
                    if let Some(v) = f64_be(data, d.saturating_mul(8)) {
                        static_value.push(v);
                    }
                }
            }
            Chunk::List { list_type: [b'l', b'i', b's', b't'], children } => {
                keyframes = parse_keyframe_list(children, dims, spatial, time_scale)?;
            }
            _ => {}
        }
    }

    if static_value.is_empty()
        && let Some(first) = keyframes.first()
    {
        static_value = first.value.clone();
    }

    Ok(AepProperty { match_name: match_name.to_string(), name, dims, spatial, unit_ratio, static_value, keyframes, expression })
}

fn decode_interp(byte: u8) -> Interp {
    match byte {
        2 => Interp::Bezier,
        3 => Interp::Hold,
        _ => Interp::Linear,
    }
}

fn parse_keyframe_list(children: &[Chunk<'_>], dims: u16, spatial: bool, time_scale: u32) -> Result<Vec<AepKeyframe>, String> {
    let mut lhd3: Option<&[u8]> = None;
    let mut ldat: Option<&[u8]> = None;
    for c in children {
        match c {
            Chunk::Leaf { tag: [b'l', b'h', b'd', b'3'], data } => lhd3 = Some(data),
            Chunk::Leaf { tag: [b'l', b'd', b'a', b't'], data } => ldat = Some(data),
            _ => {}
        }
    }
    let (Some(lhd3), Some(ldat)) = (lhd3, ldat) else { return Ok(Vec::new()) };
    let key_count = (u32_be(lhd3, 8).unwrap_or(0) as usize).min(MAX_KEYS);
    let key_stride = u32_be(lhd3, 16).unwrap_or(0) as usize;
    if key_count == 0 || key_stride < 16 {
        return Ok(Vec::new());
    }
    let ts = f64::from(time_scale.max(1));
    let mut out = Vec::with_capacity(key_count.min(1024));
    let dim_count = usize::from(dims.clamp(1, 4));

    for k in 0..key_count {
        let Some(start) = k.checked_mul(key_stride) else { break };
        let Some(end) = start.checked_add(key_stride) else { break };
        let Some(rec) = ldat.get(start..end) else { break };

        let t_raw = i32_be(rec, 0).unwrap_or(0);
        let time_seconds = f64::from(t_raw) / ts;
        let in_interp = decode_interp(rec.get(4).copied().unwrap_or(1));
        let out_interp = decode_interp(rec.get(5).copied().unwrap_or(1));

        if spatial && key_stride >= 128 {
            // Spatial keyframe layout (128 bytes, observed at `@6864` in `test_simple.aep`):
            // 16..24: in_speed, 32..40: in_influence, 40..48: out_speed, 48..56: out_influence
            // 56..80: [x, y, z], 80..104: [ix, iy, iz], 104..128: [ox, oy, oz]
            let in_speed = f64_be(rec, 16).filter(|v| v.is_finite()).unwrap_or(0.0);
            let in_inf = f64_be(rec, 32).filter(|v| v.is_finite()).unwrap_or(1.0 / 6.0).clamp(0.0, 1.0);
            let out_speed = f64_be(rec, 40).filter(|v| v.is_finite()).unwrap_or(0.0);
            let out_inf = f64_be(rec, 48).filter(|v| v.is_finite()).unwrap_or(1.0 / 6.0).clamp(0.0, 1.0);
            let vx = f64_be(rec, 56).unwrap_or(0.0);
            let vy = f64_be(rec, 64).unwrap_or(0.0);
            let vz = f64_be(rec, 72).unwrap_or(0.0);
            let ix = f64_be(rec, 80).filter(|v| v.is_finite()).unwrap_or(0.0);
            let iy = f64_be(rec, 88).filter(|v| v.is_finite()).unwrap_or(0.0);
            let iz = f64_be(rec, 96).filter(|v| v.is_finite()).unwrap_or(0.0);
            let ox = f64_be(rec, 104).filter(|v| v.is_finite()).unwrap_or(0.0);
            let oy = f64_be(rec, 112).filter(|v| v.is_finite()).unwrap_or(0.0);
            let oz = f64_be(rec, 120).filter(|v| v.is_finite()).unwrap_or(0.0);
            let value = if dim_count == 2 { vec![vx, vy] } else { vec![vx, vy, vz] };
            out.push(AepKeyframe {
                time_seconds,
                value,
                in_interp,
                out_interp,
                in_ease: vec![Ease { speed: in_speed, influence: in_inf }],
                out_ease: vec![Ease { speed: out_speed, influence: out_inf }],
                spatial_in: [ix, iy, iz],
                spatial_out: [ox, oy, oz],
            });
        } else {
            // Non-spatial keyframe layout (observed at `@8294` for 3D Scale stride=128 and `@9878` for 1D Rotate Z stride=48):
            // 8 .. 8 + dim*8: values
            // followed by in_speed[dim], in_influence[dim], out_speed[dim], out_influence[dim].
            let mut value = Vec::with_capacity(dim_count);
            for d in 0..dim_count {
                let v = f64_be(rec, 8 + d * 8).unwrap_or(0.0);
                value.push(v);
            }
            let ease_base = 8 + dim_count * 8;
            let mut in_ease = Vec::with_capacity(dim_count);
            let mut out_ease = Vec::with_capacity(dim_count);
            for d in 0..dim_count {
                let in_speed = f64_be(rec, ease_base + d * 8).filter(|v| v.is_finite()).unwrap_or(0.0);
                let in_inf = f64_be(rec, ease_base + (dim_count + d) * 8)
                    .filter(|v| v.is_finite())
                    .unwrap_or(1.0 / 6.0)
                    .clamp(0.0, 1.0);
                let out_speed = f64_be(rec, ease_base + (2 * dim_count + d) * 8).filter(|v| v.is_finite()).unwrap_or(0.0);
                let out_inf = f64_be(rec, ease_base + (3 * dim_count + d) * 8)
                    .filter(|v| v.is_finite())
                    .unwrap_or(1.0 / 6.0)
                    .clamp(0.0, 1.0);
                in_ease.push(Ease { speed: in_speed, influence: in_inf });
                out_ease.push(Ease { speed: out_speed, influence: out_inf });
            }
            out.push(AepKeyframe {
                time_seconds,
                value,
                in_interp,
                out_interp,
                in_ease,
                out_ease,
                spatial_in: [0.0; 3],
                spatial_out: [0.0; 3],
            });
        }
    }
    Ok(out)
}

// ----------------------------------------------------------------------------
// Step 3: Mapping AepProject -> EffectCraft Internal Project / Comp / Layer / Keyframe
// ----------------------------------------------------------------------------

fn seconds_to_tick(seconds: f64, rate: FrameRate) -> Tick {
    if !seconds.is_finite() {
        return Tick::ZERO;
    }
    let raw = Tick::from_seconds_f64(seconds);
    // Snap to exact frame boundary when within 1 millisecond of a frame tick.
    let snapped = rate.snap_nearest(raw);
    if (snapped.0 - raw.0).abs() <= (effectcraft_time::TICKS_PER_SECOND / 1000) {
        snapped
    } else {
        raw
    }
}

/// Convert a parsed [`AepProject`] into an EffectCraft [`Project`].
pub fn aep_to_project(aep: &AepProject) -> Project {
    let mut proj = Project::default();
    let mut folder_map: BTreeMap<u32, ItemId> = BTreeMap::new();
    let mut solid_map: BTreeMap<u32, (ItemId, u32, u32)> = BTreeMap::new();
    let mut footage_map: BTreeMap<u32, (ItemId, u32, u32, Tick)> = BTreeMap::new();
    let mut comp_map: BTreeMap<u32, (ItemId, u32, u32, Tick)> = BTreeMap::new();

    for f in &aep.folders {
        let parent = f.parent_id.and_then(|pid| folder_map.get(&pid).copied());
        let id = proj.add_item(&f.name, Label::Yellow, parent, ItemKind::Folder);
        folder_map.insert(f.id, id);
    }

    for s in &aep.solids {
        let parent = s.parent_id.and_then(|pid| folder_map.get(&pid).copied());
        let w = effectcraft_project::limits::size(u64::from(s.width));
        let h = effectcraft_project::limits::size(u64::from(s.height));
        let id = proj.add_item(&s.name, Label::Red, parent, ItemKind::Solid(Solid { width: w, height: h, pixel_aspect: 1.0, color: s.color }));
        solid_map.insert(s.id, (id, w, h));
    }

    for f in &aep.footages {
        let parent = f.parent_id.and_then(|pid| folder_map.get(&pid).copied());
        let w = effectcraft_project::limits::size(u64::from(f.width));
        let h = effectcraft_project::limits::size(u64::from(f.height));
        let rate = FrameRate::from_f64(f.fps.clamp(1.0, 999.0));
        let duration = seconds_to_tick(f.duration_seconds, rate);
        let footage = Footage {
            path: f.path.clone(),
            kind: if !f.has_video && f.has_audio {
                FootageKind::Audio
            } else if f.duration_seconds <= 0.0 {
                FootageKind::Still
            } else {
                FootageKind::Video
            },
            width: w,
            height: h,
            pixel_aspect: 1.0,
            frame_rate: rate,
            native_rate: None,
            duration,
            has_video: f.has_video,
            has_audio: f.has_audio,
            alpha: AlphaMode::Straight,
            fields: FieldOrder::Off,
            start_timecode: None,
            ..Footage::default()
        };
        let id = proj.add_item(&f.name, Label::Lavender, parent, ItemKind::Footage(footage));
        footage_map.insert(f.id, (id, w, h, duration));
    }

    // First pass: register compositions so nested precomp references resolve.
    for ac in &aep.comps {
        let parent = ac.parent_id.and_then(|pid| folder_map.get(&pid).copied());
        let w = effectcraft_project::limits::size(u64::from(ac.width));
        let h = effectcraft_project::limits::size(u64::from(ac.height));
        let rate = FrameRate::from_f64(ac.fps.clamp(1.0, 999.0));
        let duration = seconds_to_tick(ac.duration_seconds, rate).max(rate.frame_duration());
        let mut comp = Comp::new(w, h, rate, duration);
        comp.pixel_aspect = if ac.pixel_aspect.is_finite() && ac.pixel_aspect > 0.0 { ac.pixel_aspect } else { 1.0 };
        comp.background = ac.background;
        comp.shutter_angle = ac.shutter_angle.clamp(0.0, 720.0);
        for m in &ac.markers {
            let time = seconds_to_tick(m.time_seconds, rate);
            let duration = seconds_to_tick(m.duration_seconds, rate);
            comp.markers.push(Marker {
                time,
                duration,
                comment: m.comment.clone(),
                ..Marker::default()
            });
        }
        let cid = proj.add_item(&ac.name, Label::Sandstone, parent, ItemKind::Comp(Arc::new(comp)));
        comp_map.insert(ac.id, (cid, w, h, duration));
    }

    // Second pass: populate layers, transforms, keyframes and vector shape groups.
    for ac in &aep.comps {
        let Some(&(cid, comp_w, comp_h, _)) = comp_map.get(&ac.id) else { continue };
        let Some(mut comp) = proj.comp(cid).cloned() else { continue };
        let rate = comp.frame_rate;

        let mut layer_id_map: BTreeMap<u32, LayerId> = BTreeMap::new();
        let mut pending_parents: Vec<(usize, u32)> = Vec::new();
        let mut pending_mattes: Vec<(usize, MatteKind)> = Vec::new();

        for al in &ac.layers {
            let (source, size, dur) = match al.kind {
                AepLayerKind::Shape => (LayerSource::Shape, (comp_w, comp_h), None),
                AepLayerKind::Text => (LayerSource::Text, (comp_w, comp_h), None),
                AepLayerKind::Null => (LayerSource::Null, (100, 100), None),
                AepLayerKind::Camera => (LayerSource::Camera, (comp_w, comp_h), None),
                AepLayerKind::Light(kind) => (LayerSource::Light { kind }, (comp_w, comp_h), None),
                AepLayerKind::Solid(sid) | AepLayerKind::Comp(sid) | AepLayerKind::Footage(sid) => {
                    let target_id = if al.source_id != 0 { al.source_id } else { sid };
                    if let Some(&(item, cw, ch, cdur)) = comp_map.get(&target_id) {
                        (LayerSource::Comp { item }, (cw, ch), Some(cdur))
                    } else if let Some(&(item, fw, fh, fdur)) = footage_map.get(&target_id) {
                        (LayerSource::Footage { item }, (fw, fh), Some(fdur))
                    } else if let Some(&(item, sw, sh)) = solid_map.get(&target_id) {
                        (LayerSource::Solid { item }, (sw, sh), None)
                    } else {
                        (LayerSource::Null, (100, 100), None)
                    }
                }
            };

            let mut layer_name = al.name.clone();
            if (layer_name.is_empty() || layer_name.starts_with("Layer "))
                && let Some(src_item) = source.item().and_then(|item_id| proj.item(item_id))
            {
                layer_name = src_item.name.clone();
            }

            let mut layer = build::layer(&mut proj, &comp, &layer_name, source, size, dur);
            layer.start_time = seconds_to_tick(al.start_time_seconds, rate);
            layer.in_point = seconds_to_tick(al.in_point_seconds, rate);
            layer.out_point = seconds_to_tick(al.out_point_seconds, rate).max(layer.in_point);
            layer.stretch = if al.stretch.abs() > 1e-4 { al.stretch } else { 100.0 };

            layer.switches.video = al.video_active;
            layer.switches.audio = al.audio_active;
            layer.switches.solo = al.solo;
            layer.switches.locked = al.locked;
            layer.switches.shy = al.shy;
            layer.switches.collapse = al.collapse;
            layer.switches.motion_blur = al.motion_blur;
            layer.switches.adjustment = al.adjustment;
            layer.switches.three_d = al.three_d;
            layer.blend_mode = al.blend_mode;

            apply_layer_transform(&mut layer, &al.transform, comp_w, comp_h, rate);

            if let Some(ref cam) = al.camera {
                apply_camera_options(&mut layer, cam, rate);
            }
            if let Some(ref light) = al.light {
                apply_light_options(&mut layer, light, rate);
            }
            if let Some(ref aep_text) = al.text {
                let mut doc = TextDoc::default();
                doc.text = aep_text.text.clone();
                if !aep_text.font_family.is_empty() {
                    doc.font = aep_text.font_family.clone();
                }
                if aep_text.font_size > 0.0 {
                    doc.size = aep_text.font_size;
                }
                doc.fill = aep_text.fill_color;
                doc.stroke = aep_text.stroke_color;
                doc.stroke_width = aep_text.stroke_width;
                doc.tracking = aep_text.tracking;
                doc.justify = match aep_text.justification {
                    1 => Justify::Right,
                    2 => Justify::Center,
                    _ => Justify::Left,
                };
                if let Some(text_grp) = layer.props.sub_mut("text") {
                    if let Some(prop) = text_grp.get_mut("sourceText") {
                        prop.value = Value::Text(Box::new(doc));
                    }
                }
            }

            if matches!(layer.source, LayerSource::Shape) && !al.shape_groups.is_empty() {
                let mut ids = Ids(&mut proj.next_id);
                let built_groups: Vec<PropGroup> = al.shape_groups.iter().map(|sg| build_shape_group(&mut ids, sg, rate)).collect();
                if let Some(contents) = layer.props.sub_mut("contents") {
                    for g in built_groups {
                        contents.children.push(Node::Group(g));
                    }
                }
            }

            for m in &al.markers {
                let time = seconds_to_tick(m.time_seconds, rate);
                let duration = seconds_to_tick(m.duration_seconds, rate);
                layer.markers.push(Marker {
                    time,
                    duration,
                    comment: m.comment.clone(),
                    ..Marker::default()
                });
            }

            if !al.masks.is_empty() {
                let mut ids = Ids(&mut proj.next_id);
                for (m_idx, m) in al.masks.iter().enumerate() {
                    let path = m.path.as_ref().map(|p| ShapePath {
                        vertices: p.vertices.clone(),
                        in_tangents: p.in_tangents.clone(),
                        out_tangents: p.out_tangents.clone(),
                        closed: p.closed,
                        feather: Vec::new(),
                    }).unwrap_or_else(|| ShapePath {
                        vertices: Vec::new(),
                        in_tangents: Vec::new(),
                        out_tangents: Vec::new(),
                        closed: true,
                        feather: Vec::new(),
                    });
                    let color = build::MASK_COLORS.get(m_idx % build::MASK_COLORS.len()).copied().unwrap_or([255, 255, 0]);
                    let mut mg = build::mask(&mut ids, &m.name, path, m.mode, color);
                    if let GroupKind::Mask { inverted, .. } = &mut mg.kind {
                        *inverted = m.inverted;
                    }
                    if let (Some(src), Some(dst)) = (&m.feather, mg.get_mut("feather")) {
                        apply_prop(dst, src, rate, &|v| Value::Vec2([v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0)]));
                    }
                    if let (Some(src), Some(dst)) = (&m.opacity, mg.get_mut("opacity")) {
                        apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(100.0).clamp(0.0, 100.0)));
                    }
                    if let (Some(src), Some(dst)) = (&m.expansion, mg.get_mut("expansion")) {
                        apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(0.0)));
                    }
                    if layer.props.sub("masks").is_none() {
                        let root_masks = build::masks(&mut ids);
                        layer.props.children.push(Node::Group(root_masks));
                    }
                    if let Some(masks_grp) = layer.props.sub_mut("masks") {
                        masks_grp.children.push(Node::Group(mg));
                    }
                }
            }

            if !al.effects.is_empty() {
                let (lw, lh) = (comp.width as f64, comp.height as f64);
                let mut ids = Ids(&mut proj.next_id);
                for fx in &al.effects {
                    let effect_id = ae_match_to_effect_id(&fx.match_name).unwrap_or(&fx.match_name);
                    let effect_group = if let Some(spec) = effectcraft_effects::find(effect_id) {
                        let mut eg = effectcraft_effects::instantiate(spec, &mut ids, &fx.name, [lw, lh]);
                        eg.enabled = fx.enabled;
                        for p in &fx.properties {
                            let leaf = p.match_name.rsplit_once('/').map(|(_, l)| l).unwrap_or(&p.match_name);
                            let child_idx = eg.children.iter().position(|c| match c {
                                Node::Prop(prop) => {
                                    prop.match_id == p.match_name
                                        || prop.match_id == leaf
                                        || prop.name.eq_ignore_ascii_case(&p.name)
                                        || prop.match_id.eq_ignore_ascii_case(&p.name)
                                        || prop.name.eq_ignore_ascii_case(&p.match_name)
                                        || prop.match_id.eq_ignore_ascii_case(leaf)
                                }
                                _ => false,
                            });
                            if let Some(idx) = child_idx {
                                if let Some(Node::Prop(dst)) = eg.children.get_mut(idx) {
                                    apply_dynamic_prop(dst, p, rate);
                                }
                            }
                        }
                        eg
                    } else {
                        let mut eg = ids.group(&fx.match_name, &fx.name);
                        eg.kind = GroupKind::Effect { effect: fx.match_name.clone() };
                        eg.enabled = fx.enabled;
                        for p in &fx.properties {
                            let mut prop = Property::new(ids.alloc(), &p.match_name, &p.name, Value::Scalar(p.static_value.first().copied().unwrap_or(0.0)));
                            apply_dynamic_prop(&mut prop, p, rate);
                            eg.children.push(Node::Prop(prop));
                        }
                        eg
                    };

                    if layer.props.sub("effects").is_none() {
                        let root_fx = build::effects(&mut ids);
                        layer.props.children.push(Node::Group(root_fx));
                    }
                    if let Some(effects_grp) = layer.props.sub_mut("effects") {
                        effects_grp.children.push(Node::Group(effect_group));
                    }
                }
            }

            let idx = comp.layers.len();
            layer_id_map.insert(al.id, layer.id);
            if let Some(pid) = al.parent_id {
                pending_parents.push((idx, pid));
            }
            if let Some(mk) = al.track_matte_kind {
                pending_mattes.push((idx, mk));
            }
            comp.layers.push(layer);
        }

        for (idx, aep_parent) in pending_parents {
            if let (Some(&parent_lid), Some(layer)) = (layer_id_map.get(&aep_parent), comp.layers.get_mut(idx))
                && layer.id != parent_lid
            {
                layer.parent = Some(parent_lid);
            }
        }

        for (idx, mk) in pending_mattes {
            // In After Effects, the track matte source is the layer directly above (i.e. idx - 1).
            if idx > 0 {
                let prev_lid = comp.layers.get(idx - 1).map(|l| l.id);
                if let (Some(matte_lid), Some(layer)) = (prev_lid, comp.layers.get_mut(idx)) {
                    layer.track_matte = Some(TrackMatte { layer: matte_lid, kind: mk });
                }
            }
        }

        if let Some(item) = proj.item_mut(cid) {
            item.kind = ItemKind::Comp(Arc::new(comp));
        }
    }

    proj.fix_next_id();
    proj
}

fn apply_layer_transform(layer: &mut Layer, tr: &AepTransform, comp_w: u32, comp_h: u32, rate: FrameRate) {
    let Some(tr_group) = layer.props.sub_mut("transform") else { return };

    if let (Some(src), Some(dst)) = (&tr.anchor_point, tr_group.get_mut("anchor")) {
        apply_prop(dst, src, rate, &|vals| {
            let raw = [vals.first().copied().unwrap_or(0.0), vals.get(1).copied().unwrap_or(0.0), vals.get(2).copied().unwrap_or(0.0)];
            Value::Vec3(aep_anchor_to_ec(raw))
        });
    }
    if let (Some(src), Some(dst)) = (&tr.position, tr_group.get_mut("position")) {
        apply_prop(dst, src, rate, &|vals| {
            let raw = [
                vals.first().copied().unwrap_or(f64::from(comp_w) * 0.5),
                vals.get(1).copied().unwrap_or(f64::from(comp_h) * 0.5),
                vals.get(2).copied().unwrap_or(0.0),
            ];
            Value::Vec3(aep_position_to_ec(raw, comp_w, comp_h))
        });
    }
    if let (Some(src), Some(dst)) = (&tr.poi, tr_group.get_mut("poi")) {
        apply_prop(dst, src, rate, &|vals| {
            let raw = [
                vals.first().copied().unwrap_or(f64::from(comp_w) * 0.5),
                vals.get(1).copied().unwrap_or(f64::from(comp_h) * 0.5),
                vals.get(2).copied().unwrap_or(0.0),
            ];
            Value::Vec3(aep_position_to_ec(raw, comp_w, comp_h))
        });
    }
    if let (Some(src), Some(dst)) = (&tr.scale, tr_group.get_mut("scale")) {
        let unit_ratio = src.unit_ratio;
        apply_prop(dst, src, rate, &|vals| {
            let def = if unit_ratio { 1.0 } else { 100.0 };
            let sx = vals.first().copied().unwrap_or(def);
            let sy = vals.get(1).copied().unwrap_or(sx);
            let sz = vals.get(2).copied().unwrap_or(sy);
            Value::Vec3(aep_scale_to_ec([sx, sy, sz], unit_ratio))
        });
    }
    if let (Some(src), Some(dst)) = (&tr.orientation, tr_group.get_mut("orientation")) {
        apply_prop(dst, src, rate, &|vals| {
            Value::Vec3([
                aep_rotation_to_ec(vals.first().copied().unwrap_or(0.0)),
                aep_rotation_to_ec(vals.get(1).copied().unwrap_or(0.0)),
                aep_rotation_to_ec(vals.get(2).copied().unwrap_or(0.0)),
            ])
        });
    }
    if let (Some(src), Some(dst)) = (&tr.rotate_x, tr_group.get_mut("rotationX")) {
        apply_prop(dst, src, rate, &|vals| Value::Scalar(aep_rotation_to_ec(vals.first().copied().unwrap_or(0.0))));
    }
    if let (Some(src), Some(dst)) = (&tr.rotate_y, tr_group.get_mut("rotationY")) {
        apply_prop(dst, src, rate, &|vals| Value::Scalar(aep_rotation_to_ec(vals.first().copied().unwrap_or(0.0))));
    }
    if let (Some(src), Some(dst)) = (&tr.rotate_z, tr_group.get_mut("rotation")) {
        apply_prop(dst, src, rate, &|vals| Value::Scalar(aep_rotation_to_ec(vals.first().copied().unwrap_or(0.0))));
    }
    if let (Some(src), Some(dst)) = (&tr.opacity, tr_group.get_mut("opacity")) {
        let unit_ratio = src.unit_ratio;
        apply_prop(dst, src, rate, &|vals| {
            let v = vals.first().copied().unwrap_or(if unit_ratio { 1.0 } else { 100.0 });
            let pct = if unit_ratio { v * 100.0 } else { v };
            Value::Scalar(pct.clamp(0.0, 100.0))
        });
    }
}

fn apply_camera_options(layer: &mut Layer, cam: &AepCamera, rate: FrameRate) {
    let Some(cam_opts) = layer.props.sub_mut("cameraOptions") else { return };
    if let (Some(src), Some(dst)) = (&cam.zoom, cam_opts.get_mut("zoom")) {
        apply_prop(dst, src, rate, &|vals| Value::Scalar(vals.first().copied().unwrap_or(0.0)));
    }
    if let (Some(src), Some(dst)) = (&cam.focus_distance, cam_opts.get_mut("focusDistance")) {
        apply_prop(dst, src, rate, &|vals| Value::Scalar(vals.first().copied().unwrap_or(0.0)));
    }
    if let (Some(src), Some(dst)) = (&cam.aperture, cam_opts.get_mut("aperture")) {
        apply_prop(dst, src, rate, &|vals| Value::Scalar(vals.first().copied().unwrap_or(0.0)));
    }
    if let (Some(src), Some(dst)) = (&cam.blur_level, cam_opts.get_mut("blurLevel")) {
        apply_prop(dst, src, rate, &|vals| Value::Scalar(vals.first().copied().unwrap_or(100.0)));
    }
}

fn apply_light_options(layer: &mut Layer, light: &AepLight, rate: FrameRate) {
    let Some(light_opts) = layer.props.sub_mut("lightOptions") else { return };
    if let (Some(src), Some(dst)) = (&light.intensity, light_opts.get_mut("intensity")) {
        apply_prop(dst, src, rate, &|vals| Value::Scalar(vals.first().copied().unwrap_or(100.0)));
    }
    if let (Some(src), Some(dst)) = (&light.color, light_opts.get_mut("color")) {
        apply_prop(dst, src, rate, &|vals| {
            let r = vals.first().copied().unwrap_or(1.0);
            let g = vals.get(1).copied().unwrap_or(1.0);
            let b = vals.get(2).copied().unwrap_or(1.0);
            Value::Color([r, g, b, 1.0])
        });
    }
    if let (Some(src), Some(dst)) = (&light.cone_angle, light_opts.get_mut("coneAngle")) {
        apply_prop(dst, src, rate, &|vals| Value::Scalar(vals.first().copied().unwrap_or(90.0)));
    }
    if let (Some(src), Some(dst)) = (&light.cone_feather, light_opts.get_mut("coneFeather")) {
        apply_prop(dst, src, rate, &|vals| Value::Scalar(vals.first().copied().unwrap_or(50.0)));
    }
    if let (Some(src), Some(dst)) = (&light.casts_shadows, light_opts.get_mut("castsShadows")) {
        apply_prop(dst, src, rate, &|vals| Value::Bool(vals.first().copied().unwrap_or(0.0) > 0.5));
    }
}

fn apply_prop(dst: &mut Property, src: &AepProperty, rate: FrameRate, conv: &dyn Fn(&[f64]) -> Value) {
    if let Some(expr_text) = &src.expression {
        dst.expr = Some(effectcraft_project::Expression {
            text: expr_text.clone(),
            enabled: true,
        });
    }
    let scale_factor = if src.unit_ratio { 100.0 } else { 1.0 };
    if !src.static_value.is_empty() {
        dst.value = conv(&src.static_value);
    }
    if !src.keyframes.is_empty() {
        dst.keys.clear();
        for ak in &src.keyframes {
            let time = seconds_to_tick(ak.time_seconds, rate);
            let value = conv(&ak.value);
            let mut key = Keyframe::new(time, value);
            key.in_interp = ak.in_interp;
            key.out_interp = ak.out_interp;
            key.in_ease = ak.in_ease.iter().map(|e| Ease { speed: e.speed * scale_factor, influence: e.influence }).collect();
            key.out_ease = ak.out_ease.iter().map(|e| Ease { speed: e.speed * scale_factor, influence: e.influence }).collect();
            if src.spatial {
                key.spatial_in = ak.spatial_in;
                key.spatial_out = ak.spatial_out;
                let has_tangents = ak.spatial_in.iter().any(|v| v.abs() > 1e-6) || ak.spatial_out.iter().any(|v| v.abs() > 1e-6);
                key.spatial_auto = !has_tangents;
            }
            effectcraft_keyframe::set_key(&mut dst.keys, key);
        }
        if let Some(first) = dst.keys.first() {
            dst.value = first.value.clone();
        }
    }
}

fn apply_dynamic_prop(dst: &mut Property, src: &AepProperty, rate: FrameRate) {
    #[derive(Clone, Copy)]
    enum Kind {
        Scalar,
        Vec2,
        Vec3,
        Color,
        Bool,
        Enum,
        Other,
    }
    let kind = match dst.value {
        Value::Scalar(_) => Kind::Scalar,
        Value::Vec2(_) => Kind::Vec2,
        Value::Vec3(_) => Kind::Vec3,
        Value::Color(_) => Kind::Color,
        Value::Bool(_) => Kind::Bool,
        Value::Enum(_) => Kind::Enum,
        _ => Kind::Other,
    };
    apply_prop(dst, src, rate, &|vals| match kind {
        Kind::Scalar => Value::Scalar(vals.first().copied().unwrap_or(0.0)),
        Kind::Vec2 => Value::Vec2([vals.first().copied().unwrap_or(0.0), vals.get(1).copied().unwrap_or(0.0)]),
        Kind::Vec3 => Value::Vec3([
            vals.first().copied().unwrap_or(0.0),
            vals.get(1).copied().unwrap_or(0.0),
            vals.get(2).copied().unwrap_or(0.0),
        ]),
        Kind::Color => {
            let r = vals.first().copied().unwrap_or(1.0);
            let g = vals.get(1).copied().unwrap_or(1.0);
            let b = vals.get(2).copied().unwrap_or(1.0);
            let a = vals.get(3).copied().unwrap_or(1.0);
            Value::Color([r, g, b, a])
        }
        Kind::Bool => Value::Bool(vals.first().copied().unwrap_or(0.0) > 0.5),
        Kind::Enum => Value::Enum(vals.first().copied().unwrap_or(0.0).round() as u32),
        Kind::Other => Value::Scalar(vals.first().copied().unwrap_or(0.0)),
    });
}

fn ae_match_to_effect_id(ae_name: &str) -> Option<&'static str> {
    match ae_name {
        "ADBE Gaussian Blur 2" | "ADBE Gaussian Blur" => Some("ec.blur.gaussian"),
        "ADBE Box Blur2" | "ADBE Fast Blur" => Some("ec.blur.fastbox"),
        "ADBE Motion Blur" => Some("ec.blur.directional"),
        "ADBE Radial Blur" => Some("ec.blur.radial"),
        "ADBE Camera Lens Blur" => Some("ec.blur.cameralens"),
        "ADBE Channel Blur" => Some("ec.blur.channel"),
        "ADBE Compound Blur" => Some("ec.blur.compound"),
        "ADBE Sharpen" => Some("ec.blur.sharpen"),
        "ADBE Smart Blur" => Some("ec.blur.smart"),
        "ADBE Unsharp Mask2" => Some("ec.blur.unsharp"),
        "ADBE Invert" => Some("ec.channel.invert"),
        "ADBE Set Matte3" => Some("ec.channel.setmatte"),
        "ADBE Shift Channels" => Some("ec.channel.shiftchannels"),
        "ADBE Minimax" => Some("ec.channel.minimax"),
        "ADBE Blend" => Some("ec.channel.blend"),
        "ADBE Solid Composite" => Some("ec.channel.solidcomposite"),
        "ADBE Brightness & Contrast 2" => Some("ec.color.brightnesscontrast"),
        "ADBE CurvesCustom" => Some("ec.color.curves"),
        "ADBE Easy Levels2" => Some("ec.color.levels"),
        "ADBE Pro Levels2" => Some("ec.color.levelsic"),
        "ADBE HUE SATURATION" => Some("ec.color.huesaturation"),
        "ADBE Exposure2" => Some("ec.color.exposure"),
        "ADBE Tint" => Some("ec.color.tint"),
        "ADBE Tritone" => Some("ec.color.tritone"),
        "ADBE Black&White" => Some("ec.color.blackwhite"),
        "ADBE Color Balance 2" => Some("ec.color.colorbalance"),
        "ADBE Photo Filter" => Some("ec.color.photofilter"),
        "ADBE Vibrance" => Some("ec.color.vibrance"),
        "ADBE Lumetri" => Some("ec.color.lumetri"),
        "ADBE Slider Control" => Some("ec.control.slider"),
        "ADBE Angle Control" => Some("ec.control.angle"),
        "ADBE Checkbox Control" => Some("ec.control.checkbox"),
        "ADBE Color Control" => Some("ec.control.color"),
        "ADBE Point Control" => Some("ec.control.point"),
        "ADBE Point3D Control" => Some("ec.control.point3d"),
        "ADBE Layer Control" => Some("ec.control.layer"),
        "ADBE Dropdown Control" => Some("ec.control.dropdown"),
        "ADBE Geometry2" | "ADBE Transform" => Some("ec.distort.transform"),
        "ADBE Corner Pin" => Some("ec.distort.cornerpin"),
        "ADBE Bulge" => Some("ec.distort.bulge"),
        "ADBE Mirror" => Some("ec.distort.mirror"),
        "ADBE Offset" => Some("ec.distort.offset"),
        "ADBE Ripple" => Some("ec.distort.ripple"),
        "ADBE Spherize" => Some("ec.distort.spherize"),
        "ADBE Twirl" => Some("ec.distort.twirl"),
        "ADBE Wave Warp" => Some("ec.distort.wavewarp"),
        "ADBE Turbulent Displace" => Some("ec.distort.turbulentdisplace"),
        "ADBE Displacement Map" => Some("ec.distort.displacementmap"),
        "ADBE Polar Coordinates" => Some("ec.distort.polar"),
        "ADBE Optics Compensation" => Some("ec.distort.opticscompensation"),
        "ADBE Magnify" => Some("ec.distort.magnify"),
        "ADBE SubspaceStabilizer" => Some("ec.distort.warpstabilizer"),
        "ADBE FreePin3" => Some("ec.distort.puppet"),
        "ADBE Fill" => Some("ec.generate.fill"),
        "ADBE Ramp" => Some("ec.generate.gradientramp"),
        "ADBE 4ColorGradient" => Some("ec.generate.fourcolor"),
        "ADBE Stroke" => Some("ec.generate.stroke"),
        "ADBE Grid" => Some("ec.generate.grid"),
        "ADBE Checkerboard" => Some("ec.generate.checkerboard"),
        "ADBE Circle" => Some("ec.generate.circle"),
        "ADBE Ellipse" => Some("ec.generate.ellipse"),
        "ADBE Cell Pattern" => Some("ec.generate.cellpattern"),
        "ADBE Lens Flare" => Some("ec.generate.lensflare"),
        "ADBE Vegas" => Some("ec.generate.vegas"),
        "ADBE Write On" => Some("ec.generate.writeon"),
        "ADBE Color Key" => Some("ec.key.colorkey"),
        "ADBE Luma Key" => Some("ec.key.luma"),
        "ADBE Linear Color Key2" => Some("ec.key.linearcolor"),
        "ADBE Simple Choker" => Some("ec.matte.simplechoker"),
        "ADBE Matte Choker" => Some("ec.matte.mattechoker"),
        "ADBE Noise" => Some("ec.noise.noise"),
        "ADBE Fractal Noise" => Some("ec.noise.fractal"),
        "ADBE Median" => Some("ec.noise.median"),
        "ADBE Dust & Scratches" => Some("ec.noise.dustscratches"),
        "ADBE Turbulent Noise" => Some("ec.noise.turbulent"),
        "ADBE Drop Shadow" => Some("ec.perspective.dropshadow"),
        "ADBE Radial Shadow" => Some("ec.perspective.radialshadow"),
        "ADBE Bevel Alpha" => Some("ec.perspective.bevelalpha"),
        "ADBE Bevel Edges" => Some("ec.perspective.beveledges"),
        "ADBE 3D Glasses2" => Some("ec.perspective.3dglasses"),
        "ADBE Glo2" => Some("ec.stylize.glow"),
        "ADBE Mosaic" => Some("ec.stylize.mosaic"),
        "ADBE Posterize" => Some("ec.stylize.posterize"),
        "ADBE Threshold2" => Some("ec.stylize.threshold"),
        "ADBE Find Edges" => Some("ec.stylize.findedges"),
        "ADBE Emboss" => Some("ec.stylize.emboss"),
        "ADBE Roughen Edges" => Some("ec.stylize.roughenedges"),
        "ADBE Motion Tile" | "ADBE Tile" => Some("ec.stylize.motiontile"),
        "ADBE Echo" => Some("ec.time.echo"),
        "ADBE Posterize Time" => Some("ec.time.posterizetime"),
        "ADBE Timewarp" => Some("ec.time.timewarp"),
        "ADBE Linear Wipe" => Some("ec.transition.linearwipe"),
        "ADBE Radial Wipe" => Some("ec.transition.radialwipe"),
        "ADBE Venetian Blinds" => Some("ec.transition.venetian"),
        "ADBE CC Composite" => Some("ec.channel.cccomposite"),
        _ => None,
    }
}

fn build_shape_group(ids: &mut Ids, sg: &AepShapeGroup, rate: FrameRate) -> PropGroup {
    let mut items = Vec::new();
    let vec2_conv = |vals: &[f64], def: [f64; 2]| -> Value {
        Value::Vec2([vals.first().copied().unwrap_or(def[0]), vals.get(1).copied().unwrap_or(def[1])])
    };
    let color_conv = |vals: &[f64], def: [f64; 4]| -> Value {
        let c = if let &[r, g, b, a, ..] = vals {
            [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0), a.clamp(0.0, 1.0)]
        } else {
            def
        };
        Value::Color(c)
    };

    if sg.rect_size.is_some() || sg.rect_position.is_some() || sg.rect_roundness.is_some() {
        let mut rect = build::shape_rect(ids, [100.0, 100.0], [0.0, 0.0], 0.0);
        if let (Some(src), Some(dst)) = (&sg.rect_size, rect.get_mut("size")) {
            apply_prop(dst, src, rate, &|v| vec2_conv(v, [100.0, 100.0]));
        }
        if let (Some(src), Some(dst)) = (&sg.rect_position, rect.get_mut("position")) {
            apply_prop(dst, src, rate, &|v| {
                let raw = [v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0)];
                Value::Vec2(aep_vector_pos_to_ec(raw))
            });
        }
        if let (Some(src), Some(dst)) = (&sg.rect_roundness, rect.get_mut("roundness")) {
            apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(0.0)));
        }
        items.push(rect);
    }

    if sg.ellipse_size.is_some() || sg.ellipse_position.is_some() {
        let mut ellipse = build::shape_ellipse(ids, [100.0, 100.0], [0.0, 0.0]);
        if let (Some(src), Some(dst)) = (&sg.ellipse_size, ellipse.get_mut("size")) {
            apply_prop(dst, src, rate, &|v| vec2_conv(v, [100.0, 100.0]));
        }
        if let (Some(src), Some(dst)) = (&sg.ellipse_position, ellipse.get_mut("position")) {
            apply_prop(dst, src, rate, &|v| {
                let raw = [v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0)];
                Value::Vec2(aep_vector_pos_to_ec(raw))
            });
        }
        items.push(ellipse);
    }

    if sg.star_points.is_some() || sg.star_outer_radius.is_some() {
        let is_star = sg.star_type.as_ref().and_then(|p| p.static_value.first().copied()).map(|t| t < 1.5).unwrap_or(true);
        let points = sg.star_points.as_ref().and_then(|p| p.static_value.first().copied()).unwrap_or(5.0);
        let outer = sg.star_outer_radius.as_ref().and_then(|p| p.static_value.first().copied()).unwrap_or(100.0);
        let inner = sg.star_inner_radius.as_ref().and_then(|p| p.static_value.first().copied()).unwrap_or(50.0);
        let mut star = build::shape_star(ids, is_star, points, [0.0, 0.0], outer, inner);
        if let (Some(src), Some(dst)) = (&sg.star_points, star.get_mut("points")) {
            apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(5.0)));
        }
        if let (Some(src), Some(dst)) = (&sg.star_position, star.get_mut("position")) {
            apply_prop(dst, src, rate, &|v| {
                let raw = [v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0)];
                Value::Vec2(aep_vector_pos_to_ec(raw))
            });
        }
        if let (Some(src), Some(dst)) = (&sg.star_rotation, star.get_mut("rotation")) {
            apply_prop(dst, src, rate, &|v| Value::Scalar(aep_rotation_to_ec(v.first().copied().unwrap_or(0.0))));
        }
        if let (Some(src), Some(dst)) = (&sg.star_outer_radius, star.get_mut("outerRadius")) {
            apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(100.0)));
        }
        if let (Some(src), Some(dst)) = (&sg.star_outer_roundness, star.get_mut("outerRoundness")) {
            apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(0.0)));
        }
        if is_star {
            if let (Some(src), Some(dst)) = (&sg.star_inner_radius, star.get_mut("innerRadius")) {
                apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(50.0)));
            }
            if let (Some(src), Some(dst)) = (&sg.star_inner_roundness, star.get_mut("innerRoundness")) {
                apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(0.0)));
            }
        }
        items.push(star);
    }

    if let Some(path_data) = &sg.custom_path {
        let path_obj = ShapePath {
            vertices: path_data.vertices.clone(),
            in_tangents: path_data.in_tangents.clone(),
            out_tangents: path_data.out_tangents.clone(),
            closed: path_data.closed,
            feather: Vec::new(),
        };
        let path_grp = build::shape_path(ids, path_obj);
        items.push(path_grp);
    }

    if sg.stroke_width.is_some() || sg.stroke_color.is_some() || sg.stroke_opacity.is_some() {
        let mut stroke = build::shape_stroke(ids, [1.0, 1.0, 1.0, 1.0], 2.0);
        if let (Some(src), Some(dst)) = (&sg.stroke_color, stroke.get_mut("color")) {
            apply_prop(dst, src, rate, &|v| color_conv(v, [1.0, 1.0, 1.0, 1.0]));
        }
        if let (Some(src), Some(dst)) = (&sg.stroke_width, stroke.get_mut("width")) {
            apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(2.0).max(0.0)));
        }
        if let (Some(src), Some(dst)) = (&sg.stroke_opacity, stroke.get_mut("opacity")) {
            apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(100.0).clamp(0.0, 100.0)));
        }
        items.push(stroke);
    }

    if sg.gstroke_colors.is_some() || sg.gstroke_width.is_some() {
        let radial = sg.gstroke_type.as_ref().and_then(|p| p.static_value.first().copied()).map(|t| t > 1.5).unwrap_or(false);
        let start = [0.0, 0.0];
        let end = [100.0, 0.0];
        let g = Gradient::default();
        let mut gstroke = build::shape_gradient_stroke(ids, radial, start, end, g, 2.0);
        if let (Some(src), Some(dst)) = (&sg.gstroke_start, gstroke.get_mut("start")) {
            apply_prop(dst, src, rate, &|v| {
                let raw = [v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0)];
                Value::Vec2(aep_vector_pos_to_ec(raw))
            });
        }
        if let (Some(src), Some(dst)) = (&sg.gstroke_end, gstroke.get_mut("end")) {
            apply_prop(dst, src, rate, &|v| {
                let raw = [v.first().copied().unwrap_or(100.0), v.get(1).copied().unwrap_or(0.0)];
                Value::Vec2(aep_vector_pos_to_ec(raw))
            });
        }
        if let (Some(src), Some(dst)) = (&sg.gstroke_width, gstroke.get_mut("width")) {
            apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(2.0).max(0.0)));
        }
        if let (Some(src), Some(dst)) = (&sg.gstroke_opacity, gstroke.get_mut("opacity")) {
            apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(100.0).clamp(0.0, 100.0)));
        }
        items.push(gstroke);
    }

    if sg.gfill_colors.is_some() || sg.gfill_start.is_some() || sg.gfill_end.is_some() {
        let radial = sg.gfill_type.as_ref().and_then(|p| p.static_value.first().copied()).map(|t| t > 1.5).unwrap_or(false);
        let start = [0.0, 0.0];
        let end = [100.0, 0.0];
        let g = Gradient::default();
        let mut gfill = build::shape_gradient_fill(ids, radial, start, end, g);
        if let (Some(src), Some(dst)) = (&sg.gfill_start, gfill.get_mut("start")) {
            apply_prop(dst, src, rate, &|v| {
                let raw = [v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0)];
                Value::Vec2(aep_vector_pos_to_ec(raw))
            });
        }
        if let (Some(src), Some(dst)) = (&sg.gfill_end, gfill.get_mut("end")) {
            apply_prop(dst, src, rate, &|v| {
                let raw = [v.first().copied().unwrap_or(100.0), v.get(1).copied().unwrap_or(0.0)];
                Value::Vec2(aep_vector_pos_to_ec(raw))
            });
        }
        if let (Some(src), Some(dst)) = (&sg.gfill_opacity, gfill.get_mut("opacity")) {
            apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(100.0).clamp(0.0, 100.0)));
        }
        items.push(gfill);
    } else {
        // Always include a Fill so shapes are visible even when AE wrote default Fill 1 without child streams.
        let mut fill = build::shape_fill(ids, [1.0, 0.0, 0.0, 1.0]);
        if let (Some(src), Some(dst)) = (&sg.fill_color, fill.get_mut("color")) {
            apply_prop(dst, src, rate, &|v| color_conv(v, [1.0, 0.0, 0.0, 1.0]));
        }
        if let (Some(src), Some(dst)) = (&sg.fill_opacity, fill.get_mut("opacity")) {
            apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(100.0).clamp(0.0, 100.0)));
        }
        items.push(fill);
    }

    if sg.trim_start.is_some() || sg.trim_end.is_some() || sg.trim_offset.is_some() {
        let mut trim = build::shape_trim(ids, 0.0, 100.0, 0.0);
        if let (Some(src), Some(dst)) = (&sg.trim_start, trim.get_mut("start")) {
            apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(0.0).clamp(0.0, 100.0)));
        }
        if let (Some(src), Some(dst)) = (&sg.trim_end, trim.get_mut("end")) {
            apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(100.0).clamp(0.0, 100.0)));
        }
        if let (Some(src), Some(dst)) = (&sg.trim_offset, trim.get_mut("offset")) {
            apply_prop(dst, src, rate, &|v| Value::Scalar(aep_rotation_to_ec(v.first().copied().unwrap_or(0.0))));
        }
        items.push(trim);
    }

    if sg.repeater_copies.is_some() || sg.repeater_offset.is_some() {
        let copies = sg.repeater_copies.as_ref().and_then(|p| p.static_value.first().copied()).unwrap_or(3.0);
        let mut repeater = build::shape_repeater(ids, copies, [100.0, 0.0]);
        if let (Some(src), Some(dst)) = (&sg.repeater_copies, repeater.get_mut("copies")) {
            apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(3.0)));
        }
        if let (Some(src), Some(dst)) = (&sg.repeater_offset, repeater.get_mut("offset")) {
            apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(0.0)));
        }
        items.push(repeater);
    }

    let mut group = build::shape_group(ids, &sg.name, items);
    if let Some(tr_group) = group.sub_mut("transform") {
        if let (Some(src), Some(dst)) = (&sg.transform.anchor_point, tr_group.get_mut("anchor")) {
            apply_prop(dst, src, rate, &|v| vec2_conv(v, [0.0, 0.0]));
        }
        if let (Some(src), Some(dst)) = (&sg.transform.position, tr_group.get_mut("position")) {
            apply_prop(dst, src, rate, &|v| {
                let raw = [v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0)];
                Value::Vec2(aep_vector_pos_to_ec(raw))
            });
        }
        if let (Some(src), Some(dst)) = (&sg.transform.scale, tr_group.get_mut("scale")) {
            let unit_ratio = src.unit_ratio;
            apply_prop(dst, src, rate, &|v| {
                let def = if unit_ratio { 1.0 } else { 100.0 };
                let sx = v.first().copied().unwrap_or(def);
                let sy = v.get(1).copied().unwrap_or(sx);
                let sc = aep_scale_to_ec([sx, sy, 100.0], unit_ratio);
                Value::Vec2([sc[0], sc[1]])
            });
        }
        if let (Some(src), Some(dst)) = (&sg.transform.rotate_z, tr_group.get_mut("rotation")) {
            apply_prop(dst, src, rate, &|v| Value::Scalar(aep_rotation_to_ec(v.first().copied().unwrap_or(0.0))));
        }
        if let (Some(src), Some(dst)) = (&sg.transform.opacity, tr_group.get_mut("opacity")) {
            apply_prop(dst, src, rate, &|v| Value::Scalar(v.first().copied().unwrap_or(100.0).clamp(0.0, 100.0)));
        }
    }
    group
}

/// Parse an `.aep` or `.aepx` byte buffer and convert it directly into an EffectCraft [`Project`].
pub fn import_aep_bytes(bytes: &[u8], fallback_name: &str) -> Result<Project, String> {
    let aep = parse_aep(bytes, fallback_name)?;
    if aep.comps.is_empty() && aep.solids.is_empty() && aep.folders.is_empty() {
        return Err("After Effects project contains no compositions or items".into());
    }
    Ok(aep_to_project(&aep))
}

// ----------------------------------------------------------------------------
// AEPX XML Parser (`<AfterEffectsProject>` XML format)
// ----------------------------------------------------------------------------

fn xml_attr(tag: &str, name: &str) -> Option<String> {
    let pat = format!("{name}=\"");
    let start = tag.find(&pat)?.saturating_add(pat.len());
    let rest = tag.get(start..)?;
    let end = rest.find('"')?;
    Some(rest.get(..end)?.to_string())
}

fn parse_aepx_xml(xml: &str, fallback_name: &str) -> Result<AepProject, String> {
    if !xml.contains("<AfterEffectsProject") {
        return Err("not an AfterEffectsProject XML document".into());
    }
    let mut proj = AepProject::default();
    let mut comp_id_counter: u32 = 1;
    let mut layer_id_counter: u32 = 1;

    // Parse `<Folder .../>`
    let mut cursor = xml;
    while let Some(pos) = cursor.find("<Folder") {
        let after = cursor.get(pos..).unwrap_or("");
        let Some(tag_end) = after.find('>') else { break };
        let tag = after.get(..=tag_end).unwrap_or("");
        let id = xml_attr(tag, "id").and_then(|v| v.parse::<u32>().ok()).unwrap_or(comp_id_counter);
        comp_id_counter = comp_id_counter.max(id.saturating_add(1));
        let name = xml_attr(tag, "name").unwrap_or_else(|| format!("Folder {id}"));
        let parent_id = xml_attr(tag, "parentId").and_then(|v| v.parse::<u32>().ok());
        proj.folders.push(AepFolder { id, name, parent_id });
        cursor = after.get(tag_end + 1..).unwrap_or("");
    }

    // Parse `<Solid .../>`
    cursor = xml;
    while let Some(pos) = cursor.find("<Solid") {
        let after = cursor.get(pos..).unwrap_or("");
        let Some(tag_end) = after.find('>') else { break };
        let tag = after.get(..=tag_end).unwrap_or("");
        let id = xml_attr(tag, "id").and_then(|v| v.parse::<u32>().ok()).unwrap_or(comp_id_counter);
        comp_id_counter = comp_id_counter.max(id.saturating_add(1));
        let name = xml_attr(tag, "name").unwrap_or_else(|| format!("Solid {id}"));
        let parent_id = xml_attr(tag, "parentId").and_then(|v| v.parse::<u32>().ok());
        let width = xml_attr(tag, "width").and_then(|v| v.parse::<u32>().ok()).unwrap_or(1920);
        let height = xml_attr(tag, "height").and_then(|v| v.parse::<u32>().ok()).unwrap_or(1080);
        let color_vals = xml_attr(tag, "color").as_deref().map(parse_csv_f64s).unwrap_or_default();
        let color = [
            color_vals.first().copied().unwrap_or(0.5) as f32,
            color_vals.get(1).copied().unwrap_or(0.5) as f32,
            color_vals.get(2).copied().unwrap_or(0.5) as f32,
        ];
        proj.solids.push(AepSolid { id, name, width, height, color, parent_id });
        cursor = after.get(tag_end + 1..).unwrap_or("");
    }

    // Parse `<Footage .../>`
    cursor = xml;
    while let Some(pos) = cursor.find("<Footage") {
        let after = cursor.get(pos..).unwrap_or("");
        let Some(tag_end) = after.find('>') else { break };
        let tag = after.get(..=tag_end).unwrap_or("");
        let id = xml_attr(tag, "id").and_then(|v| v.parse::<u32>().ok()).unwrap_or(comp_id_counter);
        comp_id_counter = comp_id_counter.max(id.saturating_add(1));
        let name = xml_attr(tag, "name").unwrap_or_else(|| format!("Footage {id}"));
        let path = xml_attr(tag, "path").unwrap_or_else(|| name.clone());
        let parent_id = xml_attr(tag, "parentId").and_then(|v| v.parse::<u32>().ok());
        let width = xml_attr(tag, "width").and_then(|v| v.parse::<u32>().ok()).unwrap_or(1920);
        let height = xml_attr(tag, "height").and_then(|v| v.parse::<u32>().ok()).unwrap_or(1080);
        let fps = xml_attr(tag, "fps").and_then(|v| v.parse::<f64>().ok()).unwrap_or(30.0);
        let duration_seconds = xml_attr(tag, "duration").and_then(|v| v.parse::<f64>().ok()).unwrap_or(10.0);
        let has_video = xml_attr(tag, "hasVideo").and_then(|v| v.parse::<bool>().ok()).unwrap_or(true);
        let has_audio = xml_attr(tag, "hasAudio").and_then(|v| v.parse::<bool>().ok()).unwrap_or(false);
        proj.footages.push(AepFootage { id, name, path, width, height, fps, duration_seconds, has_video, has_audio, parent_id });
        cursor = after.get(tag_end + 1..).unwrap_or("");
    }

    // Parse `<Composition ...>...</Composition>` blocks.
    cursor = xml;
    while let Some(c_pos) = cursor.find("<Composition") {
        let after_start = cursor.get(c_pos..).unwrap_or("");
        let Some(tag_end) = after_start.find('>') else { break };
        let open_tag = after_start.get(..=tag_end).unwrap_or("");
        let self_closing = open_tag.ends_with("/>");

        let id = xml_attr(open_tag, "id").and_then(|v| v.parse::<u32>().ok()).unwrap_or(comp_id_counter);
        comp_id_counter = comp_id_counter.max(id.saturating_add(1));
        let name = xml_attr(open_tag, "name").unwrap_or_else(|| format!("{fallback_name} {id}"));
        let parent_id = xml_attr(open_tag, "parentId").and_then(|v| v.parse::<u32>().ok());
        let width = xml_attr(open_tag, "width").and_then(|v| v.parse::<u32>().ok()).filter(|&w| w > 0).unwrap_or(1920);
        let height = xml_attr(open_tag, "height").and_then(|v| v.parse::<u32>().ok()).filter(|&h| h > 0).unwrap_or(1080);
        let fps = xml_attr(open_tag, "frameRate")
            .or_else(|| xml_attr(open_tag, "fps"))
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|&f| f > 0.0)
            .unwrap_or(30.0);
        let duration_seconds = xml_attr(open_tag, "duration")
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|&d| d > 0.0)
            .unwrap_or(3.0);

        let (body, next_cursor) = if self_closing {
            ("", after_start.get(tag_end + 1..).unwrap_or(""))
        } else if let Some(close_pos) = after_start.find("</Composition>") {
            let b = after_start.get(tag_end + 1..close_pos).unwrap_or("");
            let next = after_start.get(close_pos + "</Composition>".len()..).unwrap_or("");
            (b, next)
        } else {
            break;
        };

        // Strip `<Layer ...>...</Layer>` from body to parse only Composition-level `<Marker .../>`.
        let mut comp_body = String::new();
        let mut cur = body;
        while let Some(l_start) = cur.find("<Layer") {
            comp_body.push_str(cur.get(..l_start).unwrap_or(""));
            let after_l = cur.get(l_start..).unwrap_or("");
            if let Some(l_end) = after_l.find("</Layer>") {
                cur = after_l.get(l_end + "</Layer>".len()..).unwrap_or("");
            } else if let Some(tag_end) = after_l.find("/>") {
                cur = after_l.get(tag_end + "/>".len()..).unwrap_or("");
            } else {
                break;
            }
        }
        comp_body.push_str(cur);

        let mut markers = Vec::new();
        let mut m_cursor = comp_body.as_str();
        while let Some(m_pos) = m_cursor.find("<Marker") {
            let after_m = m_cursor.get(m_pos..).unwrap_or("");
            let Some(m_tag_end) = after_m.find('>') else { break };
            let m_tag = after_m.get(..=m_tag_end).unwrap_or("");
            let time_seconds = xml_attr(m_tag, "time").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
            let duration_seconds = xml_attr(m_tag, "duration").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
            let comment = xml_attr(m_tag, "comment").unwrap_or_default();
            markers.push(AepMarker { time_seconds, duration_seconds, comment });
            m_cursor = after_m.get(m_tag_end + 1..).unwrap_or("");
        }

        let layers = parse_aepx_layers(body, fps, duration_seconds, &mut layer_id_counter);
        proj.comps.push(AepComp {
            id,
            name,
            parent_id,
            width,
            height,
            pixel_aspect: 1.0,
            fps,
            duration_seconds,
            background: [0.0, 0.0, 0.0],
            shutter_angle: 180.0,
            markers,
            layers,
        });
        cursor = next_cursor;
    }

    Ok(proj)
}

fn parse_aepx_layers(body: &str, fps: f64, comp_duration: f64, layer_id_counter: &mut u32) -> Vec<AepLayer> {
    let mut layers = Vec::new();
    let mut cursor = body;
    while let Some(l_pos) = cursor.find("<Layer") {
        let after_start = cursor.get(l_pos..).unwrap_or("");
        let Some(tag_end) = after_start.find('>') else { break };
        let open_tag = after_start.get(..=tag_end).unwrap_or("");
        let self_closing = open_tag.ends_with("/>");

        let id = xml_attr(open_tag, "id").and_then(|v| v.parse::<u32>().ok()).unwrap_or(*layer_id_counter);
        *layer_id_counter = (*layer_id_counter).max(id.saturating_add(1));
        let name = xml_attr(open_tag, "name").unwrap_or_else(|| format!("Layer {id}"));
        let source_id = xml_attr(open_tag, "sourceId").and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
        let kind_str = xml_attr(open_tag, "type").or_else(|| xml_attr(open_tag, "kind")).unwrap_or_else(|| "Shape".into());
        let kind = match kind_str.to_ascii_lowercase().as_str() {
            "text" => AepLayerKind::Text,
            "null" => AepLayerKind::Null,
            "camera" => AepLayerKind::Camera,
            "light" => AepLayerKind::Light(LightKind::Point),
            "solid" => AepLayerKind::Solid(source_id),
            "footage" => AepLayerKind::Footage(source_id),
            "comp" | "precomp" => AepLayerKind::Comp(source_id),
            _ => AepLayerKind::Shape,
        };
        let in_point_seconds = xml_attr(open_tag, "inPoint").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        let out_point_seconds = xml_attr(open_tag, "outPoint").and_then(|v| v.parse::<f64>().ok()).unwrap_or(comp_duration);
        let start_time_seconds = xml_attr(open_tag, "startTime").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        let stretch = xml_attr(open_tag, "stretch").and_then(|v| v.parse::<f64>().ok()).unwrap_or(100.0);
        let video_active = xml_attr(open_tag, "video").and_then(|v| v.parse::<bool>().ok()).unwrap_or(true);
        let audio_active = xml_attr(open_tag, "audio").and_then(|v| v.parse::<bool>().ok()).unwrap_or(true);
        let solo = xml_attr(open_tag, "solo").and_then(|v| v.parse::<bool>().ok()).unwrap_or(false);
        let locked = xml_attr(open_tag, "locked").and_then(|v| v.parse::<bool>().ok()).unwrap_or(false);
        let shy = xml_attr(open_tag, "shy").and_then(|v| v.parse::<bool>().ok()).unwrap_or(false);
        let collapse = xml_attr(open_tag, "collapse").and_then(|v| v.parse::<bool>().ok()).unwrap_or(false);
        let motion_blur = xml_attr(open_tag, "motionBlur").and_then(|v| v.parse::<bool>().ok()).unwrap_or(false);
        let adjustment = xml_attr(open_tag, "adjustment").and_then(|v| v.parse::<bool>().ok()).unwrap_or(false);
        let three_d = xml_attr(open_tag, "threeD").and_then(|v| v.parse::<bool>().ok()).unwrap_or(false);
        let parent_id = xml_attr(open_tag, "parentId").and_then(|v| v.parse::<u32>().ok());

        let blend_mode = xml_attr(open_tag, "blendMode")
            .and_then(|b| b.parse::<u8>().ok())
            .map(decode_blend_mode)
            .unwrap_or(BlendMode::Normal);
        let track_matte_kind = xml_attr(open_tag, "trackMatte")
            .and_then(|m| m.parse::<u8>().ok())
            .and_then(decode_track_matte);

        let (layer_body, next_cursor) = if self_closing {
            ("", after_start.get(tag_end + 1..).unwrap_or(""))
        } else if let Some(close_pos) = after_start.find("</Layer>") {
            let b = after_start.get(tag_end + 1..close_pos).unwrap_or("");
            let next = after_start.get(close_pos + "</Layer>".len()..).unwrap_or("");
            (b, next)
        } else {
            break;
        };

        let transform = parse_aepx_transform(layer_body, fps);
        let mut shape_groups = Vec::new();
        if kind == AepLayerKind::Shape {
            let mut sg = AepShapeGroup {
                name: "Shape Group 1".into(),
                ..Default::default()
            };
            let mut found_any = false;

            if let Some(r_pos) = layer_body.find("<Rectangle") {
                found_any = true;
                let tag = layer_body.get(r_pos..).and_then(|s| s.get(..=s.find('>')?)).unwrap_or("");
                let size = xml_attr(tag, "size").as_deref().map(parse_csv_f64s).unwrap_or_else(|| vec![200.0, 200.0]);
                let roundness = xml_attr(tag, "roundness").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
                sg.rect_size = Some(AepProperty {
                    match_name: "ADBE Vector Rect Size".into(),
                    name: "Size".into(),
                    dims: 2,
                    spatial: false,
                    unit_ratio: false,
                    static_value: size,
                    keyframes: Vec::new(),
                    expression: None,
                });
                if roundness > 0.0 {
                    sg.rect_roundness = Some(AepProperty {
                        match_name: "ADBE Vector Rect Roundness".into(),
                        name: "Roundness".into(),
                        dims: 1,
                        spatial: false,
                        unit_ratio: false,
                        static_value: vec![roundness],
                        keyframes: Vec::new(),
                        expression: None,
                    });
                }
            }

            if let Some(e_pos) = layer_body.find("<Ellipse") {
                found_any = true;
                let tag = layer_body.get(e_pos..).and_then(|s| s.get(..=s.find('>')?)).unwrap_or("");
                let size = xml_attr(tag, "size").as_deref().map(parse_csv_f64s).unwrap_or_else(|| vec![100.0, 100.0]);
                sg.ellipse_size = Some(AepProperty {
                    match_name: "ADBE Vector Ellipse Size".into(),
                    name: "Size".into(),
                    dims: 2,
                    spatial: false,
                    unit_ratio: false,
                    static_value: size,
                    keyframes: Vec::new(),
                    expression: None,
                });
            }

            if let Some(s_pos) = layer_body.find("<Polystar") {
                found_any = true;
                let tag = layer_body.get(s_pos..).and_then(|s| s.get(..=s.find('>')?)).unwrap_or("");
                let star_type_val = if xml_attr(tag, "type").as_deref() == Some("polygon") { 2.0 } else { 1.0 };
                let points = xml_attr(tag, "points").and_then(|v| v.parse::<f64>().ok()).unwrap_or(5.0);
                let outer = xml_attr(tag, "outerRadius").and_then(|v| v.parse::<f64>().ok()).unwrap_or(100.0);
                let inner = xml_attr(tag, "innerRadius").and_then(|v| v.parse::<f64>().ok()).unwrap_or(50.0);
                sg.star_type = Some(AepProperty {
                    match_name: "ADBE Vector Star Type".into(),
                    name: "Type".into(),
                    dims: 1,
                    spatial: false,
                    unit_ratio: false,
                    static_value: vec![star_type_val],
                    keyframes: Vec::new(),
                    expression: None,
                });
                sg.star_points = Some(AepProperty {
                    match_name: "ADBE Vector Star Points".into(),
                    name: "Points".into(),
                    dims: 1,
                    spatial: false,
                    unit_ratio: false,
                    static_value: vec![points],
                    keyframes: Vec::new(),
                    expression: None,
                });
                sg.star_outer_radius = Some(AepProperty {
                    match_name: "ADBE Vector Star Outer Radius".into(),
                    name: "Outer Radius".into(),
                    dims: 1,
                    spatial: false,
                    unit_ratio: false,
                    static_value: vec![outer],
                    keyframes: Vec::new(),
                    expression: None,
                });
                sg.star_inner_radius = Some(AepProperty {
                    match_name: "ADBE Vector Star Inner Radius".into(),
                    name: "Inner Radius".into(),
                    dims: 1,
                    spatial: false,
                    unit_ratio: false,
                    static_value: vec![inner],
                    keyframes: Vec::new(),
                    expression: None,
                });
            }

            if let Some(p_pos) = layer_body.find("<Path") {
                found_any = true;
                let tag = layer_body.get(p_pos..).and_then(|s| s.get(..=s.find('>')?)).unwrap_or("");
                let verts_str = xml_attr(tag, "vertices").unwrap_or_default();
                let closed = xml_attr(tag, "closed").and_then(|v| v.parse::<bool>().ok()).unwrap_or(true);
                let mut vertices = Vec::new();
                for pair in verts_str.split_whitespace() {
                    let parts: Vec<f64> = pair.split(',').filter_map(|x| x.parse::<f64>().ok()).collect();
                    if parts.len() >= 2 {
                        vertices.push([parts[0], parts[1]]);
                    }
                }
                let in_tangents = vec![[0.0; 2]; vertices.len()];
                let out_tangents = vec![[0.0; 2]; vertices.len()];
                sg.custom_path = Some(AepShapePath { closed, vertices, in_tangents, out_tangents });
            }

            if let Some(t_pos) = layer_body.find("<TrimPaths") {
                let tag = layer_body.get(t_pos..).and_then(|s| s.get(..=s.find('>')?)).unwrap_or("");
                let start = xml_attr(tag, "start").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
                let end = xml_attr(tag, "end").and_then(|v| v.parse::<f64>().ok()).unwrap_or(100.0);
                let offset = xml_attr(tag, "offset").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
                sg.trim_start = Some(AepProperty { match_name: "ADBE Vector Trim Start".into(), name: "Start".into(), dims: 1, spatial: false, unit_ratio: false, static_value: vec![start], keyframes: Vec::new(), expression: None });
                sg.trim_end = Some(AepProperty { match_name: "ADBE Vector Trim End".into(), name: "End".into(), dims: 1, spatial: false, unit_ratio: false, static_value: vec![end], keyframes: Vec::new(), expression: None });
                sg.trim_offset = Some(AepProperty { match_name: "ADBE Vector Trim Offset".into(), name: "Offset".into(), dims: 1, spatial: false, unit_ratio: false, static_value: vec![offset], keyframes: Vec::new(), expression: None });
            }

            if let Some(r_pos) = layer_body.find("<Repeater") {
                let tag = layer_body.get(r_pos..).and_then(|s| s.get(..=s.find('>')?)).unwrap_or("");
                let copies = xml_attr(tag, "copies").and_then(|v| v.parse::<f64>().ok()).unwrap_or(3.0);
                sg.repeater_copies = Some(AepProperty { match_name: "ADBE Vector Repeater Copies".into(), name: "Copies".into(), dims: 1, spatial: false, unit_ratio: false, static_value: vec![copies], keyframes: Vec::new(), expression: None });
            }

            if !found_any {
                // Default fallback rectangle
                sg.name = "Rectangle 1".into();
                sg.rect_size = Some(AepProperty {
                    match_name: "ADBE Vector Rect Size".into(),
                    name: "Size".into(),
                    dims: 2,
                    spatial: false,
                    unit_ratio: false,
                    static_value: vec![200.0, 200.0],
                    keyframes: Vec::new(),
                    expression: None,
                });
            }
            shape_groups.push(sg);
        }

        let mut text: Option<AepText> = None;
        if kind == AepLayerKind::Text {
            let text_content = xml_attr(open_tag, "text").unwrap_or_else(|| "Sample Text".into());
            let font_family = xml_attr(open_tag, "fontFamily").unwrap_or_else(|| "Arial".into());
            let font_size = xml_attr(open_tag, "fontSize").and_then(|v| v.parse::<f64>().ok()).unwrap_or(48.0);
            text = Some(AepText {
                text: text_content,
                font_family,
                font_size,
                fill_color: [1.0, 1.0, 1.0, 1.0],
                stroke_color: [0.0, 0.0, 0.0, 1.0],
                stroke_width: 0.0,
                justification: 0,
                tracking: 0.0,
            });
        }

        let mut camera: Option<AepCamera> = None;
        if kind == AepLayerKind::Camera {
            camera = Some(AepCamera {
                zoom: Some(AepProperty {
                    match_name: "ADBE Camera Zoom".into(),
                    name: "Zoom".into(),
                    dims: 1,
                    spatial: false,
                    unit_ratio: false,
                    static_value: vec![1500.0],
                    keyframes: Vec::new(),
                    expression: None,
                }),
                ..Default::default()
            });
        }

        let mut light: Option<AepLight> = None;
        if let AepLayerKind::Light(l_kind) = kind {
            light = Some(AepLight {
                kind: l_kind,
                intensity: Some(AepProperty {
                    match_name: "ADBE Light Intensity".into(),
                    name: "Intensity".into(),
                    dims: 1,
                    spatial: false,
                    unit_ratio: false,
                    static_value: vec![100.0],
                    keyframes: Vec::new(),
                    expression: None,
                }),
                color: Some(AepProperty {
                    match_name: "ADBE Light Color".into(),
                    name: "Color".into(),
                    dims: 3,
                    spatial: false,
                    unit_ratio: false,
                    static_value: vec![1.0, 1.0, 1.0],
                    keyframes: Vec::new(),
                    expression: None,
                }),
                cone_angle: None,
                cone_feather: None,
                casts_shadows: None,
            });
        }

        let mut markers = Vec::new();
        let mut m_cursor = layer_body;
        while let Some(m_pos) = m_cursor.find("<Marker") {
            let after_m = m_cursor.get(m_pos..).unwrap_or("");
            let Some(m_tag_end) = after_m.find('>') else { break };
            let m_tag = after_m.get(..=m_tag_end).unwrap_or("");
            let time_seconds = xml_attr(m_tag, "time").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
            let duration_seconds = xml_attr(m_tag, "duration").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
            let comment = xml_attr(m_tag, "comment").unwrap_or_default();
            markers.push(AepMarker { time_seconds, duration_seconds, comment });
            m_cursor = after_m.get(m_tag_end + 1..).unwrap_or("");
        }

        let mut masks = Vec::new();
        let mut mask_cursor = layer_body;
        while let Some(m_pos) = mask_cursor.find("<Mask") {
            let after_m = mask_cursor.get(m_pos..).unwrap_or("");
            let Some(m_tag_end) = after_m.find('>') else { break };
            let m_open_tag = after_m.get(..=m_tag_end).unwrap_or("");
            let m_self_closing = m_open_tag.ends_with("/>");
            let (mask_body, m_next) = if m_self_closing {
                ("", after_m.get(m_tag_end + 1..).unwrap_or(""))
            } else if let Some(m_close) = after_m.find("</Mask>") {
                (after_m.get(m_tag_end + 1..m_close).unwrap_or(""), after_m.get(m_close + "</Mask>".len()..).unwrap_or(""))
            } else {
                ("", after_m.get(m_tag_end + 1..).unwrap_or(""))
            };

            let mask_name = xml_attr(m_open_tag, "name").unwrap_or_else(|| format!("Mask {}", masks.len() + 1));
            let mode_str = xml_attr(m_open_tag, "mode").unwrap_or_else(|| "Add".into());
            let mode = MaskMode::from_name(&mode_str).unwrap_or(MaskMode::Add);
            let inverted = xml_attr(m_open_tag, "inverted").and_then(|v| v.parse::<bool>().ok()).unwrap_or(false);

            let feather_val = xml_attr(m_open_tag, "feather").as_deref().map(parse_csv_f64s).unwrap_or_default();
            let feather = if !feather_val.is_empty() {
                Some(AepProperty {
                    match_name: "ADBE Mask Feather".into(),
                    name: "Feather".into(),
                    dims: 2,
                    spatial: false,
                    unit_ratio: false,
                    static_value: feather_val,
                    keyframes: Vec::new(),
                    expression: None,
                })
            } else {
                None
            };

            let opacity_val = xml_attr(m_open_tag, "opacity").and_then(|v| v.parse::<f64>().ok());
            let opacity = opacity_val.map(|v| AepProperty {
                match_name: "ADBE Mask Opacity".into(),
                name: "Opacity".into(),
                dims: 1,
                spatial: false,
                unit_ratio: false,
                static_value: vec![v],
                keyframes: Vec::new(),
                expression: None,
            });

            let expansion_val = xml_attr(m_open_tag, "expansion").and_then(|v| v.parse::<f64>().ok());
            let expansion = expansion_val.map(|v| AepProperty {
                match_name: "ADBE Mask Offset".into(),
                name: "Expansion".into(),
                dims: 1,
                spatial: false,
                unit_ratio: false,
                static_value: vec![v],
                keyframes: Vec::new(),
                expression: None,
            });

            let mut path: Option<AepShapePath> = None;
            if let Some(p_pos) = mask_body.find("<Path") {
                let p_tag = mask_body.get(p_pos..).and_then(|s| s.get(..=s.find('>')?)).unwrap_or("");
                let verts_str = xml_attr(p_tag, "vertices").unwrap_or_default();
                let closed = xml_attr(p_tag, "closed").and_then(|v| v.parse::<bool>().ok()).unwrap_or(true);
                let mut vertices = Vec::new();
                for pair in verts_str.split_whitespace() {
                    let parts: Vec<f64> = pair.split(',').filter_map(|x| x.parse::<f64>().ok()).collect();
                    if parts.len() >= 2 {
                        vertices.push([parts[0], parts[1]]);
                    }
                }
                let in_tangents = vec![[0.0; 2]; vertices.len()];
                let out_tangents = vec![[0.0; 2]; vertices.len()];
                path = Some(AepShapePath { closed, vertices, in_tangents, out_tangents });
            }

            masks.push(AepMask { name: mask_name, mode, inverted, path, feather, opacity, expansion });
            mask_cursor = m_next;
        }

        let mut effects = Vec::new();
        let mut fx_cursor = layer_body;
        while let Some(e_pos) = fx_cursor.find("<Effect") {
            let after_e = fx_cursor.get(e_pos..).unwrap_or("");
            let Some(e_tag_end) = after_e.find('>') else { break };
            let e_open_tag = after_e.get(..=e_tag_end).unwrap_or("");
            let e_self_closing = e_open_tag.ends_with("/>");
            let (fx_inner, e_next) = if e_self_closing {
                ("", after_e.get(e_tag_end + 1..).unwrap_or(""))
            } else if let Some(e_close) = after_e.find("</Effect>") {
                (after_e.get(e_tag_end + 1..e_close).unwrap_or(""), after_e.get(e_close + "</Effect>".len()..).unwrap_or(""))
            } else {
                ("", after_e.get(e_tag_end + 1..).unwrap_or(""))
            };

            let match_name = xml_attr(e_open_tag, "matchName").unwrap_or_else(|| "ADBE Effect Unknown".into());
            let fx_name = xml_attr(e_open_tag, "name").unwrap_or_else(|| match_name.strip_prefix("ADBE ").unwrap_or(&match_name).to_string());
            let enabled = xml_attr(e_open_tag, "enabled").and_then(|v| v.parse::<bool>().ok()).unwrap_or(true);

            let mut properties = Vec::new();
            let mut prop_cursor = fx_inner;
            while let Some(p_pos) = prop_cursor.find("<Property") {
                let after_p = prop_cursor.get(p_pos..).unwrap_or("");
                let Some(p_end) = after_p.find('>') else { break };
                let p_tag = after_p.get(..=p_end).unwrap_or("");
                let p_self_closing = p_tag.ends_with("/>");
                let (p_body, p_next) = if p_self_closing {
                    ("", after_p.get(p_end + 1..).unwrap_or(""))
                } else if let Some(p_close) = after_p.find("</Property>") {
                    (after_p.get(p_end + 1..p_close).unwrap_or(""), after_p.get(p_close + "</Property>".len()..).unwrap_or(""))
                } else {
                    ("", after_p.get(p_end + 1..).unwrap_or(""))
                };

                let p_match = xml_attr(p_tag, "matchName").or_else(|| xml_attr(p_tag, "name")).unwrap_or_default();
                let p_name = xml_attr(p_tag, "name").unwrap_or_else(|| p_match.clone());
                let static_value = xml_attr(p_tag, "value").as_deref().map(parse_csv_f64s).unwrap_or_default();
                let expression = xml_attr(p_tag, "expr");
                let keyframes = parse_aepx_keyframes(p_body, 1, fps);

                properties.push(AepProperty {
                    match_name: p_match,
                    name: p_name,
                    dims: static_value.len() as u16,
                    spatial: false,
                    unit_ratio: false,
                    static_value,
                    keyframes,
                    expression,
                });
                prop_cursor = p_next;
            }

            effects.push(AepEffect { name: fx_name, match_name, enabled, properties });
            fx_cursor = e_next;
        }

        layers.push(AepLayer {
            id,
            name,
            kind,
            source_id,
            start_time_seconds,
            in_point_seconds,
            out_point_seconds,
            stretch,
            video_active,
            audio_active,
            solo,
            locked,
            shy,
            collapse,
            motion_blur,
            adjustment,
            three_d,
            blend_mode,
            track_matte_kind,
            parent_id,
            transform,
            text,
            camera,
            light,
            shape_groups,
            masks,
            effects,
            markers,
        });
        cursor = next_cursor;
    }
    layers
}

fn parse_aepx_transform(layer_body: &str, fps: f64) -> AepTransform {
    let mut tr = AepTransform::default();
    let mut cursor = layer_body;
    while let Some(p_pos) = cursor.find("<Property") {
        let after_start = cursor.get(p_pos..).unwrap_or("");
        let Some(tag_end) = after_start.find('>') else { break };
        let open_tag = after_start.get(..=tag_end).unwrap_or("");
        let self_closing = open_tag.ends_with("/>");

        let prop_name = xml_attr(open_tag, "name").or_else(|| xml_attr(open_tag, "matchName")).unwrap_or_default();
        let value_attr = xml_attr(open_tag, "value");

        let (prop_body, next_cursor) = if self_closing {
            ("", after_start.get(tag_end + 1..).unwrap_or(""))
        } else if let Some(close_pos) = after_start.find("</Property>") {
            let b = after_start.get(tag_end + 1..close_pos).unwrap_or("");
            let next = after_start.get(close_pos + "</Property>".len()..).unwrap_or("");
            (b, next)
        } else {
            break;
        };

        let lower = prop_name.to_ascii_lowercase();
        let (match_name, dims, spatial) = if lower.contains("poi") || lower.contains("point of interest") {
            ("ADBE Point of Interest", 3, true)
        } else if lower.contains("position") {
            ("ADBE Position", 3, true)
        } else if lower.contains("anchor") {
            ("ADBE Anchor Point", 3, true)
        } else if lower.contains("scale") {
            ("ADBE Scale", 3, false)
        } else if lower.contains("orientation") {
            ("ADBE Orientation", 3, false)
        } else if lower.contains("rotat") {
            ("ADBE Rotate Z", 1, false)
        } else if lower.contains("opacity") {
            ("ADBE Opacity", 1, false)
        } else {
            cursor = next_cursor;
            continue;
        };

        let expression = xml_attr(open_tag, "expr");
        let static_value = value_attr.as_deref().map(parse_csv_f64s).unwrap_or_default();
        let keyframes = parse_aepx_keyframes(prop_body, dims, fps);
        let prop = AepProperty {
            match_name: match_name.into(),
            name: prop_name,
            dims,
            spatial,
            unit_ratio: false,
            static_value,
            keyframes,
            expression,
        };
        match match_name {
            "ADBE Position" => tr.position = Some(prop),
            "ADBE Anchor Point" => tr.anchor_point = Some(prop),
            "ADBE Point of Interest" => tr.poi = Some(prop),
            "ADBE Scale" => tr.scale = Some(prop),
            "ADBE Orientation" => tr.orientation = Some(prop),
            "ADBE Rotate Z" => tr.rotate_z = Some(prop),
            "ADBE Opacity" => tr.opacity = Some(prop),
            _ => {}
        }
        cursor = next_cursor;
    }
    tr
}

fn parse_csv_f64s(s: &str) -> Vec<f64> {
    s.split([',', ' ']).map(str::trim).filter(|x| !x.is_empty()).filter_map(|x| x.parse::<f64>().ok()).collect()
}

fn parse_aepx_keyframes(prop_body: &str, dims: u16, fps: f64) -> Vec<AepKeyframe> {
    let mut keys = Vec::new();
    let mut cursor = prop_body;
    while let Some(k_pos) = cursor.find("<Keyframe") {
        let after = cursor.get(k_pos..).unwrap_or("");
        let Some(tag_end) = after.find('>') else { break };
        let open_tag = after.get(..=tag_end).unwrap_or("");
        let time_seconds = xml_attr(open_tag, "time")
            .and_then(|v| v.parse::<f64>().ok())
            .or_else(|| xml_attr(open_tag, "frame").and_then(|v| v.parse::<f64>().ok()).map(|f| f / fps.max(1.0)))
            .unwrap_or(0.0);
        let value = xml_attr(open_tag, "value").as_deref().map(parse_csv_f64s).unwrap_or_else(|| vec![0.0; usize::from(dims)]);
        let interp_str = xml_attr(open_tag, "interp").unwrap_or_else(|| "linear".into());
        let interp = match interp_str.to_ascii_lowercase().as_str() {
            "bezier" | "ease" => Interp::Bezier,
            "hold" => Interp::Hold,
            _ => Interp::Linear,
        };
        let n_ease = usize::from(dims.max(1));
        let default_ease = if interp == Interp::Bezier { Ease::EASY } else { Ease::default() };
        keys.push(AepKeyframe {
            time_seconds,
            value,
            in_interp: interp,
            out_interp: interp,
            in_ease: vec![default_ease; n_ease],
            out_ease: vec![default_ease; n_ease],
            spatial_in: [0.0; 3],
            spatial_out: [0.0; 3],
        });
        cursor = after.get(tag_end + 1..).unwrap_or("");
    }
    keys
}

// ----------------------------------------------------------------------------
// Step 5: Unit & Integration Tests (Synthetic RIFX + AEPX + Local Sample)
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Services, Session};
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Build a RIFX chunk (`[tag: 4B][len: u32 BE][data][pad]`).
    fn leaf_chunk(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + payload.len() + 1);
        out.extend_from_slice(tag);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    /// Build a `LIST` chunk (`["LIST"][len: u32 BE][list_type: 4B][children...]`).
    fn list_chunk(list_type: &[u8; 4], children: &[Vec<u8>]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(list_type);
        for c in children {
            body.extend_from_slice(c);
        }
        leaf_chunk(b"LIST", &body)
    }

    fn tdmn_chunk(match_name: &str) -> Vec<u8> {
        let mut buf = [0u8; 40];
        let bytes = match_name.as_bytes();
        let n = bytes.len().min(39);
        buf[..n].copy_from_slice(&bytes[..n]);
        leaf_chunk(b"tdmn", &buf)
    }

    /// Build a synthetic `.aep` RIFX binary reproducing the structure of `test_simple.aep`.
    fn synthetic_test_simple_aep() -> Vec<u8> {
        let mut idta = [0u8; 84];
        idta[0..2].copy_from_slice(&4u16.to_be_bytes()); // Comp
        idta[16..20].copy_from_slice(&1u32.to_be_bytes()); // item_id = 1

        let mut cdta = [0u8; 204];
        cdta[4..8].copy_from_slice(&1024u32.to_be_bytes()); // divisor = 1024
        cdta[8..12].copy_from_slice(&30720u32.to_be_bytes()); // time_scale = 30 * 1024
        cdta[44..48].copy_from_slice(&92160u32.to_be_bytes()); // duration = 3.0s
        cdta[48..52].copy_from_slice(&30720u32.to_be_bytes());
        cdta[140..142].copy_from_slice(&1080u16.to_be_bytes()); // width = 1080
        cdta[142..144].copy_from_slice(&1920u16.to_be_bytes()); // height = 1920
        cdta[144..148].copy_from_slice(&1u32.to_be_bytes());
        cdta[148..152].copy_from_slice(&1u32.to_be_bytes());
        cdta[156..158].copy_from_slice(&30u16.to_be_bytes()); // 30 fps
        cdta[174..176].copy_from_slice(&180u16.to_be_bytes()); // shutter = 180

        let mut ldta = [0u8; 164];
        ldta[0..4].copy_from_slice(&18u32.to_be_bytes());
        ldta[16..20].copy_from_slice(&30720u32.to_be_bytes());
        ldta[24..28].copy_from_slice(&30720u32.to_be_bytes());
        ldta[28..32].copy_from_slice(&92160u32.to_be_bytes());
        ldta[32..36].copy_from_slice(&30720u32.to_be_bytes());
        ldta[36..40].copy_from_slice(&0x0000_0087u32.to_be_bytes());
        ldta[60..62].copy_from_slice(&0x0008u16.to_be_bytes()); // Shape layer

        // Position stream (4 spatial keyframes)
        let mut pos_tdb4 = [0u8; 124];
        pos_tdb4[2..4].copy_from_slice(&3u16.to_be_bytes());
        pos_tdb4[4..6].copy_from_slice(&0x000eu16.to_be_bytes()); // spatial
        pos_tdb4[12..16].copy_from_slice(&30720u32.to_be_bytes());
        let mut pos_lhd3 = [0u8; 52];
        pos_lhd3[8..12].copy_from_slice(&4u32.to_be_bytes());
        pos_lhd3[16..20].copy_from_slice(&128u32.to_be_bytes());
        let pos_keys_data: [(i32, u8, u8, [f64; 3]); 4] = [
            (0i32, 1u8, 1u8, [896.5714285714286, 976.0, 0.0]),
            (26624i32, 2u8, 2u8, [1070.8721804511278, 487.3270676691729, 0.0]),
            (44032i32, 2u8, 2u8, [587.5599220591502, 443.7605403759668, 0.0]),
            (63488i32, 2u8, 2u8, [544.5714285714286, 720.0, 0.0]),
        ];
        let mut pos_ldat = vec![0u8; 4 * 128];
        for (i, (t, in_i, out_i, xyz)) in pos_keys_data.iter().enumerate() {
            let b = i * 128;
            pos_ldat[b..b + 4].copy_from_slice(&t.to_be_bytes());
            pos_ldat[b + 4] = *in_i;
            pos_ldat[b + 5] = *out_i;
            pos_ldat[b + 32..b + 40].copy_from_slice(&(1.0f64 / 3.0).to_be_bytes());
            pos_ldat[b + 48..b + 56].copy_from_slice(&(1.0f64 / 3.0).to_be_bytes());
            pos_ldat[b + 56..b + 64].copy_from_slice(&xyz[0].to_be_bytes());
            pos_ldat[b + 64..b + 72].copy_from_slice(&xyz[1].to_be_bytes());
            pos_ldat[b + 72..b + 80].copy_from_slice(&xyz[2].to_be_bytes());
        }
        let pos_tdbs = list_chunk(
            b"tdbs",
            &[
                leaf_chunk(b"tdb4", &pos_tdb4),
                list_chunk(b"list", &[leaf_chunk(b"lhd3", &pos_lhd3), leaf_chunk(b"ldat", &pos_ldat)]),
            ],
        );

        // Scale stream (3 non-spatial 3D keyframes, unit_ratio = 0x0001)
        let mut scale_tdb4 = [0u8; 124];
        scale_tdb4[2..4].copy_from_slice(&3u16.to_be_bytes());
        scale_tdb4[4..6].copy_from_slice(&0u16.to_be_bytes());
        scale_tdb4[8..10].copy_from_slice(&1u16.to_be_bytes()); // unit ratio: 1.0 -> 100%
        scale_tdb4[12..16].copy_from_slice(&30720u32.to_be_bytes());
        let mut scale_lhd3 = [0u8; 52];
        scale_lhd3[8..12].copy_from_slice(&3u32.to_be_bytes());
        scale_lhd3[16..20].copy_from_slice(&128u32.to_be_bytes());
        let scale_keys_data = [(0i32, 1.0f64), (19456i32, 0.46f64), (56320i32, 1.2f64)];
        let mut scale_ldat = vec![0u8; 3 * 128];
        for (i, (t, s)) in scale_keys_data.iter().enumerate() {
            let b = i * 128;
            scale_ldat[b..b + 4].copy_from_slice(&t.to_be_bytes());
            scale_ldat[b + 4] = 1;
            scale_ldat[b + 5] = 1;
            scale_ldat[b + 8..b + 16].copy_from_slice(&s.to_be_bytes());
            scale_ldat[b + 16..b + 24].copy_from_slice(&s.to_be_bytes());
            scale_ldat[b + 24..b + 32].copy_from_slice(&1.0f64.to_be_bytes());
        }
        let scale_tdbs = list_chunk(
            b"tdbs",
            &[
                leaf_chunk(b"tdb4", &scale_tdb4),
                list_chunk(b"list", &[leaf_chunk(b"lhd3", &scale_lhd3), leaf_chunk(b"ldat", &scale_ldat)]),
            ],
        );

        // Rotate Z stream (3 non-spatial 1D keyframes)
        let mut rot_tdb4 = [0u8; 124];
        rot_tdb4[2..4].copy_from_slice(&1u16.to_be_bytes());
        rot_tdb4[4..6].copy_from_slice(&0u16.to_be_bytes());
        rot_tdb4[12..16].copy_from_slice(&30720u32.to_be_bytes());
        let mut rot_lhd3 = [0u8; 52];
        rot_lhd3[8..12].copy_from_slice(&3u32.to_be_bytes());
        rot_lhd3[16..20].copy_from_slice(&48u32.to_be_bytes());
        let rot_keys_data = [(0i32, 0.0f64), (26624i32, 42.0f64), (47104i32, -29.0f64)];
        let mut rot_ldat = vec![0u8; 3 * 48];
        for (i, (t, deg)) in rot_keys_data.iter().enumerate() {
            let b = i * 48;
            rot_ldat[b..b + 4].copy_from_slice(&t.to_be_bytes());
            rot_ldat[b + 4] = 1;
            rot_ldat[b + 5] = 1;
            rot_ldat[b + 8..b + 16].copy_from_slice(&deg.to_be_bytes());
        }
        let rot_tdbs = list_chunk(
            b"tdbs",
            &[
                leaf_chunk(b"tdb4", &rot_tdb4),
                list_chunk(b"list", &[leaf_chunk(b"lhd3", &rot_lhd3), leaf_chunk(b"ldat", &rot_ldat)]),
            ],
        );

        let tr_tdgp = list_chunk(
            b"tdgp",
            &[
                tdmn_chunk("ADBE Position"),
                pos_tdbs,
                tdmn_chunk("ADBE Scale"),
                scale_tdbs,
                tdmn_chunk("ADBE Rotate Z"),
                rot_tdbs,
                tdmn_chunk("ADBE Group End"),
            ],
        );

        let root_tdgp = list_chunk(b"tdgp", &[tdmn_chunk("ADBE Transform Group"), tr_tdgp, tdmn_chunk("ADBE Group End")]);
        let layr = list_chunk(b"Layr", &[leaf_chunk(b"ldta", &ldta), leaf_chunk(b"Utf8", b"Shape Layer 1"), root_tdgp]);
        let item = list_chunk(b"Item", &[leaf_chunk(b"idta", &idta), leaf_chunk(b"Utf8", b"Comp 1"), leaf_chunk(b"cdta", &cdta), layr]);
        let fold = list_chunk(b"Fold", &[item]);

        let mut rifx = Vec::new();
        rifx.extend_from_slice(b"RIFX");
        rifx.extend_from_slice(&((4 + fold.len()) as u32).to_be_bytes());
        rifx.extend_from_slice(b"Egg!");
        rifx.extend_from_slice(&fold);
        rifx
    }

    #[test]
    fn parses_synthetic_aep_comp_layers_coordinates_and_keyframes() {
        let bytes = synthetic_test_simple_aep();
        assert!(is_aep_bytes(&bytes));
        let proj = import_aep_bytes(&bytes, "test_simple").expect("synthetic AEP imports");
        let (_, comp) = proj.comps().next().expect("one composition imported");
        assert_eq!((comp.width, comp.height), (1080, 1920));
        assert!((comp.frame_rate.as_f64() - 30.0).abs() < 1e-6);
        assert!((comp.duration.seconds() - 3.0).abs() < 1e-6);
        assert_eq!(comp.layers.len(), 1);

        let layer = &comp.layers[0];
        assert_eq!(layer.name, "Shape Layer 1");
        assert_eq!(layer.source, LayerSource::Shape);

        let pos = layer.props.prop("transform/position").expect("Position property");
        assert_eq!(pos.keys.len(), 4);
        assert!((pos.keys[0].value.as_vec3()[0] - 896.57142857).abs() < 1e-3);
        assert!((pos.keys[0].value.as_vec3()[1] - 976.0).abs() < 1e-3);
        assert_eq!(pos.keys[1].in_interp, Interp::Bezier);
        assert!((pos.keys[3].value.as_vec3()[0] - 544.57142857).abs() < 1e-3);
        assert!((pos.keys[3].value.as_vec3()[1] - 720.0).abs() < 1e-3);

        let scale = layer.props.prop("transform/scale").expect("Scale property");
        assert_eq!(scale.keys.len(), 3);
        assert_eq!(scale.keys[0].value, Value::Vec3([100.0, 100.0, 100.0]));
        assert!((scale.keys[1].value.as_vec3()[0] - 46.0).abs() < 1e-4);
        assert!((scale.keys[2].value.as_vec3()[0] - 120.0).abs() < 1e-4);

        let rot = layer.props.prop("transform/rotation").expect("Rotation property");
        assert_eq!(rot.keys.len(), 3);
        assert_eq!(rot.keys[0].value, Value::Scalar(0.0));
        assert_eq!(rot.keys[1].value, Value::Scalar(42.0));
        assert_eq!(rot.keys[2].value, Value::Scalar(-29.0));
    }

    #[test]
    fn imports_aepx_xml_via_file_open_and_import_project() {
        let aepx = r#"<?xml version="1.0" encoding="UTF-8"?>
<AfterEffectsProject version="1">
  <Composition name="Comp 1" width="1080" height="1920" frameRate="30" duration="3.0">
    <Layer name="Shape Layer 1" type="Shape" inPoint="0.0" outPoint="3.0">
      <Property name="Position">
        <Keyframe time="0.0" value="896.5714, 976.0, 0.0" interp="linear"/>
        <Keyframe frame="26" value="1070.8722, 487.3271, 0.0" interp="bezier"/>
        <Keyframe frame="43" value="587.5599, 443.7605, 0.0" interp="bezier"/>
        <Keyframe frame="62" value="544.5714, 720.0, 0.0" interp="bezier"/>
      </Property>
      <Property name="Scale">
        <Keyframe time="0.0" value="100.0, 100.0, 100.0" interp="linear"/>
        <Keyframe frame="19" value="46.0, 46.0, 100.0" interp="linear"/>
        <Keyframe frame="55" value="120.0, 120.0, 100.0" interp="linear"/>
      </Property>
      <Property name="Rotation">
        <Keyframe time="0.0" value="0.0" interp="linear"/>
        <Keyframe frame="26" value="42.0" interp="linear"/>
        <Keyframe frame="46" value="-29.0" interp="linear"/>
      </Property>
    </Layer>
  </Composition>
</AfterEffectsProject>"#;

        struct Mem(Mutex<HashMap<String, Vec<u8>>>);
        impl Services for Mem {
            fn read_file(&self, p: &str) -> std::io::Result<Vec<u8>> {
                self.0.lock().unwrap().get(p).cloned().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, p))
            }
            fn write_file(&self, p: &str, d: &[u8]) -> std::io::Result<()> {
                self.0.lock().unwrap().insert(p.into(), d.to_vec());
                Ok(())
            }
            fn exists(&self, p: &str) -> bool {
                self.0.lock().unwrap().contains_key(p)
            }
        }

        let mut files = HashMap::new();
        files.insert("tests/test_simple.aepx".to_string(), aepx.as_bytes().to_vec());
        let mut session = Session::new();
        session.services = Arc::new(Mem(Mutex::new(files)));

        session.execute("file.open", json!({"path": "tests/test_simple.aepx"})).expect("file.open tests/test_simple.aepx");
        // Verify protection: s.path must be None (Untitled) so original AEP is never overwritten!
        assert_eq!(session.path, None);
        assert!(session.execute("file.save", json!({})).is_err());
        assert!(session.execute("file.saveAs", json!({"path": "tests/test_simple.aepx"})).is_err());
        assert!(session.execute("file.saveAs", json!({"path": "tests/my_test.aep"})).is_err());

        let (_, comp) = session.project.comps().next().expect("Comp 1 opened");
        assert_eq!((comp.width, comp.height), (1080, 1920));
        assert_eq!(comp.layers.len(), 1);
        assert_eq!(comp.layers[0].props.prop("transform/position").unwrap().keys.len(), 4);
        assert_eq!(comp.layers[0].props.prop("transform/scale").unwrap().keys.len(), 3);
        assert_eq!(comp.layers[0].props.prop("transform/rotation").unwrap().keys.len(), 3);

        // Also test file.importProject merging tests/test_simple.aepx into an existing project
        let imported = session.execute("file.importProject", json!({"path": "tests/test_simple.aepx"})).expect("file.importProject");
        assert_eq!(imported["comps"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn parses_local_test_simple_aep_when_present() {
        let sample_path = std::path::Path::new(r"d:\TLink\ArtCraft\File Test\test_simple.aep");
        if !sample_path.exists() {
            return;
        }
        let bytes = std::fs::read(sample_path).expect("read local test_simple.aep");
        let proj = import_aep_bytes(&bytes, "test_simple").expect("import local test_simple.aep");
        let (&cid, comp) = proj.comps().next().expect("Comp 1 exists");
        assert_eq!(proj.item(cid).unwrap().name, "Comp 1");
        assert_eq!((comp.width, comp.height), (1080, 1920));
        assert!((comp.frame_rate.as_f64() - 30.0).abs() < 1e-6);
        assert!((comp.duration.seconds() - 3.0).abs() < 1e-6);
        assert_eq!(comp.layers.len(), 1);

        let layer = &comp.layers[0];
        assert_eq!(layer.name, "Shape Layer 1");
        assert_eq!(layer.source, LayerSource::Shape);

        let pos = layer.props.prop("transform/position").expect("Position");
        assert_eq!(pos.keys.len(), 4);
        assert!((pos.keys[0].value.as_vec3()[0] - 896.57142857).abs() < 1e-2);
        assert!((pos.keys[0].value.as_vec3()[1] - 976.0).abs() < 1e-2);
        assert!((pos.keys[3].value.as_vec3()[0] - 544.57142857).abs() < 1e-2);
        assert!((pos.keys[3].value.as_vec3()[1] - 720.0).abs() < 1e-2);

        let scale = layer.props.prop("transform/scale").expect("Scale");
        assert_eq!(scale.keys.len(), 3);
        assert!((scale.keys[0].value.as_vec3()[0] - 100.0).abs() < 1e-3);
        assert!((scale.keys[1].value.as_vec3()[0] - 46.0).abs() < 1e-3);
        assert!((scale.keys[2].value.as_vec3()[0] - 120.0).abs() < 1e-3);

        let rot = layer.props.prop("transform/rotation").expect("Rotation");
        assert_eq!(rot.keys.len(), 3);
        assert!((rot.keys[0].value.as_f64() - 0.0).abs() < 1e-3);
        assert!((rot.keys[1].value.as_f64() - 42.0).abs() < 1e-3);
        assert!((rot.keys[2].value.as_f64() - (-29.0)).abs() < 1e-3);

        // Verify the Rectangle 1 shape group inside Contents
        let contents = layer.props.sub("contents").expect("Shape Contents");
        let rect_group = contents.sub("group").expect("Rectangle 1 shape group");
        assert_eq!(rect_group.name, "Rectangle 1");
        let rect_size = rect_group.prop("contents/rect/size").expect("Rectangle Path 1 Size");
        assert!((rect_size.value.as_vec2()[0] - 420.57142857).abs() < 1e-2);
        assert!((rect_size.value.as_vec2()[1] - 429.71428571).abs() < 1e-2);
    }

    #[test]
    fn parses_aepx_with_camera_light_text_footage_and_switches() {
        let aepx = r#"<?xml version="1.0" encoding="UTF-8"?>
<AfterEffectsProject version="1">
  <Folder id="10" name="Assets" />
  <Solid id="20" name="Red Solid" width="1920" height="1080" color="1.0, 0.0, 0.0" parentId="10" />
  <Footage id="30" name="clip.mp4" path="clip.mp4" width="1920" height="1080" duration="5.0" hasVideo="true" hasAudio="true" parentId="10" />
  <Composition id="1" name="Comp 1" width="1920" height="1080" frameRate="30" duration="5.0">
    <!-- Camera Layer with Zoom and POI -->
    <Layer id="1" name="Camera 1" type="Camera" inPoint="0.0" outPoint="5.0">
      <Property name="Position" value="960.0, 540.0, -1500.0" />
      <Property name="Point of Interest" value="960.0, 540.0, 0.0" />
    </Layer>
    <!-- Light Layer -->
    <Layer id="2" name="Light 1" type="Light" inPoint="0.0" outPoint="5.0">
      <Property name="Position" value="700.0, 300.0, -400.0" />
    </Layer>
    <!-- Text Layer with switches and blend mode -->
    <Layer id="3" name="Title" type="Text" text="Hello EffectCraft" fontFamily="Helvetica" fontSize="72.0"
           inPoint="0.0" outPoint="5.0" threeD="true" stretch="100.0" blendMode="11" solo="true">
      <Property name="Position" value="960.0, 540.0, 0.0" />
    </Layer>
    <!-- Footage Layer with Track Matte -->
    <Layer id="4" name="Video Background" type="Footage" sourceId="30"
           inPoint="0.0" outPoint="5.0" trackMatte="1" parentId="3">
      <Property name="Position" value="960.0, 540.0, 0.0" />
    </Layer>
  </Composition>
</AfterEffectsProject>"#;

        let proj = import_aep_bytes(aepx.as_bytes(), "test_full").expect("imports complete aepx");
        let (_, comp) = proj.comps().next().expect("Comp 1 exists");
        assert_eq!(comp.layers.len(), 4);

        // Check camera layer
        let cam_layer = &comp.layers[0];
        assert_eq!(cam_layer.name, "Camera 1");
        assert_eq!(cam_layer.source, LayerSource::Camera);
        assert!(cam_layer.props.sub("cameraOptions").is_some());

        // Check light layer
        let light_layer = &comp.layers[1];
        assert_eq!(light_layer.name, "Light 1");
        assert!(matches!(light_layer.source, LayerSource::Light { .. }));
        assert!(light_layer.props.sub("lightOptions").is_some());

        // Check text layer
        let text_layer = &comp.layers[2];
        assert_eq!(text_layer.name, "Title");
        assert_eq!(text_layer.source, LayerSource::Text);
        assert!(text_layer.switches.three_d);
        assert!(text_layer.switches.solo);
        assert_eq!(text_layer.blend_mode, BlendMode::Screen); // blend mode 11 = Screen
        let text_prop = text_layer.props.prop("text/sourceText").expect("sourceText property");
        if let Value::Text(doc) = &text_prop.value {
            assert_eq!(doc.text, "Hello EffectCraft");
            assert_eq!(doc.font, "Helvetica");
            assert_eq!(doc.size, 72.0);
        } else {
            panic!("Expected Value::Text");
        }

        // Check footage layer
        let footage_layer = &comp.layers[3];
        assert_eq!(footage_layer.name, "Video Background");
        assert!(matches!(footage_layer.source, LayerSource::Footage { .. }));
        assert_eq!(footage_layer.parent, Some(text_layer.id));
        assert!(footage_layer.track_matte.is_some());
        assert_eq!(footage_layer.track_matte.unwrap().kind, MatteKind::Alpha);
        assert_eq!(footage_layer.track_matte.unwrap().layer, text_layer.id);
    }

    #[test]
    fn parses_aepx_with_polystar_path_gradients_trim_and_repeater() {
        let aepx = r#"<?xml version="1.0" encoding="UTF-8"?>
<AfterEffectsProject version="1">
  <Composition id="1" name="Comp Shapes" width="1920" height="1080" frameRate="30" duration="5.0">
    <!-- Star Layer with Trim Paths -->
    <Layer id="1" name="Star Layer" type="Shape" inPoint="0.0" outPoint="5.0">
      <Polystar type="star" points="5" outerRadius="150" innerRadius="70" />
      <TrimPaths start="10" end="90" offset="45" />
    </Layer>
    <!-- Polygon Layer with Repeater -->
    <Layer id="2" name="Hexagon Repeater" type="Shape" inPoint="0.0" outPoint="5.0">
      <Polystar type="polygon" points="6" outerRadius="80" />
      <Repeater copies="4" />
    </Layer>
    <!-- Custom Bezier Path Layer -->
    <Layer id="3" name="Custom Path" type="Shape" inPoint="0.0" outPoint="5.0">
      <Path vertices="0,0 100,50 50,150" closed="true" />
    </Layer>
  </Composition>
</AfterEffectsProject>"#;

        let proj = import_aep_bytes(aepx.as_bytes(), "test_shapes").expect("imports shape aepx");
        let (_, comp) = proj.comps().next().expect("Comp Shapes exists");
        assert_eq!(comp.layers.len(), 3);

        // Check Star Layer
        let star_layer = &comp.layers[0];
        assert_eq!(star_layer.name, "Star Layer");
        let contents = star_layer.props.sub("contents").expect("contents group");
        let group = contents.sub("group").expect("shape group");
        assert!(group.group("contents/star").is_some());
        assert!(group.group("contents/trim").is_some());

        // Check Hexagon Repeater Layer
        let hex_layer = &comp.layers[1];
        let hex_contents = hex_layer.props.sub("contents").expect("contents group");
        let hex_group = hex_contents.sub("group").expect("shape group");
        assert!(hex_group.group("contents/star").is_some());
        assert!(hex_group.group("contents/repeater").is_some());

        // Check Custom Path Layer
        let path_layer = &comp.layers[2];
        let path_contents = path_layer.props.sub("contents").expect("contents group");
        let path_group = path_contents.sub("group").expect("shape group");
        assert!(path_group.group("contents/path").is_some());
    }

    #[test]
    fn parses_aepx_with_masks_effects_expressions_and_markers() {
        let aepx = r#"<?xml version="1.0" encoding="UTF-8"?>
<AfterEffectsProject version="1">
  <Composition id="1" name="Comp FX" width="1920" height="1080" frameRate="30" duration="10.0">
    <Marker time="1.0" duration="0.0" comment="Intro Marker" />
    <Layer id="1" name="Hero Layer" type="Shape" inPoint="0.0" outPoint="10.0">
      <Marker time="2.5" duration="0.5" comment="Action Hit" />
      <Property name="Position" value="960, 540" expr="wiggle(5, 20)" />
      <Mask name="Subject Mask" mode="Subtract" inverted="true" feather="50, 50" opacity="80" expansion="-10">
        <Path vertices="0,0 200,0 200,200 0,200" closed="true" />
      </Mask>
      <Effect name="Gaussian Blur" matchName="ADBE Gaussian Blur 2" enabled="true">
        <Property name="Blurriness" matchName="ADBE Gaussian Blur 2-0001" value="25" />
      </Effect>
      <Effect name="Custom Plugin" matchName="ADBE Custom Plugin" enabled="true">
        <Property name="Amount" matchName="ADBE Amount" value="75" expr="time * 10" />
      </Effect>
    </Layer>
  </Composition>
</AfterEffectsProject>"#;

        let proj = import_aep_bytes(aepx.as_bytes(), "test_fx").expect("imports fx aepx");
        let (_, comp) = proj.comps().next().expect("Comp FX exists");

        // Check Comp Marker
        assert_eq!(comp.markers.len(), 1);
        assert_eq!(comp.markers[0].comment, "Intro Marker");
        assert_eq!(comp.markers[0].time, seconds_to_tick(1.0, comp.frame_rate));

        // Check Hero Layer
        assert_eq!(comp.layers.len(), 1);
        let layer = &comp.layers[0];
        assert_eq!(layer.name, "Hero Layer");

        // Check Layer Marker
        assert_eq!(layer.markers.len(), 1);
        assert_eq!(layer.markers[0].comment, "Action Hit");
        assert_eq!(layer.markers[0].time, seconds_to_tick(2.5, comp.frame_rate));

        // Check Transform Expression
        let pos_prop = layer.props.prop("transform/position").expect("position property");
        assert!(pos_prop.expr.is_some());
        assert_eq!(pos_prop.expr.as_ref().expect("has expr").text, "wiggle(5, 20)");

        // Check Mask
        let masks_grp = layer.props.sub("masks").expect("masks group exists");
        assert_eq!(masks_grp.children.len(), 1);
        let mask_node = &masks_grp.children[0];
        let mask = mask_node.as_group().expect("mask group");
        assert_eq!(mask.name, "Subject Mask");
        if let GroupKind::Mask { mode, inverted, .. } = mask.kind {
            assert_eq!(mode, MaskMode::Subtract);
            assert!(inverted);
        } else {
            panic!("Expected GroupKind::Mask");
        }
        let feather = mask.get("feather").expect("mask feather");
        assert_eq!(feather.value, Value::Vec2([50.0, 50.0]));
        let opacity = mask.get("opacity").expect("mask opacity");
        assert_eq!(opacity.value, Value::Scalar(80.0));
        let expansion = mask.get("expansion").expect("mask expansion");
        assert_eq!(expansion.value, Value::Scalar(-10.0));

        // Check Effects
        let fx_grp = layer.props.sub("effects").expect("effects group exists");
        assert_eq!(fx_grp.children.len(), 2);

        // Gaussian Blur (instantiated from effectcraft-effects)
        let blur_node = &fx_grp.children[0];
        let blur_fx = blur_node.as_group().expect("blur group");
        assert_eq!(blur_fx.name, "Gaussian Blur");
        let blur_prop = blur_fx.prop("blurriness").or_else(|| blur_fx.get("blurriness")).expect("blurriness prop");
        assert_eq!(blur_prop.value, Value::Scalar(25.0));

        // Custom Plugin (generic effect group)
        let custom_node = &fx_grp.children[1];
        let custom_fx = custom_node.as_group().expect("custom plugin group");
        assert_eq!(custom_fx.name, "Custom Plugin");
        let amt_prop = custom_fx.get("ADBE Amount").or_else(|| custom_fx.prop("Amount")).expect("amount prop");
        assert_eq!(amt_prop.value, Value::Scalar(75.0));
        assert!(amt_prop.expr.is_some());
        assert_eq!(amt_prop.expr.as_ref().expect("has expr").text, "time * 10");
    }

    #[test]
    fn test_resolve_footage_path_priorities_and_fallbacks() {
        use std::sync::Mutex;
        struct TestMem(Mutex<HashMap<String, Vec<u8>>>);
        impl Services for TestMem {
            fn read_file(&self, p: &str) -> std::io::Result<Vec<u8>> {
                self.0.lock().unwrap().get(p).cloned().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, p))
            }
            fn write_file(&self, p: &str, d: &[u8]) -> std::io::Result<()> {
                self.0.lock().unwrap().insert(p.into(), d.to_vec());
                Ok(())
            }
            fn exists(&self, p: &str) -> bool {
                self.0.lock().unwrap().contains_key(p)
            }
        }

        let mut files = HashMap::new();
        // 1. Exact path exists
        files.insert(r"\\Ot-server\e\Assets\logo.png".to_string(), vec![1]);
        // 2. macOS /Volumes mount exists
        files.insert("/Volumes/e/Assets/video.mov".to_string(), vec![2]);
        // 3. Project-relative folder exists
        files.insert("/projects/myproj/Footer/Comp 1.mov".to_string(), vec![3]);
        let mem = TestMem(Mutex::new(files));

        // Priority 1: Exact path remains untouched
        let mut p1 = r"\\Ot-server\e\Assets\logo.png".to_string();
        assert!(resolve_footage_path(&mut p1, Some(std::path::Path::new("/projects/myproj")), &mem));
        assert_eq!(p1, r"\\Ot-server\e\Assets\logo.png");

        // Priority 2: Cross-platform UNC -> /Volumes/
        let mut p2 = r"\\Ot-server\e\Assets\video.mov".to_string();
        assert!(resolve_footage_path(&mut p2, Some(std::path::Path::new("/projects/myproj")), &mem));
        assert_eq!(p2, "/Volumes/e/Assets/video.mov");

        // Priority 3: Project-relative suffix fallback
        let mut p3 = r"\\Old-Server\d\Work\Footer\Comp 1.mov".to_string();
        assert!(resolve_footage_path(&mut p3, Some(std::path::Path::new("/projects/myproj")), &mem));
        assert_eq!(p3, "/projects/myproj/Footer/Comp 1.mov");

        // Missing file: retains original path, returns false
        let mut p4 = r"\\Ot-server\e\Assets\missing.mp4".to_string();
        assert!(!resolve_footage_path(&mut p4, Some(std::path::Path::new("/projects/myproj")), &mem));
        assert_eq!(p4, r"\\Ot-server\e\Assets\missing.mp4");
    }
}
