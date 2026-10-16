# `effectcraft-engine`

The EffectCraft engine façade (`L4`), command registry, session state, and project importers.

## After Effects Project Importer (`src/aep.rs`)

Per `AGENTS.md` §2 (Owner Decision 2026-10-16) and `docs/gaps.md` G4, the `.aep` (binary RIFX) and `.aepx` (XML) importer in [`src/aep.rs`](src/aep.rs) was implemented clean-room by observing only the byte layout of contributor-authored After Effects sample files (`test_simple.aep`):

| Chunk / Tag | Observed Offset / Size | Meaning |
|---|---|---|
| `RIFX` ... `Egg!` | `0..12` (12 bytes) | Big-endian RIFF container header (`"RIFX"` at `0..4`, `u32` BE payload size at `4..8`, form `"Egg!"` at `8..12`). |
| `LIST Fold` / `LIST Sfdr` | Variable | Project folder hierarchy containing `LIST Item` entries. |
| `idta` | 84 bytes (`@536`, `@2006`) | Item header: `u16` BE at `0..2` = item kind (`1` = Folder, `4` = Composition, `7` = Footage/Solid); `u32` BE at `16..20` = item ID. |
| `Utf8` | Variable | UTF-8 display name of the enclosing item or layer. |
| `LIST Pin ` → `sspc` / `opti` | 222B / 282B (`@818`, `@1056`) | Solid/footage metadata (`"Soli"` tag at `sspc[22..26]` and `opti[0..4]`, `u32` BE width/height at `sspc[30..38]`, `f32` BE ARGB colour at `opti[10..26]`, fallback name at `opti[26..]`). |
| `cdta` | 204 bytes (`@2136`) | Composition header: `u32` BE divisor at `4..8` (`1024`), `u32` BE `time_scale` at `8..12` (`30720`), `u32` BE duration at `44..48` (`92160` = `3.0`s), RGB background at `52..55`, `u16` BE `width` at `140..142` (`1080`), `u16` BE `height` at `142..144` (`1920`), `u32` BE PAR num/den at `144..152`, `u16` BE nominal fps at `156..158` (`30`), `u16` BE shutter angle at `174..176` (`180`). |
| `LIST Layr` | Variable (`@2512`) | User timeline layer (`LIST CLay` cameras and `LIST SecL` marker layers are skipped). |
| `ldta` | 164 bytes (`@2524`) | Layer header: `u32` BE layer ID at `0..4`, `i32`/`u32` BE `start_time`, `in_point`, `out_point` + `time_scale` at `12..36`, `u32` BE switches at `36..40`, `u32` BE source item ID at `56..60`, `u16` BE layer type flags at `60..62` (`0x0008` = Shape layer), `u32` BE parent layer ID at `108..112`. |
| `LIST tdgp` / `tdmn` | Variable / 40 bytes | Property group and null-padded ASCII match name (`"ADBE Transform Group"`, `"ADBE Root Vectors Group"`, `"ADBE Group End"`). |
| `LIST tdbs` → `tdb4` | 124 bytes (`@6660`, `@8090`, `@9674`) | Property stream header: `u16` BE dimensions at `2..4`, `u16` BE spatial flag (`& 0x0002`) at `4..6`, `u16` BE unit-ratio flag (`0x0001` = `1.0` → `100%`) at `8..10`, `u32` BE `time_scale` at `12..16`. |
| `cdat` | `dim × 8` bytes | Static `f64` BE property value components. |
| `LIST list` → `lhd3` + `ldat` | 52B + `key_count × key_stride` | Keyframe table: `u32` BE `key_count` at `lhd3[8..12]`, `u32` BE `key_stride` at `lhd3[16..20]`. Spatial records (`128`B) store `i32` BE time at `0..4`, `u8` in/out interpolation at `4..6`, `f64` BE speeds/influences at `16..56`, `3 × f64` BE `[x, y, z]` at `56..80`, and spatial tangents at `80..128`. Non-spatial records store `dim × f64` BE values at `8..8+dim*8` followed by per-dimension speeds and influences. |
