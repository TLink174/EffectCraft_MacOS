//! Export an EffectCraft project or composition to an Adobe After Effects ExtendScript (`.jsx`) file.
//!
//! Running the generated `.jsx` script in After Effects (`File ▸ Scripts ▸ Run Script File...`)
//! recreates the compositions, footages, solid layers, precomp layers, null layers,
//! parenting hierarchies, transform properties, and keyframe animations natively in After Effects.

use std::collections::HashSet;
use effectcraft_project::{Comp, ItemId, ItemKind, Layer, LayerSource, Project};
use effectcraft_keyframe::Value;

/// Escapes a string for use in JavaScript string literals.
fn js_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_add(4));
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Helper to sanitize floats so NaN/Inf don't break JavaScript execution.
fn js_f64(v: f64) -> f64 {
    if v.is_finite() { v } else { 0.0 }
}

/// Generates a complete Adobe After Effects ExtendScript (`.jsx`) script from an EffectCraft project.
pub fn export_project_to_jsx(project: &Project, target_comp_id: Option<ItemId>) -> String {
    let mut out = String::with_capacity(8192);

    out.push_str("// =========================================================================\n");
    out.push_str("// EffectCraft to Adobe After Effects Exporter\n");
    out.push_str("// Generated automatically by EffectCraft\n");
    out.push_str("// Usage: In Adobe After Effects, run File -> Scripts -> Run Script File...\n");
    out.push_str("// =========================================================================\n\n");
    out.push_str("(function() {\n");
    out.push_str("    app.beginUndoGroup(\"Import EffectCraft Project\");\n");
    out.push_str("    var proj = app.project;\n");
    out.push_str("    if (!proj) {\n");
    out.push_str("        alert(\"Please open or create a project in Adobe After Effects first.\");\n");
    out.push_str("        return;\n");
    out.push_str("    }\n\n");

    // Root folder in AE Project panel
    out.push_str("    var rootFolder = proj.items.addFolder(\"EffectCraft Export\");\n");
    out.push_str("    var footageMap = {};\n");
    out.push_str("    var compMap = {};\n\n");

    // 1. Footages
    out.push_str("    // --- Import Footages ---\n");
    out.push_str("    function loadFootage(id, path, name) {\n");
    out.push_str("        try {\n");
    out.push_str("            var f = new File(path);\n");
    out.push_str("            if (f.exists) {\n");
    out.push_str("                var item = proj.importFile(new ImportOptions(f));\n");
    out.push_str("                if (name) item.name = name;\n");
    out.push_str("                item.parentFolder = rootFolder;\n");
    out.push_str("                footageMap[id] = item;\n");
    out.push_str("                return item;\n");
    out.push_str("            }\n");
    out.push_str("        } catch(e) {}\n");
    out.push_str("        return null;\n");
    out.push_str("    }\n\n");

    for (&id, item) in &project.items {
        if let ItemKind::Footage(f) = &item.kind {
            if !f.path.is_empty() {
                out.push_str(&format!(
                    "    loadFootage({}, {}, {});\n",
                    id.0,
                    js_quote(&f.path),
                    js_quote(&item.name)
                ));
            }
        }
    }
    out.push('\n');

    // 2. Identify comps to export (dependency-sorted)
    let comps_to_export = order_comps_for_export(project, target_comp_id);

    // 3. Create Comps
    out.push_str("    // --- Create Compositions ---\n");
    for &cid in &comps_to_export {
        if let Some(comp) = project.comp(cid) {
            let comp_name = project.item(cid).map(|i| i.name.as_str()).unwrap_or("Comp");
            let w = comp.width;
            let h = comp.height;
            let fps = comp.frame_rate.as_f64().clamp(1.0, 999.0);
            let dur = comp.duration.seconds().max(0.1);

            out.push_str(&format!(
                "    var comp_{id} = proj.items.addComp({name}, {w}, {h}, 1.0, {dur}, {fps});\n",
                id = cid.0,
                name = js_quote(comp_name),
            ));
            out.push_str(&format!("    comp_{id}.parentFolder = rootFolder;\n", id = cid.0));
            out.push_str(&format!("    compMap[{id}] = comp_{id};\n\n", id = cid.0));
        }
    }

    // 4. Populate Layers for each Comp (bottom-to-top so AE layer indices match EffectCraft)
    out.push_str("    // --- Populate Composition Layers ---\n");
    for &cid in &comps_to_export {
        if let Some(comp) = project.comp(cid) {
            export_comp_layers(&mut out, cid, comp, project);
        }
    }

    // 5. Open primary comp
    if let Some(&first_id) = comps_to_export.last() {
        out.push_str(&format!("    if (compMap[{id}]) compMap[{id}].openInViewer();\n", id = first_id.0));
    }

    out.push_str("\n    app.endUndoGroup();\n");
    out.push_str("})();\n");

    out
}

/// Order comps so precomps are created before the compositions that nest them.
fn order_comps_for_export(project: &Project, target: Option<ItemId>) -> Vec<ItemId> {
    let mut order = Vec::new();
    let mut visited = HashSet::new();

    fn visit(
        id: ItemId,
        project: &Project,
        visited: &mut HashSet<ItemId>,
        order: &mut Vec<ItemId>,
    ) {
        if visited.contains(&id) {
            return;
        }
        visited.insert(id);
        if let Some(comp) = project.comp(id) {
            for layer in &comp.layers {
                if let LayerSource::Comp { item } = layer.source {
                    visit(item, project, visited, order);
                }
            }
            order.push(id);
        }
    }

    if let Some(target_id) = target {
        visit(target_id, project, &mut visited, &mut order);
    } else {
        for (&id, _) in project.comps() {
            visit(id, project, &mut visited, &mut order);
        }
    }

    order
}

/// Exports layers of a single composition to JSX ExtendScript.
fn export_comp_layers(out: &mut String, cid: ItemId, comp: &Comp, _project: &Project) {
    let comp_var = format!("comp_{}", cid.0);
    out.push_str(&format!("    // Comp: {}\n", cid.0));
    out.push_str(&format!("    var layerMap_{} = {{}};\n", cid.0));

    // Iterate layers in reverse order (bottom-to-top) so when added in AE they stack in the right order
    for layer in comp.layers.iter().rev() {
        let lid = layer.id.0;
        let layer_name = js_quote(&layer.name);

        match layer.source {
            LayerSource::Solid { item: _ } => {
                let w = comp.width;
                let h = comp.height;
                out.push_str(&format!(
                    "    var l_{lid} = {comp_var}.layers.addSolid([0.5, 0.5, 0.5], {layer_name}, {w}, {h}, 1.0);\n"
                ));
            }
            LayerSource::Footage { item } => {
                out.push_str(&format!(
                    "    var l_{lid} = footageMap[{fid}] ? {comp_var}.layers.add(footageMap[{fid}]) : {comp_var}.layers.addNull();\n",
                    fid = item.0
                ));
            }
            LayerSource::Comp { item } => {
                out.push_str(&format!(
                    "    var l_{lid} = compMap[{sub_cid}] ? {comp_var}.layers.add(compMap[{sub_cid}]) : {comp_var}.layers.addNull();\n",
                    sub_cid = item.0
                ));
            }
            _ => {
                out.push_str(&format!("    var l_{lid} = {comp_var}.layers.addNull();\n"));
            }
        }

        out.push_str(&format!("    l_{lid}.name = {layer_name};\n"));
        let st = js_f64(layer.start_time.seconds());
        let in_pt = js_f64(layer.in_point.seconds());
        let out_pt = js_f64(layer.out_point.seconds());
        out.push_str(&format!("    l_{lid}.startTime = {st};\n"));
        out.push_str(&format!("    l_{lid}.inPoint = {in_pt};\n"));
        out.push_str(&format!("    l_{lid}.outPoint = {out_pt};\n"));
        if !layer.switches.video {
            out.push_str(&format!("    l_{lid}.enabled = false;\n"));
        }
        if layer.switches.adjustment {
            out.push_str(&format!("    l_{lid}.adjustmentLayer = true;\n"));
        }
        if layer.switches.three_d {
            out.push_str(&format!("    l_{lid}.threeDLayer = true;\n"));
        }

        // Export transform properties and keyframes
        export_layer_transforms(out, lid, layer);

        out.push_str(&format!("    layerMap_{cid}[{lid}] = l_{lid};\n\n", cid = cid.0));
    }

    // Apply parenting links
    for layer in &comp.layers {
        if let Some(parent_id) = layer.parent {
            out.push_str(&format!(
                "    if (layerMap_{cid}[{lid}] && layerMap_{cid}[{pid}]) layerMap_{cid}[{lid}].parent = layerMap_{cid}[{pid}];\n",
                cid = cid.0,
                lid = layer.id.0,
                pid = parent_id.0
            ));
        }
    }
    out.push('\n');
}

/// Exports layer transform properties (AnchorPoint, Position, Scale, Rotation, Opacity) and keyframes.
fn export_layer_transforms(out: &mut String, lid: u64, layer: &Layer) {
    let props = &layer.props;

    // Anchor Point
    if let Some(prop) = props.prop("transform/anchor_point") {
        emit_prop_keyframes(out, &format!("l_{lid}.property(\"Anchor Point\")"), prop);
    }
    // Position
    if let Some(prop) = props.prop("transform/position") {
        emit_prop_keyframes(out, &format!("l_{lid}.property(\"Position\")"), prop);
    }
    // Scale
    if let Some(prop) = props.prop("transform/scale") {
        emit_prop_keyframes(out, &format!("l_{lid}.property(\"Scale\")"), prop);
    }
    // Rotation
    if let Some(prop) = props.prop("transform/rotation") {
        emit_prop_keyframes(out, &format!("l_{lid}.property(\"Rotation\")"), prop);
    }
    // Opacity
    if let Some(prop) = props.prop("transform/opacity") {
        emit_prop_keyframes(out, &format!("l_{lid}.property(\"Opacity\")"), prop);
    }
}

/// Emits JavaScript to set property static value or keyframes at specific times.
fn emit_prop_keyframes(out: &mut String, js_prop: &str, prop: &effectcraft_project::Property) {
    if prop.keys.is_empty() {
        if let Some(val_str) = format_jsx_value(&prop.value) {
            out.push_str(&format!("    try {{ {js_prop}.setValue({val_str}); }} catch(e) {{}}\n"));
        }
    } else {
        for key in &prop.keys {
            let t = js_f64(key.time.seconds());
            if let Some(val_str) = format_jsx_value(&key.value) {
                out.push_str(&format!(
                    "    try {{ {js_prop}.setValueAtTime({t}, {val_str}); }} catch(e) {{}}\n"
                ));
            }
        }
    }
}

/// Formats a keyframe/property `Value` into ExtendScript syntax.
fn format_jsx_value(val: &Value) -> Option<String> {
    match val {
        Value::Scalar(v) => Some(format!("{}", js_f64(*v))),
        Value::Vec2([x, y]) => Some(format!("[{}, {}]", js_f64(*x), js_f64(*y))),
        Value::Vec3([x, y, z]) => Some(format!("[{}, {}, {}]", js_f64(*x), js_f64(*y), js_f64(*z))),
        Value::Color([r, g, b, _]) => Some(format!("[{}, {}, {}]", js_f64(*r), js_f64(*g), js_f64(*b))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exports_project_to_extendscript_jsx() {
        let mut s = crate::Session::default();
        s.execute("comp.new", json!({"name": "Main Show", "width": 1920, "height": 1080, "frameRate": 30, "duration": 5})).unwrap();
        s.execute("layer.newSolid", json!({"name": "Background Solid", "color": "#ff0000"})).unwrap();

        let jsx = export_project_to_jsx(&s.project, None);
        assert!(jsx.contains("app.beginUndoGroup(\"Import EffectCraft Project\");"));
        assert!(jsx.contains(".layers.addSolid("));
        assert!(jsx.contains("Background Solid"));
        assert!(jsx.contains("app.endUndoGroup();"));
    }
}
