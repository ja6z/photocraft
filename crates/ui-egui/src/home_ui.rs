//! Home, the start page (Options bar › Home; at launch with no documents when Preferences ›
//! General › Auto show the Home Screen is on). It covers the window under the menus like
//! Photoshop's: a sidebar with Home and the project folders, and either the start view (New
//! document…, Open…, the recently opened files newest first) or a folder's view (its files, its
//! Projects / Exports / Resources paths and what to do in it).
//!
//! Everything it changes goes through `home.*` engine commands; what it shows (the view, the cards
//! and folder rows on screen) is in `ui.inspect` › `home`, and `ui.set {home}` switches the view.
//! The web build has no file system: no folders and no recents, just the start actions.

use std::hash::{Hash, Hasher};

use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use photocraft_engine::home_cmds::DirKind;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::home_cards::{self, CardAction, CardRect, Listing};
use crate::theme::Tokens;

/// Width of the sidebar.
const SIDEBAR_W: f32 = 236.0;
/// Margin of the main area.
const PAD: f32 = 36.0;
/// Listings are read again at least this often (seconds) while Home is up, so files saved by
/// other apps show up.
const REFRESH_S: f64 = 3.0;
/// Height of a sidebar row.
const ROW_H: f32 = 32.0;

/// Home's runtime state: listings, what is on screen, thumbnails.
#[derive(Default)]
pub struct Runtime {
    recent: Option<Cached>,
    folders: Option<Cached>,
    files: Option<(String, Cached)>,
    /// Cards on screen this frame (`ui.inspect` › `home.cards`).
    pub(crate) cards: Vec<CardRect>,
    /// Sidebar folder rows this frame: (folder id, rect).
    sidebar: Vec<(String, Rect)>,
    pub(crate) thumbs: home_cards::Thumbs,
    /// Files whose open makes the document belong to a folder: (path, folder id).
    open_into: Vec<(String, String)>,
    stale: bool,
}

impl Runtime {
    /// Read the listings again on the next frame.
    pub fn invalidate(&mut self) {
        self.stale = true;
    }

    /// Wait (at most `timeout`) until background thumbnails are written; false on timeout.
    pub fn wait_thumbnails(&mut self, timeout: std::time::Duration) -> bool {
        let idle = self.thumbs.wait_idle(timeout);
        self.stale = true;
        idle
    }
}

/// A listing and when it was read.
struct Cached {
    at: f64,
    key: u64,
    rows: Vec<Value>,
}

/// Folders need a file system.
pub fn folders_supported() -> bool {
    !cfg!(target_arch = "wasm32")
}

/// Is Home on screen?
pub fn shown(app: &PhotocraftApp) -> bool {
    app.jobs.opens.is_empty() && app.ui.chrome.shows_home(app.session.documents().len(), app.session.prefs().general.auto_show_home_screen)
}

/// The folder shown on Home, if Home is up with a folder view (file dialogs start there).
pub fn shown_folder(app: &PhotocraftApp) -> Option<String> {
    if shown(app) { app.ui.chrome.home_folder.clone() } else { None }
}

/// The page's name ("Home" as a place: some languages say the Home button's "Home" differently).
fn page_title() -> &'static str {
    crate::i18n::tr_ctx(crate::i18n::current(), "homePage", "Home")
}

/// "Show in Finder" in the platform's words.
pub(crate) fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        tl!("Show in Finder")
    } else if cfg!(target_os = "windows") {
        tl!("Show in Explorer")
    } else {
        tl!("Show in Folder")
    }
}

/// The start view's file-drop hint. Wayland has no native file drops (winit 0.30, #386), so the
/// hint offers File › Open and paste.
pub(crate) fn drop_hint(app: &PhotocraftApp) -> std::borrow::Cow<'static, str> {
    if app.services.is_wayland {
        crate::i18n::fmt(tl!("Use File › Open, or paste a copied image with {paste}."), &[("paste", &crate::notices::paste_hint(app))]).into()
    } else {
        tl!("Drop an image or PSD anywhere to open it.").into()
    }
}

/// Files in `paths` opened next belong to folder `id` (File › Open from a folder view).
pub(crate) fn open_into(app: &mut PhotocraftApp, paths: &[String], id: &str) {
    for p in paths {
        app.home.open_into.retain(|(q, _)| q != p);
        app.home.open_into.push((p.clone(), id.to_string()));
    }
}

/// A document was opened from `path` (it is the active one): it joins the folder it was opened
/// from, and gets a thumbnail unless a fresh one exists.
pub(crate) fn document_opened(app: &mut PhotocraftApp, path: &str) {
    if let Some(i) = app.home.open_into.iter().position(|(p, _)| p == path) {
        let (_, id) = app.home.open_into.remove(i);
        if let Err(e) = app.session.execute("home.setDocumentFolder", json!({"folder": id})) {
            log::debug!("home: {e}");
        }
    }
    if let Some(i) = app.session.active_index()
        && !photocraft_engine::home_thumbs::is_fresh(&app.session, path)
    {
        queue_document_thumbnail(app, i);
    }
    app.home.stale = true;
}

/// Render the Home thumbnail of open document `index` in the background (after a save; after an
/// open without one).
pub(crate) fn queue_document_thumbnail(app: &mut PhotocraftApp, index: usize) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some((doc, out)) = photocraft_engine::home_thumbs::document_job(&app.session, index) {
        app.home.thumbs.queue(home_cards::Job::Document { doc, out });
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (&app, index);
    app.home.stale = true;
}

/// What changes the listings: the preferences (recents, folders) and which files are open.
fn state_key(app: &PhotocraftApp) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    app.session.prefs.rev().hash(&mut h);
    for d in app.session.documents() {
        d.path.hash(&mut h);
    }
    h.finish()
}

fn rows_of(v: Result<Value, photocraft_engine::EngineError>) -> Vec<Value> {
    match v {
        Ok(Value::Array(rows)) => rows,
        Ok(_) => Vec::new(),
        Err(e) => {
            log::debug!("home: {e}");
            Vec::new()
        }
    }
}

/// Read the listings again when something changed or they're older than [`REFRESH_S`].
fn refresh(app: &mut PhotocraftApp, now: f64) {
    let key = state_key(app);
    let stale = std::mem::take(&mut app.home.stale);
    let fresh = |c: &Cached| !stale && c.key == key && (0.0..REFRESH_S).contains(&(now - c.at));
    if !folders_supported() {
        return;
    }
    if !app.home.recent.as_ref().is_some_and(fresh) {
        let rows = rows_of(app.session.execute("home.recent", json!({})));
        app.home.recent = Some(Cached { at: now, key, rows });
    }
    if !app.home.folders.as_ref().is_some_and(fresh) {
        let rows = rows_of(app.session.execute("home.folders", json!({})));
        app.home.folders = Some(Cached { at: now, key, rows });
    }
    if let Some(id) = app.ui.chrome.home_folder.clone()
        && !app.home.files.as_ref().is_some_and(|(f, c)| *f == id && fresh(c))
    {
        let rows = rows_of(app.session.execute("home.folderFiles", json!({"id": id})));
        app.home.files = Some((id, Cached { at: now, key, rows }));
    }
}

fn folder_rows(app: &PhotocraftApp) -> Vec<Value> {
    app.home.folders.as_ref().map(|c| c.rows.clone()).unwrap_or_default()
}

/// A page-level request from Home's controls.
#[derive(Clone, Debug, PartialEq)]
enum Action {
    Card(CardAction),
    NewDocument(Option<String>),
    Open,
    ShowRecent,
    ShowFolder(String),
    NewFolder,
    Rename(String),
    Paths(String),
    Reveal(String),
    RemoveFolder(String),
    /// Pick a new directory for one of folder `.0`'s paths.
    PickDir(String, DirKind),
}

/// Draw Home over the document area.
pub fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let t = Tokens::get(&ctx);
    let area = ui.available_rect_before_wrap();
    let _ = ui.allocate_rect(area, Sense::hover());
    let now = ctx.input(|i| i.time);
    if app.home.thumbs.poll(&ctx) {
        app.home.stale = true;
    }
    app.home.cards.clear();
    app.home.sidebar.clear();
    refresh(app, now);
    // A folder removed meanwhile (by an agent, another window) leaves its view.
    if let Some(id) = app.ui.chrome.home_folder.clone()
        && !folder_rows(app).iter().any(|f| f["id"].as_str() == Some(id.as_str()))
    {
        app.ui.chrome.home_folder = None;
    }
    let side_w = if folders_supported() { SIDEBAR_W.min((area.width() * 0.34).max(160.0)) } else { 0.0 };
    let side = Rect::from_min_size(area.min, vec2(side_w, area.height()));
    let main = Rect::from_min_max(pos2(side.max.x, area.min.y), area.max);
    ui.painter().rect_filled(main, 0.0, t.canvas);
    let mut action = None;
    if side_w > 0.0 {
        action = sidebar(app, ui, side, &t);
    }
    let view = match app.ui.chrome.home_folder.clone() {
        Some(id) => folder_view(app, ui, main, &id, &t),
        None => start_view(app, ui, main, &t),
    };
    if let Some(a) = view.or(action) {
        apply(app, a);
    }
}

/// Paint `text` elided to `max_w`, vertically centred on `y`; returns its width.
fn label(ui: &egui::Ui, text: &str, pos: egui::Pos2, font: FontId, color: Color32, max_w: f32) -> f32 {
    let g = crate::tab_strip::elided(ui, text, font, color, max_w);
    let w = g.size().x;
    ui.painter().galley(pos2(pos.x, pos.y - g.size().y / 2.0), g, color);
    w
}

/// A path elided from the front so its last folders stay readable: `…/Flyers/2026`.
pub(crate) fn tail_label(ui: &egui::Ui, path: &str, pos: egui::Pos2, font: FontId, color: Color32, max_w: f32) {
    let fits = |s: &str| ui.painter().layout_no_wrap(s.to_string(), font.clone(), color).size().x <= max_w;
    let shown = if fits(path) {
        path.to_string()
    } else {
        let sep = if path.contains('\\') && !path.contains('/') { '\\' } else { '/' };
        let parts: Vec<&str> = path.split(sep).filter(|p| !p.is_empty()).collect();
        (1..parts.len())
            .map(|k| format!("…{sep}{}", parts[k..].join(&sep.to_string())))
            .find(|s| fits(s))
            .unwrap_or_else(|| format!("…{sep}{}", parts.last().copied().unwrap_or_default()))
    };
    label(ui, &shown, pos, font, color, max_w);
}

/// A sidebar row: icon, text, optional count.
struct Nav<'a> {
    icon: &'a str,
    text: &'a str,
    count: Option<usize>,
    selected: bool,
}

/// A clickable sidebar row; selected rows are highlighted.
fn nav_row(ui: &mut egui::Ui, r: Rect, id: egui::Id, nav: Nav<'_>, t: &Tokens) -> egui::Response {
    let Nav { icon, text, count, selected } = nav;
    let resp = ui.interact(r, id, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, text));
    let fg = if selected && t.bevel { t.accent_text } else { t.text };
    if selected {
        ui.painter().rect_filled(r, t.radius, t.accent_soft);
    } else if resp.hovered() {
        ui.painter().rect_filled(r, t.radius, t.hover);
    }
    let icon_col = if icon == "triangle-alert" {
        t.warning
    } else if selected {
        if t.bevel { t.accent_text } else { t.accent }
    } else {
        t.icon
    };
    crate::icons::paint(ui, Rect::from_center_size(pos2(r.min.x + 17.0, r.center().y), vec2(18.0, 18.0)), icon, 16.0, icon_col);
    let mut right = r.max.x - 10.0;
    if let Some(n) = count {
        let g = ui.painter().layout_no_wrap(n.to_string(), FontId::proportional(11.5), t.text_faint);
        right -= g.size().x;
        ui.painter().galley(pos2(right, r.center().y - g.size().y / 2.0), g, t.text_faint);
        right -= 8.0;
    }
    label(ui, text, pos2(r.min.x + 34.0, r.center().y), FontId::proportional(13.0), fg, right - r.min.x - 34.0);
    resp
}

fn sidebar(app: &mut PhotocraftApp, ui: &mut egui::Ui, side: Rect, t: &Tokens) -> Option<Action> {
    let mut action = None;
    ui.painter().rect_filled(side, 0.0, t.chrome);
    ui.painter().line_segment([side.right_top(), side.right_bottom()], Stroke::new(1.0, t.separator));
    let x0 = side.min.x + 12.0;
    let w = side.width() - 24.0;
    // Brand.
    let mark = Rect::from_min_size(pos2(x0 + 4.0, side.min.y + 20.0), vec2(28.0, 28.0));
    crate::brand::paint_icon(ui, mark);
    ui.painter().text(pos2(mark.max.x + 10.0, mark.center().y), Align2::LEFT_CENTER, "PhotoCraft", crate::theme::semibold(16.0), t.text);
    let mut y = side.min.y + 70.0;
    let home = Rect::from_min_size(pos2(x0, y), vec2(w, ROW_H + 2.0));
    let nav = Nav { icon: "house", text: page_title(), count: None, selected: app.ui.chrome.home_folder.is_none() };
    if nav_row(ui, home, egui::Id::new("home-nav-recent"), nav, t).clicked() {
        action = Some(Action::ShowRecent);
    }
    y += ROW_H + 24.0;
    // Folders.
    let head = tl!("Folders").to_uppercase();
    ui.painter().text(pos2(x0 + 8.0, y), Align2::LEFT_CENTER, head, crate::theme::semibold(11.0), t.text_faint);
    let plus = Rect::from_center_size(pos2(x0 + w - 12.0, y), vec2(24.0, 24.0));
    let pr = ui.interact(plus, egui::Id::new("home-folder-new"), Sense::click());
    pr.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tl!("New Folder…")));
    if pr.hovered() {
        ui.painter().rect_filled(plus, t.radius_sm, t.hover);
    }
    crate::icons::paint(ui, plus, "plus", 15.0, if pr.hovered() { t.text } else { t.text_dim });
    if pr.on_hover_text(tl!("New Folder…")).clicked() {
        action = Some(Action::NewFolder);
    }
    y += 18.0;
    let footer_h = 112.0;
    let list = Rect::from_min_max(pos2(side.min.x, y), pos2(side.max.x, (side.max.y - footer_h).max(y)));
    let folders = folder_rows(app);
    if folders.is_empty() {
        let g = ui.painter().layout(
            tl!("Folders keep a project's files together and set where they're saved.").to_string(),
            FontId::proportional(12.0),
            t.text_faint,
            w - 16.0,
        );
        let h = g.size().y;
        ui.painter().galley(pos2(x0 + 8.0, y + 6.0), g, t.text_faint);
        let b = Rect::from_min_size(pos2(x0 + 8.0, y + 16.0 + h), vec2((w - 16.0).min(170.0), 28.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(b).id_salt("home-folder-new-big"));
        if crate::widgets::secondary_button(&mut child, tl!("New Folder…"), b.width()).clicked() {
            action = Some(Action::NewFolder);
        }
    } else {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list).id_salt("home-folders"));
        egui::ScrollArea::vertical().id_salt("home-folders-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
            for f in &folders {
                let (Some(id), Some(name)) = (f["id"].as_str(), f["name"].as_str()) else { continue };
                let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
                let r = Rect::from_min_max(pos2(x0, row.min.y), pos2(x0 + w, row.max.y));
                let selected = app.ui.chrome.home_folder.as_deref() == Some(id);
                let exists = f["exists"].as_bool().unwrap_or(false);
                let icon = if !exists {
                    "triangle-alert"
                } else if selected {
                    "folder-open"
                } else {
                    "folder"
                };
                let count = f["count"].as_u64().map(|n| n as usize);
                let resp = nav_row(ui, r, egui::Id::new(("home-nav-folder", id)), Nav { icon, text: name, count, selected }, t);
                if ui.is_rect_visible(r) {
                    app.home.sidebar.push((id.to_string(), r));
                }
                let tip = f["path"].as_str().unwrap_or_default().to_string();
                let resp = if exists { resp.on_hover_text(tip) } else { resp.on_hover_text(format!("{tip}\n{}", tl!("This folder isn't on disk any more."))) };
                if resp.clicked() {
                    action = Some(Action::ShowFolder(id.to_string()));
                }
                resp.context_menu(|ui| {
                    ui.set_min_width(200.0);
                    if ui.button(tl!("Open")).clicked() {
                        action = Some(Action::ShowFolder(id.to_string()));
                        ui.close();
                    }
                    if ui.button(tl!("Rename…")).clicked() {
                        action = Some(Action::Rename(id.to_string()));
                        ui.close();
                    }
                    if ui.button(tl!("Folder Paths…")).clicked() {
                        action = Some(Action::Paths(id.to_string()));
                        ui.close();
                    }
                    if ui.add_enabled(exists, egui::Button::new(reveal_label())).clicked() {
                        action = Some(Action::Reveal(f["path"].as_str().unwrap_or_default().to_string()));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(tl!("Remove from Home")).clicked() {
                        action = Some(Action::RemoveFolder(id.to_string()));
                        ui.close();
                    }
                });
                ui.add_space(2.0);
            }
        });
    }
    // Footer: community links.
    let foot = Rect::from_min_max(pos2(x0 + 4.0, side.max.y - footer_h + 12.0), pos2(x0 + w - 4.0, side.max.y - 8.0));
    ui.painter().line_segment([pos2(side.min.x + 12.0, foot.min.y - 6.0), pos2(side.max.x - 12.0, foot.min.y - 6.0)], Stroke::new(1.0, t.separator));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(foot).id_salt("home-footer"));
    child.spacing_mut().item_spacing = vec2(6.0, 6.0);
    crate::links::discord_button(app, &mut child, foot.width());
    let rows: [&[(&str, &str)]; 2] =
        [&[(tl!("PhotoCraft website"), crate::links::APP_PAGE)], &[(tl!("GitHub"), crate::links::GITHUB), (tl!("ArtCraft"), crate::links::ARTCRAFT_WEBSITE)]];
    let mut clicked = None;
    child.add_space(2.0);
    for links in rows {
        child.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (i, (text, url)) in links.iter().enumerate() {
                if i > 0 {
                    ui.label(egui::RichText::new("  ·  ").size(11.5).color(t.text_faint));
                }
                if ui.link(egui::RichText::new(*text).size(11.5).color(t.accent)).on_hover_text(*url).clicked() {
                    clicked = Some(*url);
                }
            }
        });
    }
    if let Some(url) = clicked {
        crate::links::open(app, ui.ctx(), url);
    }
    action
}

/// A big action tile on the start view.
struct Tile<'a> {
    id: &'a str,
    icon: &'a str,
    title: &'a str,
    sub: &'a str,
    shortcut: Option<String>,
    primary: bool,
}

/// Draw an action tile (New document…, Open…); returns whether it was clicked.
fn tile(ui: &mut egui::Ui, r: Rect, tile: Tile<'_>, t: &Tokens) -> bool {
    let Tile { id, icon, title, sub, shortcut, primary } = tile;
    let resp = ui.interact(r, egui::Id::new(id), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, title));
    let hovered = resp.hovered();
    let (bg, fg, sub_col) = if primary {
        let bg = if hovered { t.primary_bg.gamma_multiply(0.88) } else { t.primary_bg };
        (bg, t.primary_text, t.primary_text.gamma_multiply(0.78))
    } else {
        (if hovered { t.hover } else { t.card }, t.text, t.text_faint)
    };
    ui.painter().rect_filled(r, t.radius_lg, bg);
    if !primary {
        ui.painter().rect_stroke(r, t.radius_lg, Stroke::new(1.0, if hovered { t.accent } else { t.card_border }), StrokeKind::Inside);
    }
    let disc = pos2(r.min.x + 34.0, r.center().y);
    ui.painter().circle_filled(disc, 19.0, if primary { Color32::from_white_alpha(38) } else { t.field });
    crate::icons::paint(ui, Rect::from_center_size(disc, vec2(20.0, 20.0)), icon, 19.0, fg);
    let mut right = r.max.x - 14.0;
    if let Some(sc) = shortcut {
        let g = ui.painter().layout_no_wrap(sc, FontId::proportional(11.5), sub_col);
        right -= g.size().x;
        ui.painter().galley(pos2(right, r.min.y + 18.0 - g.size().y / 2.0 + 6.0), g, sub_col);
        right -= 8.0;
    }
    let x = r.min.x + 64.0;
    label(ui, title, pos2(x, r.center().y - 9.0), crate::theme::semibold(14.5), fg, right - x);
    label(ui, sub, pos2(x, r.center().y + 11.0), FontId::proportional(12.0), sub_col, r.max.x - 14.0 - x);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// The start view: title, New document… / Open…, then the recently opened files.
fn start_view(app: &mut PhotocraftApp, ui: &mut egui::Ui, main: Rect, t: &Tokens) -> Option<Action> {
    let mut action = None;
    let x = main.min.x + PAD;
    let inner_w = (main.width() - 2.0 * PAD).max(120.0);
    let p = ui.painter();
    p.text(pos2(x, main.min.y + 46.0), Align2::LEFT_CENTER, page_title(), crate::theme::semibold(26.0), t.text);
    p.text(pos2(x, main.min.y + 76.0), Align2::LEFT_CENTER, tl!("Create a new document or open an existing file."), FontId::proportional(13.5), t.text_dim);
    let tile_w = ((inner_w - 16.0) / 2.0).min(340.0);
    let tiles_y = main.min.y + 104.0;
    let new_r = Rect::from_min_size(pos2(x, tiles_y), vec2(tile_w, 74.0));
    let open_r = Rect::from_min_size(pos2(x + tile_w + 16.0, tiles_y), vec2(tile_w, 74.0));
    let new_tile = Tile {
        id: "home-new",
        icon: "file-plus",
        title: tl!("New document…"),
        sub: tl!("Presets or a custom size"),
        shortcut: crate::shortcuts::shortcut_label(app, "file.new"),
        primary: true,
    };
    if tile(ui, new_r, new_tile, t) {
        action = Some(Action::NewDocument(None));
    }
    let open_tile = Tile {
        id: "home-open",
        icon: "folder-open",
        title: tl!("Open…"),
        sub: tl!("PSD, PhotoCraft, PNG, JPEG and more"),
        shortcut: crate::shortcuts::shortcut_label(app, "file.open"),
        primary: false,
    };
    if tile(ui, open_r, open_tile, t) {
        action = Some(Action::Open);
    }
    let hint_y = tiles_y + 74.0 + 22.0;
    let hint = drop_hint(app);
    crate::icons::paint(ui, Rect::from_center_size(pos2(x + 8.0, hint_y), vec2(16.0, 16.0)), "image", 14.0, t.text_faint);
    label(ui, &hint, pos2(x + 24.0, hint_y), FontId::proportional(12.5), t.text_faint, inner_w - 24.0);
    if !folders_supported() {
        return action;
    }
    // Recently opened.
    let rows = app.home.recent.as_ref().map(|c| c.rows.clone()).unwrap_or_default();
    let head_y = hint_y + 52.0;
    let w = label(ui, tl!("Recently opened"), pos2(x, head_y), crate::theme::semibold(16.0), t.text, inner_w);
    if !rows.is_empty() {
        let n = rows.len().to_string();
        let g = ui.painter().layout_no_wrap(n, crate::theme::semibold(11.0), t.text_dim);
        let pill = Rect::from_min_size(pos2(x + w + 10.0, head_y - 9.0), vec2(g.size().x + 14.0, 18.0));
        ui.painter().rect_filled(pill, 9.0, t.card);
        ui.painter().galley(pill.center() - g.size() / 2.0, g, t.text_dim);
    }
    let grid = Rect::from_min_max(pos2(x, head_y + 22.0), pos2(main.max.x - PAD + 12.0, main.max.y - 8.0));
    if rows.is_empty() {
        home_cards::empty_state(ui, grid, "clock", tl!("No recent files yet"), tl!("Files you open or save show up here, newest first."));
        return action;
    }
    let folders = folder_rows(app);
    if let Some(a) = home_cards::grid(app, ui, grid, &rows, Listing::Recent, &folders) {
        action = Some(Action::Card(a));
    }
    action
}

/// A path as Home shows it: `~/…` in the home directory.
pub(crate) fn display_path(path: &str) -> String {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(|h| h.to_string_lossy().into_owned()).filter(|h| !h.is_empty())
        && let Some(rest) = path.strip_prefix(&home)
        && (rest.is_empty() || rest.starts_with(['/', '\\']))
    {
        return format!("~{rest}");
    }
    path.to_string()
}

/// A folder's view: name, actions, its three paths, then its files.
fn folder_view(app: &mut PhotocraftApp, ui: &mut egui::Ui, main: Rect, id: &str, t: &Tokens) -> Option<Action> {
    let mut action = None;
    let folders = folder_rows(app);
    let Some(f) = folders.iter().find(|f| f["id"].as_str() == Some(id)).cloned() else { return Some(Action::ShowRecent) };
    let name = f["name"].as_str().unwrap_or_default().to_string();
    let path = f["path"].as_str().unwrap_or_default().to_string();
    let exists = f["exists"].as_bool().unwrap_or(false);
    let rows = app.home.files.as_ref().filter(|(fid, _)| fid == id).map(|(_, c)| c.rows.clone()).unwrap_or_default();
    let x = main.min.x + PAD;
    let inner_w = (main.width() - 2.0 * PAD).max(120.0);
    // Title.
    let ty = main.min.y + 46.0;
    crate::icons::paint(ui, Rect::from_center_size(pos2(x + 14.0, ty), vec2(28.0, 28.0)), "folder-open", 26.0, t.accent);
    let w = label(ui, &name, pos2(x + 38.0, ty), crate::theme::semibold(26.0), t.text, inner_w - 160.0);
    let count = crate::i18n::trn(crate::i18n::current(), rows.len() as u64, "{n} file", "{n} files");
    ui.painter().text(pos2(x + 38.0 + w + 14.0, ty + 3.0), Align2::LEFT_CENTER, count, FontId::proportional(13.0), t.text_faint);
    // Actions.
    let bar = Rect::from_min_size(pos2(x, ty + 30.0), vec2(inner_w, 32.0));
    let mut child =
        ui.new_child(egui::UiBuilder::new().max_rect(bar).layout(egui::Layout::left_to_right(egui::Align::Center)).id_salt(("home-folder-actions", id)));
    child.spacing_mut().item_spacing.x = 10.0;
    if crate::widgets::primary_button(&mut child, tl!("New Document Here…"), 150.0).clicked() {
        action = Some(Action::NewDocument(Some(id.to_string())));
    }
    if crate::widgets::secondary_button(&mut child, tl!("Open…"), 90.0).clicked() {
        action = Some(Action::Open);
    }
    if child.add_enabled_ui(exists, |ui| crate::widgets::secondary_button(ui, reveal_label(), 110.0)).inner.clicked() {
        action = Some(Action::Reveal(path.clone()));
    }
    if crate::widgets::secondary_button(&mut child, tl!("Folder Paths…"), 110.0).clicked() {
        action = Some(Action::Paths(id.to_string()));
    }
    // The three paths; a click picks another directory.
    let chips_y = bar.max.y + 18.0;
    let chip_w = ((inner_w - 24.0) / 3.0).max(80.0);
    let kinds = [
        (DirKind::Projects, tl!("Projects"), "path", "exportsCustom"),
        (DirKind::Exports, tl!("Exports"), "exports", "exportsCustom"),
        (DirKind::Resources, tl!("Resources"), "resources", "resourcesCustom"),
    ];
    for (i, (kind, title, key, custom_key)) in kinds.iter().enumerate() {
        let r = Rect::from_min_size(pos2(x + i as f32 * (chip_w + 12.0), chips_y), vec2(chip_w, 50.0));
        let dir = f[*key].as_str().unwrap_or_default().to_string();
        let same = *kind != DirKind::Projects && f[*custom_key].as_bool() != Some(true);
        let resp = ui.interact(r, egui::Id::new(("home-path", id, kind.name())), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, *title));
        let hovered = resp.hovered();
        ui.painter().rect_filled(r, t.radius, if hovered { t.hover } else { t.card });
        ui.painter().rect_stroke(r, t.radius, Stroke::new(1.0, if hovered { t.accent } else { t.card_border }), StrokeKind::Inside);
        crate::icons::paint(ui, Rect::from_center_size(pos2(r.min.x + 20.0, r.center().y), vec2(18.0, 18.0)), "folder", 16.0, t.text_dim);
        let tx = r.min.x + 38.0;
        let head = if same { format!("{}  ·  {}", title.to_uppercase(), tl!("same as Projects")) } else { title.to_uppercase() };
        label(ui, &head, pos2(tx, r.min.y + 16.0), crate::theme::semibold(10.5), t.text_faint, r.max.x - 10.0 - tx);
        tail_label(ui, &display_path(&dir), pos2(tx, r.min.y + 34.0), FontId::proportional(12.5), t.text, r.max.x - 10.0 - tx);
        let tip = format!("{dir}\n{}", tl!("Click to choose another folder."));
        if resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            action = Some(Action::PickDir(id.to_string(), *kind));
        }
    }
    let mut y = chips_y + 50.0 + 26.0;
    if !exists {
        let banner = Rect::from_min_size(pos2(x, y - 10.0), vec2(inner_w, 40.0));
        ui.painter().rect_filled(banner, t.radius, t.warning.gamma_multiply(0.18));
        crate::icons::paint(ui, Rect::from_center_size(pos2(banner.min.x + 18.0, banner.center().y), vec2(16.0, 16.0)), "triangle-alert", 15.0, t.warning);
        label(
            ui,
            tl!("This folder isn't on disk any more. Choose where it is now with Projects above."),
            pos2(banner.min.x + 36.0, banner.center().y),
            FontId::proportional(12.5),
            t.text,
            banner.width() - 48.0,
        );
        y += 44.0;
    }
    label(ui, tl!("Files"), pos2(x, y), crate::theme::semibold(16.0), t.text, inner_w);
    let grid = Rect::from_min_max(pos2(x, y + 22.0), pos2(main.max.x - PAD + 12.0, main.max.y - 8.0));
    if rows.is_empty() {
        home_cards::empty_state(ui, grid, "folder-open", tl!("This folder is empty"), tl!("Save a document here, or add files to it from Recent."));
        return action;
    }
    if let Some(a) = home_cards::grid(app, ui, grid, &rows, Listing::Folder(id), &folders) {
        action = Some(Action::Card(a));
    }
    action
}

/// Show `path` in the system file manager.
fn reveal(app: &mut PhotocraftApp, path: &str) {
    let r = match app.services.reveal.as_ref() {
        Some(f) => f(path),
        None => Err(tl!("Showing files in the file manager isn't available here.").to_string()),
    };
    if let Err(e) = r {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// Run a Home command for a user action; errors go to the status bar.
fn run(app: &mut PhotocraftApp, id: &str, params: Value) -> Option<Value> {
    let r = app.run(id, params).ok();
    app.home.stale = true;
    r
}

fn apply(app: &mut PhotocraftApp, a: Action) {
    match a {
        Action::Card(CardAction::Open { path, folder }) => {
            if let Some(id) = folder {
                open_into(app, std::slice::from_ref(&path), &id);
            }
            if let Err(e) = app.open_path(&path) {
                app.open_failed(&crate::file_open::display_name(&path), &e);
            }
            app.home.stale = true;
        }
        Action::Card(CardAction::Activate(i)) => {
            if app.session.set_active(i) {
                app.ui.chrome.home = None;
                app.sync_views();
            }
        }
        Action::Card(CardAction::Forget(path)) => {
            run(app, "home.forgetRecent", json!({"path": path}));
        }
        Action::Card(CardAction::Reveal(path)) | Action::Reveal(path) => reveal(app, &path),
        Action::Card(CardAction::AddTo(path, id)) => {
            if run(app, "home.folderAdd", json!({"id": id, "path": path})).is_some() {
                app.ui.status = crate::i18n::fmt(tl!("Added {name} to the folder."), &[("name", &crate::file_open::display_name(&path))]);
                app.ui.status_error = false;
            }
        }
        Action::Card(CardAction::RemoveFrom(id, path)) => {
            run(app, "home.folderRemove", json!({"id": id, "path": path}));
        }
        Action::Card(CardAction::Missing(path)) => {
            app.ui.status =
                crate::i18n::fmt(tl!("{name} isn't there any more: it was moved, renamed or deleted."), &[("name", &crate::file_open::display_name(&path))]);
            app.ui.status_error = true;
        }
        Action::NewDocument(folder) => {
            let mut fields = app.new_document_fields();
            if let Some(id) = folder {
                fields.insert("__homeFolder".into(), json!(id));
            }
            app.ui.open_dialog(crate::state::DialogKind::NewDocument, fields);
        }
        Action::Open => {
            // Answered on a later frame (see `file_dialog`); cancelling is not an error.
            let _ = app.open_dialog_file();
        }
        Action::ShowRecent => app.ui.chrome.home_folder = None,
        Action::ShowFolder(id) => {
            app.ui.chrome.home_folder = Some(id);
            app.home.stale = true;
        }
        Action::NewFolder => crate::home_dialogs::open_new_folder(app),
        Action::Rename(id) => crate::home_dialogs::open_rename(app, &id),
        Action::Paths(id) => crate::home_dialogs::open_paths(app, &id),
        Action::RemoveFolder(id) => {
            if run(app, "home.folderDelete", json!({"id": id})).is_some() && app.ui.chrome.home_folder.as_deref() == Some(id.as_str()) {
                app.ui.chrome.home_folder = None;
            }
        }
        Action::PickDir(id, kind) => {
            let f = folder_rows(app).into_iter().find(|f| f["id"].as_str() == Some(id.as_str()));
            let key = match kind {
                DirKind::Projects => "path",
                DirKind::Exports => "exports",
                DirKind::Resources => "resources",
            };
            let start = f.as_ref().and_then(|f| f[key].as_str()).map(str::to_string);
            let start = start.filter(|d| photocraft_engine::home_cmds::is_dir(d));
            // The folder dialog answers on a later frame (see `file_dialog`).
            let r = app.pick_folder(start, move |app, dir| {
                let mut params = json!({"id": id});
                params[key] = json!(dir);
                let r = app.run("home.folderEdit", params);
                app.home.stale = true;
                r
            });
            if let Err(e) = r
                && e != crate::file_dialog::CANCELLED
            {
                app.ui.status = e;
                app.ui.status_error = true;
            }
        }
    }
}

/// A Home view `ui.set {home}` asks for (see [`parse_view`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    /// Leave Home.
    Hidden,
    /// The start view with the recent files.
    Recent,
    /// A folder's view.
    Folder(String),
}

/// `ui.set {home}`'s value: `false` (or `null`) leaves Home, `"recent"` (or `true`) is the start
/// view, `"folder:<id>"` a folder's view. An unknown folder or any other value is an error, so
/// `ui.set` can refuse the whole call before it changes anything.
pub fn parse_view(app: &PhotocraftApp, v: &Value) -> Result<View, String> {
    match v {
        Value::Bool(false) | Value::Null => Ok(View::Hidden),
        Value::Bool(true) => Ok(View::Recent),
        Value::String(s) if s == "recent" => Ok(View::Recent),
        Value::String(s) if s.starts_with("folder:") => {
            let id = s.trim_start_matches("folder:");
            if !app.session.prefs().home.folders.iter().any(|f| f.id == id) {
                return Err(format!("no Home folder `{id}`"));
            }
            Ok(View::Folder(id.to_string()))
        }
        _ => Err("`home` must be false, \"recent\" or \"folder:<id>\"".into()),
    }
}

/// Show `view` (see [`parse_view`]).
pub fn apply_view(app: &mut PhotocraftApp, view: View) {
    match view {
        View::Hidden => {
            app.ui.chrome.home = None;
            return;
        }
        View::Recent => app.ui.chrome.home_folder = None,
        View::Folder(id) => app.ui.chrome.home_folder = Some(id),
    }
    app.ui.chrome.home = Some(app.session.documents().len());
    app.home.stale = true;
}

/// `ui.set {home}`: [`parse_view`], then [`apply_view`].
pub fn set_view(app: &mut PhotocraftApp, v: &Value) -> Result<(), String> {
    let view = parse_view(app, v)?;
    apply_view(app, view);
    Ok(())
}

/// `ui.inspect` › `home`: whether Home is up, its view, and where its cards and folder rows are
/// (screen points, `[x, y, width, height]`) so agents can click them.
pub fn inspect(app: &PhotocraftApp) -> Value {
    let rect = |r: &Rect| json!([r.min.x, r.min.y, r.width(), r.height()]);
    let view = match &app.ui.chrome.home_folder {
        Some(id) => format!("folder:{id}"),
        None => "recent".into(),
    };
    json!({
        "shown": shown(app),
        "view": view,
        "folder": app.ui.chrome.home_folder,
        "cards": app.home.cards.iter().map(|c| json!({"path": c.path, "name": c.name, "rect": rect(&c.rect)})).collect::<Vec<_>>(),
        "folders": app.home.sidebar.iter().map(|(id, r)| json!({"id": id, "rect": rect(r)})).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
#[path = "home_ui_tests.rs"]
mod tests;
