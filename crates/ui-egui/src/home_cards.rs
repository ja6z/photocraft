//! Home's file cards (recents and folder listings) and their thumbnails.
//!
//! A card is the file's thumbnail (or a placeholder with its type), its name on one line ending in
//! "…" when long, and when it was opened (or modified). Already-open files carry an "Open" badge
//! and switch to their tab; moved files read "File missing". Thumbnails are PNGs the engine keeps
//! in the thumbnail folder (`home_thumbs`): this module decodes them into textures, a few per
//! frame, and renders missing ones on a background thread (a document's on save and open, a
//! cheap preview for files never opened here).

use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashSet;
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::Value;

use crate::PhotocraftApp;
use crate::theme::Tokens;

/// Smallest and largest card width; cards stretch between them to fill a row.
const CARD_MIN_W: f32 = 196.0;
const CARD_MAX_W: f32 = 264.0;
/// Gap between cards.
pub(crate) const GAP: f32 = 20.0;
/// Height of the text under a card's thumbnail.
const META_H: f32 = 50.0;
/// Thumbnail textures decoded per frame (the rest come on the next frames).
const LOADS_PER_FRAME: usize = 6;

/// What a click on a card (or its menu) asks for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CardAction {
    /// Open the file (into `folder` when the card is in a folder's listing).
    Open { path: String, folder: Option<String> },
    /// Switch to the document that has the file open.
    Activate(usize),
    /// Remove from Recent.
    Forget(String),
    /// Show in Finder.
    Reveal(String),
    /// Add to folder `.1`.
    AddTo(String, String),
    /// Take a file added from elsewhere out of folder `.0`.
    RemoveFrom(String, String),
    /// The file is missing.
    Missing(String),
}

/// Which listing the cards belong to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Listing<'a> {
    Recent,
    Folder(&'a str),
}

/// A card laid out this frame (`ui.inspect` reports them so agents can click one).
#[derive(Clone, Debug)]
pub(crate) struct CardRect {
    pub path: String,
    pub name: String,
    pub rect: Rect,
}

/// Background thumbnail work.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) enum Job {
    /// Render an open document's thumbnail (on save and on open).
    Document { doc: std::sync::Arc<photocraft_doc::Document>, out: PathBuf },
    /// A preview of a file never opened here (`home_thumbs::file_preview_png`).
    Preview { src: String, out: PathBuf },
}

#[cfg(not(target_arch = "wasm32"))]
struct Worker {
    jobs: std::sync::mpsc::Sender<Job>,
    done: std::sync::mpsc::Receiver<PathBuf>,
}

/// Thumbnail textures and the worker that renders thumbnails.
#[derive(Default)]
pub(crate) struct Thumbs {
    /// Texture of each thumbnail file (`None`: it couldn't be decoded).
    textures: HashMap<String, Option<egui::TextureHandle>>,
    /// Files whose preview was asked for this session (asked once each).
    #[cfg(not(target_arch = "wasm32"))]
    requested: HashSet<String>,
    /// Jobs sent and not finished yet.
    pending: usize,
    /// Textures decoded this frame.
    loads: usize,
    /// Asked for a repaint by the worker when a thumbnail is ready.
    repaint: std::sync::Arc<std::sync::Mutex<Option<egui::Context>>>,
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<Worker>,
}

impl Thumbs {
    /// Start of a frame on Home: the worker's finished thumbnails replace their textures.
    /// Returns whether any finished (the listings then show the new files).
    pub(crate) fn poll(&mut self, ctx: &egui::Context) -> bool {
        self.loads = 0;
        if let Ok(mut slot) = self.repaint.lock() {
            *slot = Some(ctx.clone());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut any = false;
            if let Some(w) = &self.worker {
                while let Ok(out) = w.done.try_recv() {
                    self.textures.remove(&out.to_string_lossy().into_owned());
                    self.pending = self.pending.saturating_sub(1);
                    any = true;
                }
            }
            any
        }
        #[cfg(target_arch = "wasm32")]
        false
    }

    /// Queue background thumbnail work (no-op on the web).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn queue(&mut self, job: Job) {
        if self.worker.is_none() {
            self.worker = spawn(self.repaint.clone());
        }
        let Some(w) = &self.worker else { return };
        if w.jobs.send(job).is_ok() {
            self.pending += 1;
        }
    }

    /// Ask once for a preview of `src` (a file listed on Home without a fresh thumbnail).
    #[cfg(not(target_arch = "wasm32"))]
    fn request_preview(&mut self, src: &str, out: PathBuf) {
        if self.requested.insert(src.to_string()) {
            self.queue(Job::Preview { src: src.to_string(), out });
        }
    }

    /// Wait until the worker has finished everything queued (tests), at most `timeout`.
    pub(crate) fn wait_idle(&mut self, timeout: std::time::Duration) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let end = std::time::Instant::now() + timeout;
            while self.pending > 0 {
                let Some(w) = &self.worker else { break };
                let left = end.saturating_duration_since(std::time::Instant::now());
                match w.done.recv_timeout(left) {
                    Ok(out) => {
                        self.textures.remove(&out.to_string_lossy().into_owned());
                        self.pending = self.pending.saturating_sub(1);
                    }
                    Err(_) => return false,
                }
            }
        }
        let _ = timeout;
        self.pending == 0
    }

    /// The texture of the thumbnail file `file`, decoding it if this frame's budget allows.
    fn texture(&mut self, ctx: &egui::Context, file: &str) -> Option<egui::TextureHandle> {
        if let Some(t) = self.textures.get(file) {
            return t.clone();
        }
        if self.loads >= LOADS_PER_FRAME {
            ctx.request_repaint();
            return None;
        }
        self.loads += 1;
        let tex = decode_texture(ctx, file);
        self.textures.insert(file.to_string(), tex.clone());
        tex
    }
}

/// Start the thumbnail worker thread.
#[cfg(not(target_arch = "wasm32"))]
fn spawn(repaint: std::sync::Arc<std::sync::Mutex<Option<egui::Context>>>) -> Option<Worker> {
    let (jobs, rx) = std::sync::mpsc::channel::<Job>();
    let (tx, done) = std::sync::mpsc::channel::<PathBuf>();
    let spawned = std::thread::Builder::new().name("home-thumbnails".into()).spawn(move || {
        for job in rx {
            let out = match &job {
                Job::Document { out, .. } | Job::Preview { out, .. } => out.clone(),
            };
            // A codec bug must not take the app down: the thumbnail is only a cache.
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(job)));
            match r {
                Ok(Err(e)) => log::debug!("home thumbnail {}: {e}", out.display()),
                Err(_) => log::warn!("home thumbnail {}: the renderer panicked", out.display()),
                Ok(Ok(())) => {}
            }
            if tx.send(out).is_err() {
                break;
            }
            if let Some(ctx) = repaint.lock().ok().and_then(|c| c.clone()) {
                ctx.request_repaint();
            }
        }
    });
    match spawned {
        Ok(_) => Some(Worker { jobs, done }),
        Err(e) => {
            log::warn!("no Home thumbnail worker: {e}");
            None
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn run(job: Job) -> Result<(), String> {
    match job {
        Job::Document { doc, out } => photocraft_engine::home_thumbs::write_document_thumbnail(&doc, &out).map_err(|e| e.to_string()),
        Job::Preview { src, out } => {
            // A document thumbnail written since this was queued is better than any preview.
            let modified = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
            if let (Some(made), Some(changed)) = (modified(&out), modified(std::path::Path::new(&src)))
                && made >= changed
            {
                return Ok(());
            }
            photocraft_engine::home_thumbs::write_file_preview(&src, &out).map(|_| ()).map_err(|e| e.to_string())
        }
    }
}

/// Decode a thumbnail PNG into a texture (`None` when it can't be read).
#[cfg(not(target_arch = "wasm32"))]
fn decode_texture(ctx: &egui::Context, file: &str) -> Option<egui::TextureHandle> {
    use std::io::Read;
    let mut bytes = Vec::new();
    // Thumbnails are small; a huge file there is not one of ours.
    std::fs::File::open(file).ok()?.take(8 << 20).read_to_end(&mut bytes).ok()?;
    let img = photocraft_codecs::decode(&bytes).ok()?.convert(photocraft_codecs::ChannelLayout::Rgba, photocraft_codecs::SampleType::U8);
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w == 0 || h == 0 || img.data().len() != w * h * 4 {
        return None;
    }
    let color = egui::ColorImage::from_rgba_unmultiplied([w, h], img.data());
    let options = egui::TextureOptions { mipmap_mode: Some(egui::TextureFilter::Linear), ..egui::TextureOptions::LINEAR };
    Some(ctx.load_texture(format!("home-thumb-{file}"), color, options))
}
#[cfg(target_arch = "wasm32")]
fn decode_texture(_ctx: &egui::Context, _file: &str) -> Option<egui::TextureHandle> {
    None
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// "Opened 12 min ago", "Opened today at 14:05", "Opened yesterday at 09:30", "Opened 2026-10-08"
/// (`opened`), or the same with "Modified".
pub(crate) fn when_label(now: i64, at: i64, offset: Option<fn(i64) -> i32>, opened: bool) -> String {
    let off = |s: i64| offset.map_or(0, |f| i64::from(f(s)));
    let ago = now.saturating_sub(at);
    if (-300..60).contains(&ago) {
        return if opened { tl!("Opened just now") } else { tl!("Modified just now") }.to_string();
    }
    if (60..3600).contains(&ago) {
        let n = (ago / 60).to_string();
        return crate::i18n::fmt(if opened { tl!("Opened {n} min ago") } else { tl!("Modified {n} min ago") }, &[("n", &n)]);
    }
    let local = at.saturating_add(off(at));
    let today = now.saturating_add(off(now)).div_euclid(86_400);
    let day = local.div_euclid(86_400);
    let time = format!("{:02}:{:02}", local.rem_euclid(86_400) / 3600, local.rem_euclid(3600) / 60);
    match today - day {
        0 => crate::i18n::fmt(if opened { tl!("Opened today at {time}") } else { tl!("Modified today at {time}") }, &[("time", &time)]),
        1 => crate::i18n::fmt(if opened { tl!("Opened yesterday at {time}") } else { tl!("Modified yesterday at {time}") }, &[("time", &time)]),
        _ => {
            let (y, m, d) = civil_from_days(day);
            let date = format!("{y:04}-{m:02}-{d:02}");
            crate::i18n::fmt(if opened { tl!("Opened {date}") } else { tl!("Modified {date}") }, &[("date", &date)])
        }
    }
}

/// A card's title: one line; a long name ends in "…" (the full path is the card's tooltip).
pub(crate) fn title_job(name: &str, font: FontId, color: Color32, width: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::simple_singleline(name.to_string(), font, color);
    job.wrap = egui::text::TextWrapping { max_width: width.max(1.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    job
}

/// Card width, columns and card height for a grid `width` wide.
pub(crate) fn layout(width: f32) -> (f32, usize, f32) {
    let cols = (((width + GAP) / (CARD_MIN_W + GAP)).floor() as usize).max(1);
    let card_w = ((width - GAP * (cols as f32 - 1.0)) / cols as f32).clamp(CARD_MIN_W.min(width.max(1.0)), CARD_MAX_W);
    (card_w, cols, thumb_h(card_w) + META_H)
}

fn thumb_h(card_w: f32) -> f32 {
    (card_w * 0.66).round()
}

/// The cards of `rows` in a scrolling grid filling `rect`; returns what was clicked.
pub(crate) fn grid(app: &mut PhotocraftApp, ui: &mut egui::Ui, rect: Rect, rows: &[Value], listing: Listing<'_>, folders: &[Value]) -> Option<CardAction> {
    let mut action = None;
    let now = photocraft_engine::home_cmds::unix_now().unwrap_or(0);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(("home-grid", listing == Listing::Recent)));
    let id = match listing {
        Listing::Recent => egui::Id::new("home-grid-recent"),
        Listing::Folder(f) => egui::Id::new(("home-grid-folder", f)),
    };
    egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show(&mut child, |ui| {
        let (card_w, cols, card_h) = layout(ui.available_width() - 4.0);
        for chunk in rows.chunks(cols) {
            ui.add_space(2.0);
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), card_h), Sense::hover());
            for (c, r) in chunk.iter().enumerate() {
                let card = Rect::from_min_size(pos2(row.min.x + 2.0 + c as f32 * (card_w + GAP), row.min.y), vec2(card_w, card_h));
                if let Some(a) = card_ui(app, ui, card, r, listing, folders, now) {
                    action = Some(a);
                }
            }
            ui.add_space(GAP - 2.0);
        }
    });
    action
}

/// One card; returns what was asked for.
fn card_ui(app: &mut PhotocraftApp, ui: &mut egui::Ui, card: Rect, r: &Value, listing: Listing<'_>, folders: &[Value], now: i64) -> Option<CardAction> {
    let t = Tokens::get(ui.ctx());
    let path = r["path"].as_str().unwrap_or_default().to_string();
    let name = r["name"].as_str().unwrap_or_default().to_string();
    let ext = r["ext"].as_str().unwrap_or_default().to_ascii_uppercase();
    let exists = r["exists"].as_bool().unwrap_or(false);
    let document = r["document"].as_u64().map(|d| d as usize);
    let folder = r["folder"].as_str().map(str::to_string);
    let resp = ui.interact(card, egui::Id::new(("home-card", &path, listing == Listing::Recent)), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
    let resp = resp.on_hover_text(&path).on_hover_cursor(egui::CursorIcon::PointingHand);
    if ui.is_rect_visible(card) {
        app.home.cards.push(CardRect { path: path.clone(), name: name.clone(), rect: card });
    }
    let thumb = Rect::from_min_size(card.min, vec2(card.width(), thumb_h(card.width())));
    let painter = ui.painter_at(card.expand(3.0));
    let radius = t.radius_lg;
    painter.rect_filled(thumb, radius, t.card);
    let tex = r["thumbnail"].as_str().and_then(|f| app.home.thumbs.texture(ui.ctx(), f));
    let has_tex = tex.is_some();
    // No thumbnail yet, or older than the file: make a preview in the background.
    #[cfg(not(target_arch = "wasm32"))]
    if exists
        && (r["thumbnail"].is_null() || r["thumbnailStale"].as_bool() == Some(true))
        && let Some(out) = photocraft_engine::home_thumbs::thumbnail_path(&app.session, &path)
    {
        app.home.thumbs.request_preview(&path, out);
    }
    let dim = if exists { Color32::WHITE } else { Color32::from_white_alpha(70) };
    match tex {
        Some(tex) => {
            let [w, h] = tex.size();
            let inner = thumb.shrink(10.0);
            let k = (inner.width() / w.max(1) as f32).min(inner.height() / h.max(1) as f32);
            let img = Rect::from_center_size(inner.center(), vec2(w as f32 * k, h as f32 * k));
            crate::widgets::checker(&painter, img, 6.0);
            painter.image(tex.id(), img, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), dim);
            painter.rect_stroke(img, 0.0, Stroke::new(1.0, Color32::from_black_alpha(40)), StrokeKind::Outside);
        }
        None => {
            let icon = if matches!(ext.as_str(), "PSD" | "PSB" | "PSDT" | "PCRAFT") { "file" } else { "image" };
            let c = thumb.center() - vec2(0.0, 10.0);
            crate::icons::paint(
                ui,
                Rect::from_center_size(c, vec2(40.0, 40.0)),
                icon,
                38.0,
                if exists { t.text_faint } else { t.text_faint.gamma_multiply(0.5) },
            );
            if exists && !ext.is_empty() {
                let g = painter.layout_no_wrap(ext.clone(), crate::theme::semibold(10.5), t.text_dim);
                let pill = Rect::from_center_size(c + vec2(0.0, 36.0), g.size() + vec2(14.0, 6.0));
                painter.rect_filled(pill, pill.height() / 2.0, t.field);
                painter.galley(pill.center() - g.size() / 2.0, g, t.text_dim);
            }
        }
    }
    // The file type, bottom left of the thumbnail (also on real thumbnails: a PNG and a PSD of the
    // same flyer look alike).
    if has_tex && !ext.is_empty() {
        let g = painter.layout_no_wrap(ext.clone(), crate::theme::semibold(9.5), Color32::WHITE);
        let pill = Rect::from_min_size(pos2(thumb.min.x + 8.0, thumb.max.y - 8.0 - g.size().y - 4.0), g.size() + vec2(10.0, 4.0));
        painter.rect_filled(pill, 4.0, Color32::from_black_alpha(150));
        painter.galley(pill.min + vec2(5.0, 2.0), g, Color32::WHITE);
    }
    let hovered = resp.hovered();
    let stroke = if hovered { Stroke::new(2.0, t.accent) } else { Stroke::new(1.0, t.card_border) };
    painter.rect_stroke(thumb, radius, stroke, StrokeKind::Inside);
    if document.is_some() {
        // "Open" as in "it is open" (the menus' Open is the verb).
        let open = crate::i18n::tr_ctx(crate::i18n::current(), "homeBadge", "Open");
        let g = painter.layout_no_wrap(open.to_string(), crate::theme::semibold(10.5), t.primary_text);
        let badge = Rect::from_min_size(thumb.min + vec2(8.0, 8.0), g.size() + vec2(14.0, 6.0));
        painter.rect_filled(badge, badge.height() / 2.0, t.primary_bg);
        painter.galley(badge.center() - g.size() / 2.0, g, t.primary_text);
    }
    if !exists {
        let g = painter.layout_no_wrap(tl!("File missing").to_string(), crate::theme::semibold(10.5), Color32::WHITE);
        // Under the placeholder icon, or low on a real thumbnail.
        let at = if has_tex { thumb.center() + vec2(0.0, thumb.height() * 0.30) } else { thumb.center() + vec2(0.0, 28.0) };
        let badge = Rect::from_center_size(at, g.size() + vec2(30.0, 8.0));
        painter.rect_filled(badge, badge.height() / 2.0, t.danger.gamma_multiply(0.9));
        crate::icons::paint(ui, Rect::from_center_size(pos2(badge.min.x + 11.0, badge.center().y), vec2(12.0, 12.0)), "triangle-alert", 11.0, Color32::WHITE);
        painter.galley(pos2(badge.min.x + 21.0, badge.center().y - g.size().y / 2.0), g, Color32::WHITE);
    }
    // Text: name, then when.
    let text_col = if exists { t.text } else { t.text_faint };
    let name_w = card.width() - 4.0;
    let galley = painter.layout_job(title_job(&name, crate::theme::semibold(13.0), text_col, name_w));
    painter.galley(pos2(card.min.x + 2.0, thumb.max.y + 9.0), galley, text_col);
    let when = match (r["openedAt"].as_i64(), r["modifiedAt"].as_i64()) {
        (Some(at), _) if listing == Listing::Recent => when_label(now, at, app.services.local_offset, true),
        (_, Some(m)) => when_label(now, m, app.services.local_offset, false),
        (Some(at), None) => when_label(now, at, app.services.local_offset, true),
        (None, None) => String::new(),
    };
    let meta_y = thumb.max.y + 33.0;
    // The folder it belongs to (recents only): a chip in the thumbnail's bottom right corner.
    if listing == Listing::Recent
        && let Some(fname) = folder.as_deref().and_then(|id| folders.iter().find(|f| f["id"].as_str() == Some(id))).and_then(|f| f["name"].as_str())
    {
        let g = crate::tab_strip::elided(ui, fname, crate::theme::medium(10.5), Color32::WHITE, (card.width() * 0.5).max(30.0));
        let h = g.size().y + 6.0;
        let chip = Rect::from_min_size(pos2(thumb.max.x - 8.0 - g.size().x - 24.0, thumb.max.y - 8.0 - h), vec2(g.size().x + 24.0, h));
        painter.rect_filled(chip, 4.0, Color32::from_black_alpha(150));
        crate::icons::paint(ui, Rect::from_center_size(pos2(chip.min.x + 10.0, chip.center().y), vec2(11.0, 11.0)), "folder", 10.0, Color32::WHITE);
        painter.galley(pos2(chip.min.x + 19.0, chip.center().y - g.size().y / 2.0), g, Color32::WHITE);
    }
    let g = crate::tab_strip::elided(ui, &when, FontId::proportional(11.5), t.text_faint, (card.width() - 4.0).max(10.0));
    painter.galley(pos2(card.min.x + 2.0, meta_y - g.size().y / 2.0), g, t.text_faint);

    // Remove from Recent (hover ×).
    let mut action = None;
    let x = Rect::from_center_size(pos2(thumb.max.x - 16.0, thumb.min.y + 16.0), vec2(22.0, 22.0));
    if listing == Listing::Recent && (hovered || ui.rect_contains_pointer(x)) {
        let xr = ui.interact(x, egui::Id::new(("home-forget", &path)), Sense::click());
        xr.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, crate::i18n::fmt(tl!("Remove {name} from Recent"), &[("name", &name)])));
        let xr = xr.on_hover_text(tl!("Remove from Recent"));
        painter.circle_filled(x.center(), 11.0, if xr.hovered() { Color32::from_black_alpha(230) } else { Color32::from_black_alpha(160) });
        crate::icons::paint(ui, x, "x", 12.0, Color32::WHITE);
        if xr.clicked() {
            action = Some(CardAction::Forget(path.clone()));
        }
    }
    let open = |path: &str| match (exists, document) {
        (_, Some(d)) => CardAction::Activate(d),
        (false, None) => CardAction::Missing(path.to_string()),
        (true, None) => CardAction::Open {
            path: path.to_string(),
            folder: match listing {
                Listing::Folder(f) => Some(f.to_string()),
                Listing::Recent => None,
            },
        },
    };
    resp.context_menu(|ui| {
        ui.set_min_width(190.0);
        if ui.add_enabled(exists || document.is_some(), egui::Button::new(tl!("Open"))).clicked() {
            action = Some(open(&path));
            ui.close();
        }
        if ui.add_enabled(exists, egui::Button::new(crate::home_ui::reveal_label())).clicked() {
            action = Some(CardAction::Reveal(path.clone()));
            ui.close();
        }
        match listing {
            Listing::Recent => {
                if !folders.is_empty() && !cfg!(target_arch = "wasm32") {
                    ui.add_enabled_ui(exists, |ui| {
                        ui.menu_button(tl!("Add to Folder"), |ui| {
                            for f in folders {
                                let (Some(id), Some(fname)) = (f["id"].as_str(), f["name"].as_str()) else { continue };
                                let here = folder.as_deref() == Some(id);
                                if ui.add_enabled(!here, egui::Button::new(fname)).clicked() {
                                    action = Some(CardAction::AddTo(path.clone(), id.to_string()));
                                    ui.close();
                                }
                            }
                        });
                    });
                }
                ui.separator();
                if ui.button(tl!("Remove from Recent")).clicked() {
                    action = Some(CardAction::Forget(path.clone()));
                    ui.close();
                }
            }
            Listing::Folder(id) => {
                let added = folders
                    .iter()
                    .find(|f| f["id"].as_str() == Some(id))
                    .and_then(|f| f["extras"].as_array())
                    .is_some_and(|e| e.iter().any(|x| x.as_str() == Some(&path)));
                if added {
                    ui.separator();
                    if ui.button(tl!("Remove from Folder")).clicked() {
                        action = Some(CardAction::RemoveFrom(id.to_string(), path.clone()));
                        ui.close();
                    }
                }
            }
        }
    });
    if action.is_none() && resp.clicked() {
        action = Some(open(&path));
    }
    action
}

/// An empty listing: an icon, a line and a hint, centred in `rect`.
pub(crate) fn empty_state(ui: &egui::Ui, rect: Rect, icon: &str, title: &str, hint: &str) {
    let t = Tokens::get(ui.ctx());
    let c = pos2(rect.center().x, rect.min.y + (rect.height() * 0.32).clamp(60.0, 160.0));
    ui.painter().circle_filled(c, 34.0, t.card);
    crate::icons::paint(ui, Rect::from_center_size(c, vec2(30.0, 30.0)), icon, 28.0, t.text_faint);
    ui.painter().text(c + vec2(0.0, 56.0), Align2::CENTER_CENTER, title, crate::theme::semibold(14.0), t.text_dim);
    let g = ui.painter().layout(hint.to_string(), FontId::proportional(12.5), t.text_faint, (rect.width() - 40.0).clamp(120.0, 420.0));
    ui.painter().galley(pos2(c.x - g.size().x / 2.0, c.y + 70.0), g, t.text_faint);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_names_end_in_an_ellipsis_on_one_line() {
        let ctx = egui::Context::default();
        let mut galleys = Vec::new();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            for name in ["FLYER-DOMINGO_PISCINA-TURCO_PROMO-ESPECIAL_9-16_v4-final-final.psd", "Short.png"] {
                galleys.push(ui.fonts_mut(|f| f.layout_job(title_job(name, FontId::proportional(13.0), Color32::WHITE, 200.0))));
            }
        });
        out.textures_delta.clear();
        let last = |g: &egui::Galley| g.rows.last().and_then(|r| r.glyphs.last()).map(|c| c.chr);
        assert_eq!(galleys[0].rows.len(), 1, "one line");
        assert_eq!(last(&galleys[0]), Some('…'));
        assert!(galleys[0].size().x <= 201.0, "fits the card: {}", galleys[0].size().x);
        assert_eq!(last(&galleys[1]), Some('g'), "a short name is left alone");
    }

    #[test]
    fn times_read_like_people_say_them() {
        crate::i18n::set_current(crate::i18n::Lang::EN);
        let midnight = 1_791_417_600; // 2026-10-08 00:00 UTC
        let now = midnight + 15 * 3600;
        assert_eq!(when_label(now, now - 20, None, true), "Opened just now");
        assert_eq!(when_label(now, now - 5 * 60, None, true), "Opened 5 min ago");
        assert_eq!(when_label(now, now - 3 * 3600, None, true), "Opened today at 12:00");
        assert_eq!(when_label(now, now - 20 * 3600, None, false), "Modified yesterday at 19:00");
        assert_eq!(when_label(now, now - 5 * 86_400, None, true), "Opened 2026-10-03");
        // The local offset moves the day boundary: 01:00 UTC is still yesterday at UTC-5.
        let est: fn(i64) -> i32 = |_| -5 * 3600;
        assert_eq!(when_label(now, midnight + 3600, Some(est), true), "Opened yesterday at 20:00");
        // A time a little in the future (clock skew) doesn't read "-2 min ago".
        assert_eq!(when_label(now, now + 100, None, true), "Opened just now");
        assert_eq!(when_label(now, now + 2 * 86_400, None, true), "Opened 2026-10-10");
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_734), (2026, 10, 8));
    }

    #[test]
    fn grids_fill_the_row_with_whole_cards() {
        let (w, cols, h) = layout(1000.0);
        assert_eq!(cols, 4);
        assert!((w * 4.0 + GAP * 3.0 - 1000.0).abs() < 0.5, "{w}");
        assert!(h > w * 0.66);
        let (w, cols, _) = layout(150.0);
        assert_eq!(cols, 1);
        assert!(w <= 150.0);
        assert_eq!(layout(400.0).0, CARD_MAX_W, "cards don't grow past their largest size");
    }
}
