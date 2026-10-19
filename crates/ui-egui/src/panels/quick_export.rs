//! Quick Export modal dialog (`File ▸ Export ▸ Quick Export...`).
//!
//! Provides a streamlined export workflow similar to Premiere Pro and After Effects:
//! - Direct presets (Match Source High/Medium, 4K, Social, WebM, ProRes)
//! - Explicit video bitrate control (Mbps slider, manual entry, quick chips)
//! - Audio bitrate selection (AAC 96–320 kbps, Opus 64–256 kbps)
//! - Live estimated file size and duration display
//! - One-click "Export" or "Send to Render Queue"

use crate::EffectcraftApp;
use crate::i18n::tr;
use crate::theme::Tokens;
use effectcraft_engine::project::ItemId;
use effectcraft_project::render_queue::OutputFormat;
use egui::{Align2, Color32, pos2, vec2};
use serde_json::json;

#[derive(Clone, Debug, PartialEq)]
pub struct QuickExportDraft {
    pub comp_id: Option<ItemId>,
    pub output_path: String,
    pub format: OutputFormat,
    pub preset_idx: usize,
    pub video_bitrate_mbps: f64,
    pub aac_bitrate_kbps: u32,
    pub opus_bitrate_kbps: u32,
}

impl Default for QuickExportDraft {
    fn default() -> Self {
        Self {
            comp_id: None,
            output_path: String::new(),
            format: OutputFormat::H264,
            preset_idx: 0,
            video_bitrate_mbps: 15.0,
            aac_bitrate_kbps: 192,
            opus_bitrate_kbps: 128,
        }
    }
}

pub struct QuickExportPreset {
    pub name: &'static str,
    pub desc: &'static str,
    pub format: OutputFormat,
    pub video_bitrate_mbps: f64,
    pub aac_bitrate_kbps: u32,
    pub opus_bitrate_kbps: u32,
}

pub const PRESETS: &[QuickExportPreset] = &[
    QuickExportPreset {
        name: "Match Source - High Bitrate",
        desc: "15 Mbps Video, 192 kbps AAC Audio (H.264 MP4)",
        format: OutputFormat::H264,
        video_bitrate_mbps: 15.0,
        aac_bitrate_kbps: 192,
        opus_bitrate_kbps: 128,
    },
    QuickExportPreset {
        name: "Match Source - Medium Bitrate",
        desc: "8 Mbps Video, 160 kbps AAC Audio (H.264 MP4)",
        format: OutputFormat::H264,
        video_bitrate_mbps: 8.0,
        aac_bitrate_kbps: 160,
        opus_bitrate_kbps: 128,
    },
    QuickExportPreset {
        name: "Web / Social (1080p)",
        desc: "4 Mbps Video, 128 kbps AAC Audio (H.264 MP4)",
        format: OutputFormat::H264,
        video_bitrate_mbps: 4.0,
        aac_bitrate_kbps: 128,
        opus_bitrate_kbps: 128,
    },
    QuickExportPreset {
        name: "High Quality (4K)",
        desc: "40 Mbps Video, 256 kbps AAC Audio (HEVC MP4)",
        format: OutputFormat::Hevc,
        video_bitrate_mbps: 40.0,
        aac_bitrate_kbps: 256,
        opus_bitrate_kbps: 160,
    },
    QuickExportPreset {
        name: "WebM Video",
        desc: "6 Mbps Video, 128 kbps Opus Audio (WebM)",
        format: OutputFormat::WebM,
        video_bitrate_mbps: 6.0,
        aac_bitrate_kbps: 192,
        opus_bitrate_kbps: 128,
    },
    QuickExportPreset {
        name: "Apple ProRes 422",
        desc: "ProRes 422, Uncompressed PCM Audio (QuickTime MOV)",
        format: OutputFormat::ProRes,
        video_bitrate_mbps: 150.0,
        aac_bitrate_kbps: 192,
        opus_bitrate_kbps: 128,
    },
    QuickExportPreset {
        name: "Custom...",
        desc: "Custom video and audio bitrate",
        format: OutputFormat::H264,
        video_bitrate_mbps: 15.0,
        aac_bitrate_kbps: 192,
        opus_bitrate_kbps: 128,
    },
];

pub fn open(app: &mut EffectcraftApp, params: &serde_json::Value) -> Result<(), String> {
    let cid = params
        .get("comp")
        .and_then(serde_json::Value::as_u64)
        .map(ItemId)
        .or_else(|| app.session.active_comp_id())
        .ok_or_else(|| "No composition is open to export".to_string())?;

    let comp_name = app.session.project.item(cid).map(|i| i.name.clone()).unwrap_or_else(|| "Comp 1".to_string());
    let draft = &mut app.dialog_state.quick_export;
    draft.comp_id = Some(cid);

    if let Some(preset_name) = params.get("preset").and_then(serde_json::Value::as_str) {
        if let Some((idx, _)) = PRESETS.iter().enumerate().find(|(_, p)| p.name.eq_ignore_ascii_case(preset_name)) {
            apply_preset(draft, idx);
        }
    }

    if let Some(format_str) = params.get("format").and_then(serde_json::Value::as_str) {
        if let Some(f) = OutputFormat::from_name(format_str) {
            draft.format = f;
            draft.preset_idx = PRESETS.len() - 1; // Custom
        }
    }

    if let Some(v_bps) = params.get("videoBitrate").and_then(serde_json::Value::as_f64) {
        draft.video_bitrate_mbps = (v_bps / 1_000_000.0).clamp(0.1, 500.0);
        draft.preset_idx = PRESETS.len() - 1;
    }

    if let Some(a_kbps) = params.get("aacBitrate").and_then(serde_json::Value::as_u64) {
        draft.aac_bitrate_kbps = (a_kbps as u32).clamp(32, 512);
        draft.preset_idx = PRESETS.len() - 1;
    }

    if let Some(path) = params.get("path").or_else(|| params.get("output")).and_then(serde_json::Value::as_str) {
        draft.output_path = path.to_string();
    } else if draft.output_path.is_empty() || draft.comp_id != Some(cid) {
        let base_dir = app.session.path.as_ref().and_then(|p| std::path::Path::new(p).parent()).map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let ext = draft.format.extension();
        let fname = format!("{comp_name}.{ext}");
        if !base_dir.is_empty() {
            draft.output_path = format!("{base_dir}/{fname}");
        } else {
            draft.output_path = fname;
        }
    }

    app.dialog = Some(crate::Dialog::QuickExport);
    Ok(())
}

fn apply_preset(draft: &mut QuickExportDraft, idx: usize) {
    if let Some(preset) = PRESETS.get(idx) {
        draft.preset_idx = idx;
        if idx < PRESETS.len() - 1 {
            draft.format = preset.format;
            draft.video_bitrate_mbps = preset.video_bitrate_mbps;
            draft.aac_bitrate_kbps = preset.aac_bitrate_kbps;
            draft.opus_bitrate_kbps = preset.opus_bitrate_kbps;
            update_extension(&mut draft.output_path, preset.format.extension());
        }
    }
}

fn update_extension(path: &mut String, new_ext: &str) {
    if path.is_empty() {
        *path = format!("Export.{new_ext}");
        return;
    }
    if let Some(dot) = path.rfind('.') {
        if let Some(slash) = path.rfind(['/', '\\']) {
            if dot > slash {
                path.truncate(dot);
            }
        } else {
            path.truncate(dot);
        }
    }
    path.push('.');
    path.push_str(new_ext);
}

pub fn show(app: &mut EffectcraftApp, ctx: &egui::Context, t: &Tokens) {
    let mut close = false;
    let mut execute_action: Option<(bool, serde_json::Value)> = None;

    let cid = match app.dialog_state.quick_export.comp_id.or_else(|| app.session.active_comp_id()) {
        Some(id) => id,
        None => {
            app.dialog = None;
            return;
        }
    };

    let comp = match app.session.project.comp(cid) {
        Some(c) => c.clone(),
        None => {
            app.dialog = None;
            return;
        }
    };

    let comp_name = app.session.project.item(cid).map(|i| i.name.clone()).unwrap_or_else(|| "Composition".to_string());
    let dur_sec = comp.duration.seconds().max(0.0);
    let auto = &mut app.auto;

    super::dialogs::modal_fit(ctx, "Quick Export", vec2(580.0, 530.0), t, |ui, height| {
        let (mut body, _) = super::dialogs::above_footer(ui, height, |foot| {
            foot.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let export_btn = ui.add(egui::Button::new(egui::RichText::new(tr("  Export  ")).color(Color32::WHITE)).fill(t.accent));
                    auto.add("dialog.quickExport.export", export_btn.rect, "Export");
                    if export_btn.clicked() {
                        execute_action = Some((true, build_export_params(&app.dialog_state.quick_export, cid)));
                    }

                    let queue_btn = ui.button(tr("Send to Render Queue"));
                    auto.add("dialog.quickExport.sendToQueue", queue_btn.rect, "Send to Render Queue");
                    if queue_btn.clicked() {
                        execute_action = Some((false, build_export_params(&app.dialog_state.quick_export, cid)));
                    }

                    let cancel_btn = ui.button(tr("Cancel"));
                    auto.add("dialog.quickExport.cancel", cancel_btn.rect, "Cancel");
                    if cancel_btn.clicked() {
                        close = true;
                    }
                });
            });
        });

        body.vertical(|ui| {
            // Summary header banner
            let banner_rect = ui.allocate_space(vec2(ui.available_width(), 38.0)).1;
            let painter = ui.painter();
            painter.rect_filled(banner_rect, 6.0, t.field_bg);
            let info_text = format!(
                "{}  •  {}×{}  •  {:.2} fps  •  {:.2}s ({} frames)",
                comp_name,
                comp.width,
                comp.height,
                comp.frame_rate.as_f64(),
                dur_sec,
                (dur_sec * comp.frame_rate.as_f64()).round() as u64
            );
            painter.text(pos2(banner_rect.min.x + 12.0, banner_rect.center().y), Align2::LEFT_CENTER, &info_text, Tokens::ui(12.0), t.text);
            ui.add_space(8.0);

            let draft = &mut app.dialog_state.quick_export;

            // 1. Preset Selector
            ui.label(egui::RichText::new(tr("Preset:")).strong());
            let current_preset_name = PRESETS.get(draft.preset_idx).map(|p| p.name).unwrap_or("Custom...");
            egui::ComboBox::from_id_salt("quick-export-preset")
                .width(ui.available_width())
                .selected_text(current_preset_name)
                .show_ui(ui, |ui| {
                    for (i, p) in PRESETS.iter().enumerate() {
                        let is_sel = draft.preset_idx == i;
                        if ui.selectable_label(is_sel, format!("{} — {}", p.name, p.desc)).clicked() {
                            apply_preset(draft, i);
                        }
                    }
                });
            ui.add_space(6.0);

            // 2. Output Location / File Name
            ui.label(egui::RichText::new(tr("Output File:")).strong());
            ui.horizontal(|ui| {
                let text_resp = ui.add(egui::TextEdit::singleline(&mut draft.output_path).desired_width(ui.available_width() - 90.0));
                auto.add("dialog.quickExport.path", text_resp.rect, "Output File Path");

                let browse_btn = ui.button(tr("Browse…"));
                auto.add("dialog.quickExport.browse", browse_btn.rect, "Browse");
                if browse_btn.clicked() {
                    let default_path = if draft.output_path.is_empty() {
                        format!("{}.{}", comp_name, draft.format.extension())
                    } else {
                        draft.output_path.clone()
                    };
                    if let Some(Some(picked)) = app.hooks.save_dialog(&default_path) {
                        draft.output_path = picked;
                    }
                }
            });
            ui.add_space(6.0);

            // 3. Format Picker
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tr("Format:")).strong());
                let cur_format_label = draft.format.label();
                egui::ComboBox::from_id_salt("quick-export-format")
                    .width(220.0)
                    .selected_text(cur_format_label)
                    .show_ui(ui, |ui| {
                        for f in [OutputFormat::H264, OutputFormat::Hevc, OutputFormat::Av1, OutputFormat::WebM, OutputFormat::ProRes] {
                            if ui.selectable_label(draft.format == f, f.label()).clicked() {
                                draft.format = f;
                                update_extension(&mut draft.output_path, f.extension());
                                draft.preset_idx = PRESETS.len() - 1; // Custom
                            }
                        }
                    });
            });
            ui.add_space(8.0);

            // 4. Video Bitrate Controls (for H.264, HEVC, AV1, WebM)
            if draft.format.has_codec_options() || matches!(draft.format, OutputFormat::H264 | OutputFormat::WebM) {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(tr("Target Video Bitrate:")).strong());
                        ui.label(format!("{:.1} Mbps ({} kbps)", draft.video_bitrate_mbps, (draft.video_bitrate_mbps * 1000.0) as u64));
                    });

                    ui.horizontal(|ui| {
                        let prev_v = draft.video_bitrate_mbps;
                        let slider = ui.add(
                            egui::Slider::new(&mut draft.video_bitrate_mbps, 0.5..=80.0)
                                .step_by(0.5)
                                .suffix(" Mbps")
                                .logarithmic(true),
                        );
                        auto.add("dialog.quickExport.videoBitrate", slider.rect, "Video Bitrate Slider");
                        if draft.video_bitrate_mbps != prev_v {
                            draft.preset_idx = PRESETS.len() - 1;
                        }
                    });

                    // Quick Chips
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(tr("Quick Targets:")).small().color(t.text_dim));
                        for target in [2.0, 4.0, 8.0, 15.0, 25.0, 50.0] {
                            let label = format!("{target:.0}M");
                            if ui.button(&label).clicked() {
                                draft.video_bitrate_mbps = target;
                                draft.preset_idx = PRESETS.len() - 1;
                            }
                        }
                    });
                });
                ui.add_space(6.0);
            }

            // 5. Audio Bitrate Controls
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(tr("Audio Bitrate:")).strong());

                    if matches!(draft.format, OutputFormat::H264 | OutputFormat::Hevc | OutputFormat::Av1) {
                        let cur_aac = format!("{} kbps (AAC)", draft.aac_bitrate_kbps);
                        egui::ComboBox::from_id_salt("quick-export-aac")
                            .width(200.0)
                            .selected_text(cur_aac)
                            .show_ui(ui, |ui| {
                                for kbps in [96, 128, 160, 192, 256, 320] {
                                    let label = match kbps {
                                        192 => format!("{kbps} kbps (Standard)"),
                                        256 => format!("{kbps} kbps (High Quality)"),
                                        320 => format!("{kbps} kbps (Maximum)"),
                                        _ => format!("{kbps} kbps"),
                                    };
                                    if ui.selectable_label(draft.aac_bitrate_kbps == kbps, label).clicked() {
                                        draft.aac_bitrate_kbps = kbps;
                                        draft.preset_idx = PRESETS.len() - 1;
                                    }
                                }
                            });
                    } else if draft.format == OutputFormat::WebM {
                        let cur_opus = format!("{} kbps (Opus)", draft.opus_bitrate_kbps);
                        egui::ComboBox::from_id_salt("quick-export-opus")
                            .width(200.0)
                            .selected_text(cur_opus)
                            .show_ui(ui, |ui| {
                                for kbps in [64, 96, 128, 160, 192, 256] {
                                    let label = match kbps {
                                        128 => format!("{kbps} kbps (Standard)"),
                                        192 => format!("{kbps} kbps (High Quality)"),
                                        _ => format!("{kbps} kbps"),
                                    };
                                    if ui.selectable_label(draft.opus_bitrate_kbps == kbps, label).clicked() {
                                        draft.opus_bitrate_kbps = kbps;
                                        draft.preset_idx = PRESETS.len() - 1;
                                    }
                                }
                            });
                    } else {
                        ui.label(tr("Linear PCM (Uncompressed 48 kHz 24-bit Stereo)"));
                    }
                });
            });
            ui.add_space(8.0);

            // 6. Estimated File Size Banner
            let v_bps = draft.video_bitrate_mbps * 1_000_000.0;
            let a_bps = match draft.format {
                OutputFormat::WebM => draft.opus_bitrate_kbps as f64 * 1000.0,
                OutputFormat::H264 | OutputFormat::Hevc | OutputFormat::Av1 => draft.aac_bitrate_kbps as f64 * 1000.0,
                OutputFormat::ProRes | OutputFormat::Uncompressed => 1_536_000.0,
                _ => 0.0,
            };
            let total_bps = v_bps + a_bps;
            let est_bytes = (dur_sec * total_bps / 8.0).max(0.0);
            let est_mb = est_bytes / (1024.0 * 1024.0);

            let est_rect = ui.allocate_space(vec2(ui.available_width(), 44.0)).1;
            let painter = ui.painter();
            painter.rect_filled(est_rect, 6.0, t.row_selected);
            let summary_text = format!(
                "Estimated File Size: ~{est_mb:.1} MB\nCombined Bitrate: ~{:.2} Mbps (Video {:.1}M + Audio {:.0}k)",
                total_bps / 1_000_000.0,
                draft.video_bitrate_mbps,
                a_bps / 1000.0
            );
            painter.text(pos2(est_rect.min.x + 12.0, est_rect.center().y), Align2::LEFT_CENTER, &summary_text, Tokens::ui(11.5), t.text);
        });
    });

    if let Some((start_render, params)) = execute_action {
        if let Err(e) = app.session.execute("renderQueue.add", params) {
            app.ui.status = e.to_string();
        } else {
            if start_render {
                if let Err(e) = app.session.execute("renderQueue.render", json!({"wait": false})) {
                    app.ui.status = e.to_string();
                }
            }
            app.show_panel(crate::dock::PanelKind::RenderQueue);
            app.dialog = None;
        }
    }

    if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.dialog = None;
    }
}

fn build_export_params(draft: &QuickExportDraft, cid: ItemId) -> serde_json::Value {
    let kbps = (draft.video_bitrate_mbps * 1000.0).round() as u32;
    let mut p = json!({
        "comp": cid.0,
        "output": draft.output_path,
        "format": format_to_str(draft.format),
        "bitrate": kbps,
        "bitrateMbps": draft.video_bitrate_mbps,
    });

    if matches!(draft.format, OutputFormat::H264 | OutputFormat::Hevc | OutputFormat::Av1) {
        p["aacBitrate"] = json!(draft.aac_bitrate_kbps);
    } else if draft.format == OutputFormat::WebM {
        p["codecOptions"] = json!({
            "opusBitrate": draft.opus_bitrate_kbps * 1000
        });
    }
    p
}

fn format_to_str(f: OutputFormat) -> &'static str {
    match f {
        OutputFormat::H264 => "h264",
        OutputFormat::Hevc => "hevc",
        OutputFormat::Av1 => "av1",
        OutputFormat::ProRes => "prores",
        OutputFormat::WebM => "webm",
        OutputFormat::Uncompressed => "uncompressed",
        _ => "h264",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> (EffectcraftApp, egui::Context) {
        let ctx = egui::Context::default();
        let mut app = EffectcraftApp::new(effectcraft_engine::Session::default());
        crate::theme::install(&ctx, &app.tokens, crate::i18n::language(&app));
        app.session.execute("comp.new", json!({"name": "QuickExportComp", "width": 1920, "height": 1080})).unwrap();
        (app, ctx)
    }

    #[test]
    fn quick_export_opens_and_renders_ui() {
        let (mut app, ctx) = test_app();
        open(&mut app, &json!({})).unwrap();
        assert_eq!(app.dialog, Some(crate::Dialog::QuickExport));
        assert!(app.dialog_state.quick_export.output_path.contains("QuickExportComp.mp4"));

        let tokens = app.tokens;
        let mut out = ctx.run_ui(Default::default(), |ui| {
            app.auto.begin_frame();
            show(&mut app, ui.ctx(), &tokens);
        });
        out.textures_delta.clear();

        for id in [
            "dialog.quickExport.path",
            "dialog.quickExport.browse",
            "dialog.quickExport.videoBitrate",
            "dialog.quickExport.export",
            "dialog.quickExport.sendToQueue",
            "dialog.quickExport.cancel",
        ] {
            assert!(app.auto.find(id).is_some(), "missing automation tag {id}");
        }
    }

    #[test]
    fn quick_export_sends_to_render_queue_with_bitrates() {
        let (mut app, _) = test_app();
        open(&mut app, &json!({
            "videoBitrate": 8_500_000.0,
            "aacBitrate": 256,
            "path": "D:/test_export.mp4"
        })).unwrap();

        let cid = app.session.active_comp_id().unwrap();
        let params = build_export_params(&app.dialog_state.quick_export, cid);
        app.session.execute("renderQueue.add", params).unwrap();

        let q = &app.session.project.render_queue;
        assert_eq!(q.len(), 1);
        let item = &q[0];
        assert_eq!(item.output.bitrate_kbps, 8_500);
        assert_eq!(item.output.aac_bitrate_kbps, 256);
    }
}

