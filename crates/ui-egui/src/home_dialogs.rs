//! Home's dialogs: New Folder, Rename Folder and Folder Paths. They are command dialogs like the
//! others (their fields are the dialog's state, so `ui.dialog.set` and `ui.dialog.confirm` drive
//! them) whose OK runs `home.*` commands.

use egui::{FontId, Sense, Stroke, StrokeKind, pos2, vec2};
use photocraft_engine::home_cmds::DirKind;
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::state::DialogKind;
use crate::theme::Tokens;

/// The field that marks a Home dialog and says which one.
const KEY: &str = "__home";

pub(crate) fn owns(f: &Map<String, Value>) -> bool {
    f.contains_key(KEY)
}

fn kind(f: &Map<String, Value>) -> &str {
    f.get(KEY).and_then(Value::as_str).unwrap_or("")
}

fn text<'a>(f: &'a Map<String, Value>, key: &str) -> &'a str {
    f.get(key).and_then(Value::as_str).unwrap_or("")
}

fn open(app: &mut PhotocraftApp, which: &str, label: &str, fields: Value) {
    let mut f: Map<String, Value> = fields.as_object().cloned().unwrap_or_default();
    f.insert(KEY.into(), json!(which));
    f.insert("__label".into(), json!(label));
    app.ui.open_dialog(DialogKind::Command, f);
}

/// File › New Folder from Home: a name, then a new directory in the projects folder or an
/// existing directory to link.
pub(crate) fn open_new_folder(app: &mut PhotocraftApp) {
    let root = photocraft_engine::home_cmds::projects_root(&app.session).unwrap_or_default();
    open(app, "newFolder", "New Folder", json!({"name": "", "mode": "create", "root": root, "path": ""}));
}

pub(crate) fn open_rename(app: &mut PhotocraftApp, id: &str) {
    let Some(name) = app.session.prefs().home.folders.iter().find(|f| f.id == id).map(|f| f.name.clone()) else { return };
    open(app, "rename", "Rename Folder", json!({"id": id, "name": name}));
}

/// A folder's three paths; Exports and Resources read "" while they follow the Projects path.
pub(crate) fn open_paths(app: &mut PhotocraftApp, id: &str) {
    let Some(f) = app.session.prefs().home.folders.iter().find(|f| f.id == id).cloned() else { return };
    let fields = json!({
        "id": id,
        "path": f.path,
        "exports": f.exports.unwrap_or_default(),
        "resources": f.resources.unwrap_or_default(),
        "__path": f.path,
    });
    open(app, "paths", "Folder Paths", fields);
}

/// The OK button's label.
pub(crate) fn ok_label(f: &Map<String, Value>) -> Option<&'static str> {
    match kind(f) {
        "newFolder" => Some(tl!("Create")),
        "rename" => Some(tl!("Rename")),
        _ => None,
    }
}

/// Can OK run with these fields?
pub(crate) fn ok_enabled(f: &Map<String, Value>) -> bool {
    match kind(f) {
        "newFolder" => !text(f, "name").trim().is_empty() && (text(f, "mode") != "link" || !text(f, "path").is_empty()),
        "rename" => !text(f, "name").trim().is_empty(),
        "paths" => !text(f, "path").is_empty(),
        _ => true,
    }
}

/// Folder Paths is wider than the default dialog.
pub(crate) fn dialog_width(f: &Map<String, Value>) -> Option<f32> {
    match kind(f) {
        "paths" => Some(560.0),
        "newFolder" => Some(500.0),
        "rename" => Some(380.0),
        _ => None,
    }
}

pub(crate) fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    match kind(f) {
        "newFolder" => new_folder_body(app, ui, f),
        "rename" => {
            name_field(ui, f, tl!("Folder name"));
        }
        "paths" => paths_body(app, ui, f),
        _ => {}
    }
}

/// The name field, focused when the dialog opens.
fn name_field(ui: &mut egui::Ui, f: &mut Map<String, Value>, hint: &str) {
    let mut name = text(f, "name").to_string();
    ui.label(egui::RichText::new(tl!("Name:")).color(Tokens::get(ui.ctx()).text_dim));
    let r = ui.add(egui::TextEdit::singleline(&mut name).hint_text(hint).desired_width(f32::INFINITY));
    if !f.contains_key("__focused") {
        r.request_focus();
        f.insert("__focused".into(), json!(true));
    }
    f.insert("name".into(), json!(name));
}

/// A path shown like a read-only field, with `~` for the home directory.
fn path_box(ui: &mut egui::Ui, path: &str, placeholder: &str, width: f32) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
    ui.painter().rect_filled(r, t.radius_sm, t.field);
    ui.painter().rect_stroke(r, t.radius_sm, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    let (s, col) = if path.is_empty() { (placeholder.to_string(), t.text_faint) } else { (crate::home_ui::display_path(path), t.text) };
    crate::home_ui::tail_label(ui, &s, pos2(r.min.x + 8.0, r.center().y), FontId::proportional(12.5), col, r.width() - 16.0);
    if !path.is_empty() {
        ui.interact(r, ui.id().with(("path-box", path)), Sense::hover()).on_hover_text(path);
    }
}

/// Ask for a directory starting at `start`.
fn pick(app: &mut PhotocraftApp, start: &str) -> Option<String> {
    let start = Some(start).filter(|s| photocraft_engine::home_cmds::is_dir(s));
    app.services.pick_folder.as_mut().and_then(|f| f(start))
}

fn new_folder_body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    name_field(ui, f, tl!("For example: Flyers 2026"));
    ui.add_space(12.0);
    let mut mode = text(f, "mode").to_string();
    let wide = ui.available_width();
    let button_w = 96.0;
    ui.radio_value(&mut mode, "create".to_string(), tl!("Create a new folder in:"));
    ui.horizontal(|ui| {
        ui.add_space(24.0);
        let root = text(f, "root").to_string();
        path_box(ui, &root, tl!("No folder chosen"), (wide - 24.0 - button_w - 8.0).max(80.0));
        if crate::widgets::secondary_button(ui, tl!("Change…"), button_w).clicked()
            && let Some(dir) = pick(app, &root)
        {
            f.insert("root".into(), json!(dir));
            mode = "create".into();
        }
    });
    ui.add_space(8.0);
    ui.radio_value(&mut mode, "link".to_string(), tl!("Use an existing folder:"));
    ui.horizontal(|ui| {
        ui.add_space(24.0);
        let path = text(f, "path").to_string();
        let start = if path.is_empty() { text(f, "root").to_string() } else { path.clone() };
        path_box(ui, &path, tl!("No folder chosen"), (wide - 24.0 - button_w - 8.0).max(80.0));
        if crate::widgets::secondary_button(ui, tl!("Choose…"), button_w).clicked()
            && let Some(dir) = pick(app, &start)
        {
            // The folder's own name is a good first name.
            if text(f, "name").trim().is_empty()
                && let Some(n) = std::path::Path::new(&dir).file_name()
            {
                f.insert("name".into(), json!(n.to_string_lossy()));
            }
            f.insert("path".into(), json!(dir));
            mode = "link".into();
        }
    });
    f.insert("mode".into(), json!(mode));
    ui.add_space(12.0);
    ui.label(
        egui::RichText::new(tl!("PhotoCraft never moves or deletes your files. Removing a folder from Home only takes it off the list."))
            .size(11.5)
            .color(t.text_faint),
    );
}

fn paths_body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let rows = [
        (DirKind::Projects, "path", tl!("Projects"), tl!("Save As and Save a Copy start here (.psd, .pcraft).")),
        (DirKind::Exports, "exports", tl!("Exports"), tl!("Export As, Quick Export and Save for Web start here (PNG, JPEG…).")),
        (DirKind::Resources, "resources", tl!("Resources"), tl!("Open and Place start here: photos, logos and other material.")),
    ];
    let wide = ui.available_width();
    for (kind, key, title, help) in rows {
        let value = text(f, key).to_string();
        let projects = text(f, "path").to_string();
        ui.label(egui::RichText::new(title.to_uppercase()).font(crate::theme::semibold(11.0)).color(t.text_faint));
        ui.horizontal(|ui| {
            // Room for Browse… and Reset on every row, so the paths line up.
            let buttons = 96.0 * 2.0 + 8.0;
            let shown = if value.is_empty() { projects.clone() } else { value.clone() };
            let placeholder = tl!("No folder chosen");
            path_box(ui, &shown, placeholder, (wide - buttons - 8.0).max(80.0));
            if crate::widgets::secondary_button(ui, tl!("Browse…"), 96.0).clicked()
                && let Some(dir) = pick(app, &shown)
            {
                f.insert(key.into(), json!(dir));
            }
            if kind != DirKind::Projects && ui.add_enabled_ui(!value.is_empty(), |ui| crate::widgets::secondary_button(ui, tl!("Reset"), 96.0)).inner.clicked()
            {
                f.insert(key.into(), json!(""));
            }
        });
        let note =
            if kind != DirKind::Projects && value.is_empty() { format!("{help} {}", tl!("Same as Projects until you choose one.")) } else { help.to_string() };
        ui.label(egui::RichText::new(note).size(11.5).color(t.text_faint));
        ui.add_space(10.0);
    }
}

/// OK: run the dialog's `home.*` command.
pub(crate) fn confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    let r = match kind(f) {
        "newFolder" => {
            let name = text(f, "name").trim().to_string();
            let params = if text(f, "mode") == "link" {
                json!({"name": name, "path": text(f, "path"), "create": false})
            } else {
                let root = text(f, "root");
                if !root.is_empty() && photocraft_engine::home_cmds::projects_root(&app.session).as_deref() != Some(root) {
                    app.run("home.setProjectsRoot", json!({"path": root, "create": true}))?;
                }
                json!({"name": name, "create": true})
            };
            let r = app.run("home.folderNew", params)?;
            if let Some(id) = r["id"].as_str() {
                app.ui.chrome.home_folder = Some(id.to_string());
            }
            r
        }
        "rename" => app.run("home.folderEdit", json!({"id": text(f, "id"), "name": text(f, "name")}))?,
        "paths" => {
            let mut params = json!({"id": text(f, "id"), "exports": text(f, "exports"), "resources": text(f, "resources")});
            // Only a changed Projects path is checked again (a moved folder can still edit the others).
            if text(f, "path") != text(f, "__path") {
                params["path"] = json!(text(f, "path"));
            }
            app.run("home.folderEdit", params)?
        }
        other => return Err(format!("unknown Home dialog `{other}`")),
    };
    app.home.invalidate();
    Ok(r)
}
