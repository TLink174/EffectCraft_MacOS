# Báo Cáo Khảo Sát Kiến Trúc Import / IO / Data Model & Kế Hoạch AEP Importer

> Tài liệu khảo sát mã nguồn EffectCraft phục vụ việc tích hợp và mở rộng bộ Importer Adobe After Effects (`.aep` / `.aepx`).

---

## 1. Crate chịu trách nhiệm xử lý File Import / IO / Serialization

Kiến trúc EffectCraft phân chia trách nhiệm File Import / IO theo mô hình đa tầng:

### A. Tầng Quản lý Session & Thực thi Command (`crates/engine` - `effectcraft-engine`)
Là tầng tiếp nhận trực tiếp các tương tác từ UI (Menu File, Phím tắt, Kéo-thả) hoặc qua Control Protocol (JSON-RPC/MCP):
* **`crates/engine/src/commands/file.rs`**: 
  * `file.open`: Mở file project (JSON `.ecproj`, XML `.ecprojx`, hoặc nhị phân After Effects `.aep` / `.aepx`).
  * `file.import`: Import tài nguyên (media footage, vector, project khác).
  * `file.save`, `file.saveAs`: Lưu project hiện tại.
* **`crates/engine/src/commands/import_project.rs`**:
  * `file.importProject`: Gộp các Composition, Solid, Footage từ một file project khác vào project đang mở.
* **`crates/engine/src/aep.rs`**:
  * Parser nhị phân RIFX (`Egg!`) và XML `<AfterEffectsProject>` của After Effects, chuyển đổi trực tiếp sang `Project` của EffectCraft.
* **`crates/engine/src/media_browser.rs`**:
  * Quét thư mục ổ đĩa, nhận diện và phân loại định dạng file (video, audio, image, project).
* **`crates/engine/src/footage_check.rs`**:
  * Kiểm tra và tự động relink các file footage/media bị thiếu khi mở project.

### B. Tầng Document Model & Serialization Core (`crates/project` - `effectcraft-project`)
* **`crates/project/src/lib.rs`**: Định nghĩa struct gốc `Project`, quản lý bảng items (`ItemId`), cấu hình màu sắc, render queue.
* **`crates/project/src/json.rs`** (hoặc `Project::from_json_repaired` / `to_json`): Serialization và deserialization project ra JSON schema version 2 (tự động repair nếu file bị lỗi hoặc vượt giới hạn).
* **`crates/engine/src/xml_project.rs`**: Bộ chuyển đổi qua lại giữa định dạng JSON và XML (`.ecprojx`).

### C. Các Crate Format Importer chuyên biệt
* **`crates/psd` (`effectcraft-psd`)**: Clean-room parser đọc file Photoshop `.psd` / `.psb` (Layer, Group, Mask, Blend Mode, Text, Effects).
* **`crates/svg` (`effectcraft-svg`)**: Đọc tài nguyên vector SVG thành Shape Layer.
* **`crates/pdf` (`effectcraft-pdf`)**: Đọc file PDF vector qua thư viện `craft-pdf`.
* **`crates/lottie` (`effectcraft-lottie`)**: Import và export hoạt cảnh Lottie JSON.
* **`crates/interchange` (`effectcraft-interchange`)**: Đọc và xuất các định dạng Timeline dựng phim (Final Cut Pro FCPXML, Premiere Pro EDL, FCP7 XML, OpenTimelineIO).
* **`crates/media` (`effectcraft-media`)**: Tách container (demux) và giải mã (decode) audio/video/ảnh (MP4, QuickTime MOV, WebM, WAV, MP3, PNG, JPEG, OpenEXR, GIF) dựa trên `filmcraft-media`, `filmcraft-codecs`, `image`, `exr`.
* **`crates/export` (`effectcraft-export`)**: Mã hóa và xuất video (H.264, ProRes, VP9, HEVC, AV1).

---

## 2. Các Crate đọc nhị phân trong Cargo.toml

### Trạng thái trong `Cargo.toml`
* **Đã có sẵn:**
  * **`bytemuck = "1"`** (kèm `bytemuck_derive = "1"`, dòng 98 trong `Cargo.toml`): Dùng để ép kiểu (zero-copy casting / transmutation an toàn) giữa raw byte slices và các struct dữ liệu đồ họa (được chia sẻ giữa `crates/gpu`, `crates/render`, và `wgpu`).
* **Hoàn toàn KHÔNG có:**
  * Các crate parser nhị phân bên ngoài như **`byteorder`**, **`nom`**, **`binrw`** **không có mặt** trong `Cargo.toml`.

### Triết lý thiết kế Pure Rust & Tiêu chuẩn "Never Crash"
Theo quy tắc kiến trúc của dự án (`AGENTS.md` §2 & `CLAUDE.md`):
1. **Không phụ thuộc thư viện phân tích nhị phân nặng:** Dự án tự xây dựng Big-Endian / Little-Endian zero-copy byte reader bằng các hàm nguyên bản an toàn của Rust:
   * `u16::from_be_bytes(data.get(offset..offset+2)?.try_into().ok()?)`
   * `u32::from_be_bytes(data.get(offset..offset+4)?.try_into().ok()?)`
   * `i32::from_be_bytes(data.get(offset..offset+4)?.try_into().ok()?)`
   * `f32::from_bits(u32::from_be_bytes(...))`
   * `f64::from_bits(u64::from_be_bytes(...))`
2. **Minh chứng thực tế trong codebase:**
   * Struct `Reader<'a>` trong [`crates/psd/src/read.rs`](crates/psd/src/read.rs): Một zero-copy big-endian reader tự viết dài chưa đầy 100 dòng code.
   * Module [`crates/engine/src/aep.rs`](crates/engine/src/aep.rs): Tự duyệt cấu trúc RIFX chunk cây phân cấp kết hợp giới hạn đệ quy `MAX_DEPTH = 64` và giới hạn kích thước phân bổ `MAX_KEYS` để chống crash bộ nhớ.

---

## 3. Cấu trúc Dữ liệu Nội bộ biểu diễn Scene / Timeline / Layer / Keyframe

Toàn bộ dữ liệu đồ họa và hoạt ảnh của EffectCraft được tổ chức theo cấu trúc phân tầng rõ ràng:

### A. Document Root: `Project`
* **File:** [`crates/project/src/lib.rs`](crates/project/src/lib.rs) (dòng 1010–1100)
* **Định nghĩa:**
  ```rust
  pub struct Project {
      pub items: BTreeMap<ItemId, Item>,
      pub settings: ProjectSettings,
      pub render_queue: RenderQueue,
      pub next_id: u64,
  }
  ```
* **Phân loại Item:** `ItemKind::Folder`, `ItemKind::Comp(Arc<Comp>)`, `ItemKind::Solid(Solid)`, `ItemKind::Footage(Footage)`.

### B. Scene / Composition: `Comp`
* **File:** [`crates/project/src/lib.rs`](crates/project/src/lib.rs) (dòng 406–465)
* **Định nghĩa:**
  ```rust
  pub struct Comp {
      pub width: u32,
      pub height: u32,
      pub pixel_aspect: f64,
      pub frame_rate: FrameRate,
      pub duration: Tick,
      pub display_start: Tick,
      pub background: [f32; 3],
      pub work_area: (Tick, Tick),
      pub layers: Vec<Layer>,          // Layer #1 nằm ở đỉnh stack (index 0)
      pub markers: Vec<Marker>,
      pub shutter_angle: f64,
      pub motion_blur_samples: u32,
      pub renderer: Renderer,          // Classic3D hoặc Advanced3D
      pub hide_shy: bool,
      pub enable_motion_blur: bool,
      pub resolution: Resolution,
  }
  ```

### C. Timeline & Đơn vị Thời gian: `Tick`, `FrameRate`, `TimeRange`
* **File:** [`crates/time/src/lib.rs`](crates/time/src/lib.rs) (dòng 25–160)
* **Định nghĩa:**
  * **`Tick(pub i64)`**: Đơn vị thời gian nguyên tuyệt đối với độ phân giải `TICKS_PER_SECOND = 1_411_200_000` ticks/giây (bội chung nhỏ nhất của 24, 25, 30, 48, 50, 60, 120, 1000 và 1001), loại bỏ hoàn toàn sai số tích lũy của dấu phẩy động (`f64`).
  * **`FrameRate { pub num: i64, pub den: i64 }`**: Tỷ số khung hình chính xác (ví dụ `30/1`, `24000/1001` cho 23.976 fps, `30000/1001` cho 29.97 fps).
  * **`TimeRange { pub start: Tick, pub duration: Tick }`**: Khoảng thời gian trên timeline.

### D. Layer: `Layer` & `LayerSource`
* **File:** [`crates/project/src/lib.rs`](crates/project/src/lib.rs) (dòng 968–1005)
* **Định nghĩa:**
  ```rust
  pub struct Layer {
      pub id: LayerId,
      pub name: String,
      pub source: LayerSource,
      pub label: Label,
      pub comment: String,
      pub start_time: Tick,            // Thời điểm 0 của layer trên timeline comp
      pub in_point: Tick,              // Điểm vào (In Point)
      pub out_point: Tick,             // Điểm ra (Out Point)
      pub stretch: f64,                // Tỷ lệ co giãn thời gian (100% = bình thường)
      pub switches: Switches,          // video, audio, solo, locked, shy, 3D, motion_blur...
      pub blend_mode: BlendMode,       // Normal, Multiply, Screen, Overlay, Add...
      pub track_matte: Option<TrackMatte>, // Alpha, Alpha Inverted, Luma, Luma Inverted
      pub parent: Option<LayerId>,     // Layer cha (Parenting)
      pub markers: Vec<Marker>,
      pub auto_orient: AutoOrient,
      pub props: PropGroup,            // Cây thuộc tính Transform, Contents, Effects
  }
  ```
* **Nguồn Layer (`LayerSource` - dòng 653–684):** `Footage`, `Comp` (Precomp), `Solid`, `Text`, `Shape`, `Null`, `Camera`, `Light`, `Model`, `Primitive`.

### E. Cây Thuộc Tính (Properties): `PropGroup`, `Property`, `Node`
* **File:** [`crates/project/src/props.rs`](crates/project/src/props.rs) (dòng 40–120)
* **Định nghĩa:**
  * **`PropGroup`**: Nhóm thuộc tính phân cấp (`transform`, `contents`, `effects`, `masks`), chứa danh sách các `Node`.
  * **`Property`**: Một thuộc tính hoạt ảnh đơn lẻ, chứa:
    * `value: Value`: Giá trị tĩnh hiện tại.
    * `keys: Vec<Keyframe>`: Danh sách keyframe hoạt ảnh.
    * `expression: Option<Expression>`: Biểu thức JavaScript/Boa tính toán tự động.

### F. Keyframe & Animation: `Keyframe`, `Value`, `Ease`, `Interp`
* **File:** [`crates/keyframe/src/lib.rs`](crates/keyframe/src/lib.rs) (dòng 59–92)
* **Định nghĩa:**
  ```rust
  pub struct Keyframe {
      pub time: Tick,                  // Thời điểm đặt keyframe
      pub value: Value,                // Giá trị tại keyframe
      pub in_interp: Interp,           // Linear, Bezier, Hold
      pub out_interp: Interp,          // Linear, Bezier, Hold
      pub in_ease: Vec<Ease>,          // Speed và Influence chiều vào (0..1)
      pub out_ease: Vec<Ease>,         // Speed và Influence chiều ra (0..1)
      pub continuous: bool,            // Giữ đường cong thời gian liên tục
      pub auto_bezier: bool,           // Tự động tính tiếp tuyến trơn tru
      pub spatial_in: [f64; 3],        // Tiếp tuyến không gian In (cho Position/Anchor)
      pub spatial_out: [f64; 3],       // Tiếp tuyến không gian Out
      pub spatial_auto: bool,          // Tự động tính đường cong không gian
      pub roving: bool,                // Keyframe trôi nổi theo thời gian (Roving)
  }
  ```
* **Kiểu dữ liệu `Value`:** Hỗ trợ `Scalar(f64)`, `Vec2([f64; 2])`, `Vec3([f64; 3])`, `Color([f64; 4])`, `Path(BezierPath)`.

---

## 4. Bảng Tổng Hợp Vị Trí File Mã Nguồn

| Thành phần | Crate | Đường dẫn file mã nguồn |
| :--- | :--- | :--- |
| **Workspace Config** | Root | `Cargo.toml` |
| **Document Root (`Project`)** | `effectcraft-project` | `crates/project/src/lib.rs` |
| **Scene / Comp (`Comp`)** | `effectcraft-project` | `crates/project/src/lib.rs`, `crates/project/src/comp.rs` |
| **Timeline Time (`Tick`, `FrameRate`)** | `effectcraft-time` | `crates/time/src/lib.rs` |
| **Layer (`Layer`, `LayerSource`)** | `effectcraft-project` | `crates/project/src/lib.rs` |
| **Property Group & Property** | `effectcraft-project` | `crates/project/src/props.rs` |
| **Keyframe & Ease (`Keyframe`, `Value`)** | `effectcraft-keyframe` | `crates/keyframe/src/lib.rs` |
| **AEP Importer (`.aep` / `.aepx`)** | `effectcraft-engine` | `crates/engine/src/aep.rs` |
| **File Commands (`file.open`, `import`)** | `effectcraft-engine` | `crates/engine/src/commands/file.rs` |
| **Project Import Command** | `effectcraft-engine` | `crates/engine/src/commands/import_project.rs` |
| **PSD Importer & Binary Reader** | `effectcraft-psd` | `crates/psd/src/lib.rs`, `crates/psd/src/read.rs` |
