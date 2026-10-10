//! Home in the real app: recent cards newest opened first, folders in the sidebar, a card opens
//! its file and leaves Home, a folder's view lists its files; file dialogs start in the folder's
//! paths; thumbnails are written on save and open; `ui.set {home}` / `ui.inspect`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Document, Size};
use photocraft_engine::home_cmds::{Stamp, note_recent};
use serde_json::{Value, json};

use super::*;
use crate::file_dialog::FileDialogRequest;
use crate::{Services, menus};

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pc-home-ui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn s_of(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// What the fake file dialogs were asked, in order.
type Asked = Rc<RefCell<Vec<FileDialogRequest>>>;

/// Services whose importer makes an 8×8 document of any file, whose file dialogs record what
/// they were asked and cancel, and whose writer writes for real.
fn services(asked: &Asked) -> Services {
    let log = asked.clone();
    Services {
        import: Some(Box::new(|name: &str, _bytes: &[u8], _depth: usize| {
            Ok((Document::new(name, Size::new(8, 8), ColorMode::Rgb, SampleType::U8), Vec::new()))
        })),
        export: Some(Box::new(|_doc: &Document, _path: &str, _s: &crate::ExportSettings| Ok((b"out".to_vec(), Vec::new())))),
        write: Some(Box::new(|path: &str, bytes: &[u8]| std::fs::write(path, bytes).map_err(|e| e.to_string()))),
        file_dialog: Some(Box::new(move |request, _parent, reply| {
            log.borrow_mut().push(request);
            reply.send(None);
        })),
        ..Default::default()
    }
}

/// The last file dialog asked for, once the app has shown it (the end of a frame).
fn last_asked(app: &mut PhotocraftApp, asked: &Asked) -> FileDialogRequest {
    app.poll_file_dialog(&egui::Context::default(), None);
    assert!(!app.file_dialog_open(), "the fake dialog answered");
    asked.borrow().last().cloned().expect("a file dialog was asked for")
}

/// An Open dialog for one file (`multiple: false`: Place) or several (File › Open) starting in `dir`.
fn open_in(multiple: bool, dir: &Path) -> FileDialogRequest {
    FileDialogRequest::Open { multiple, initial_dir: Some(s_of(dir)), extensions: None }
}

/// The suggestion of a Save dialog.
fn suggested(request: FileDialogRequest) -> String {
    match request {
        FileDialogRequest::Save { suggested } => suggested,
        other => panic!("expected a save dialog, got {other:?}"),
    }
}

fn png(path: &Path) {
    let img = photocraft_codecs::Image::from_u8(4, 3, photocraft_codecs::ChannelLayout::Rgba, vec![180u8; 48]).unwrap();
    std::fs::write(path, photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap()).unwrap();
}

/// The test files: `Alpha.png` and `Bravo.png` (recents), a recent that was moved away, and a
/// `Flyers` folder holding `Poster.png` (only listed there).
struct Files {
    dir: PathBuf,
    alpha: String,
    bravo: String,
    gone: String,
    flyers: PathBuf,
}

fn files(name: &str) -> Files {
    let dir = tmp(name);
    let flyers = dir.join("Flyers");
    std::fs::create_dir_all(&flyers).unwrap();
    let (alpha, bravo, gone, poster) = (dir.join("Alpha.png"), dir.join("Bravo.png"), dir.join("Gone.png"), flyers.join("Poster.png"));
    for p in [&alpha, &bravo, &poster] {
        png(p);
    }
    Files { alpha: s_of(&alpha), bravo: s_of(&bravo), gone: s_of(&gone), flyers, dir }
}

/// A session with the test recents (Bravo opened last) and the Flyers folder.
fn session(f: &Files) -> photocraft_engine::Session {
    let mut s = photocraft_engine::Session::new();
    s.home.data_dir = Some(f.dir.join("data"));
    note_recent(&mut s, &f.alpha, Stamp::Opened, Some(1_000));
    note_recent(&mut s, &f.gone, Stamp::Opened, Some(1_500));
    note_recent(&mut s, &f.bravo, Stamp::Opened, Some(2_000));
    s.execute("home.folderNew", json!({"name": "Flyers", "path": s_of(&f.flyers), "create": false})).unwrap();
    s
}

fn harness(f: &Files, asked: &Asked) -> Harness<'static, PhotocraftApp> {
    let session = session(f);
    let services = services(asked);
    let mut h = Harness::builder().with_size(vec2(1500.0, 950.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, crate::theme::ThemeKind::ProMedium);
        PhotocraftApp::new(session, services)
    });
    h.run_steps(6);
    h
}

fn card_names(app: &PhotocraftApp) -> Vec<String> {
    app.home.cards.iter().map(|c| c.name.clone()).collect()
}

#[test]
fn home_shows_recents_newest_first_and_folders_and_cards_open_their_files() {
    let f = files("cards");
    let asked: Asked = Rc::default();
    let mut h = harness(&f, &asked);
    assert!(shown(h.state()), "Home is up with no documents");
    assert_eq!(card_names(h.state()), ["Bravo.png", "Gone.png", "Alpha.png"], "newest opened first");
    assert!(h.query_by_label("Flyers").is_some(), "the folder is in the sidebar");
    let cards = &h.state().home.cards;
    assert!(cards[0].rect.min.x < cards[1].rect.min.x && cards[1].rect.min.y == cards[0].rect.min.y, "one row, left to right");

    // A missing file stays on Home; clicking it explains instead of failing to open.
    h.get_by_label("Gone.png").click();
    h.run_steps(3);
    assert!(h.state().session.documents().is_empty());
    assert!(h.state().ui.status_error && h.state().ui.status.contains("Gone.png"), "{}", h.state().ui.status);

    // A card opens its file and leaves Home.
    h.get_by_label("Alpha.png").click();
    h.run_steps(4);
    let app = h.state();
    assert_eq!(app.session.documents().len(), 1);
    assert_eq!(app.session.active().and_then(|d| d.path.clone()), Some(f.alpha.clone()));
    assert!(!shown(app), "opening a file leaves Home");
    let opened = app.session.prefs().home.recent_opened.get(&f.alpha).copied().unwrap();
    assert!(opened > 2_000, "opening dates the file again: {opened}");

    // Back on Home it is first, marked open; its card switches to its tab instead of reopening.
    set_view(h.state_mut(), &json!("recent")).unwrap();
    h.run_steps(4);
    assert_eq!(card_names(h.state())[0], "Alpha.png");
    let recent = h.state_mut().session.execute("home.recent", json!({})).unwrap();
    assert_eq!(recent[0]["open"], json!(true));
    h.get_by_label("Alpha.png").click();
    h.run_steps(3);
    assert_eq!(h.state().session.documents().len(), 1, "an open file isn't opened twice");
    assert!(!shown(h.state()));

    // A folder's view lists its files; a file opened there belongs to the folder.
    set_view(h.state_mut(), &json!("folder:flyers")).unwrap();
    h.run_steps(4);
    assert_eq!(card_names(h.state()), ["Poster.png"]);
    h.get_by_label("Poster.png").click();
    h.run_steps(4);
    let app = h.state_mut();
    assert_eq!(app.session.documents().len(), 2);
    assert_eq!(app.session.execute("home.documentFolder", json!({})).unwrap(), json!("flyers"));
    assert_eq!(app.session.execute("home.recent", json!({})).unwrap()[0]["folder"], json!("flyers"));
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn the_sidebar_switches_views_and_removing_a_card_keeps_the_file() {
    let f = files("sidebar");
    let asked: Asked = Rc::default();
    let mut h = harness(&f, &asked);
    h.get_by_label("Flyers").click();
    h.run_steps(4);
    assert_eq!(h.state().ui.chrome.home_folder.as_deref(), Some("flyers"));
    assert_eq!(card_names(h.state()), ["Poster.png"]);
    // A path chip picks another directory, starting at the current one.
    h.get_by_label("Exports").click();
    h.run_steps(2);
    assert_eq!(asked.borrow().last().cloned(), Some(FileDialogRequest::Folder { initial_dir: Some(s_of(&f.flyers)) }));
    h.get_by_label("Home").click();
    h.run_steps(4);
    assert_eq!(h.state().ui.chrome.home_folder, None);
    // Hover a card, then its × removes it from Recent; the file stays.
    h.get_by_label("Bravo.png").hover();
    h.run_steps(2);
    h.get_by_label("Remove Bravo.png from Recent").click();
    h.run_steps(4);
    assert_eq!(card_names(h.state()), ["Gone.png", "Alpha.png"]);
    assert!(Path::new(&f.bravo).exists());
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn file_dialogs_start_in_the_folder_paths() {
    let f = files("dialogs");
    let asked: Asked = Rc::default();
    let (exports, resources) = (f.dir.join("Out"), f.dir.join("Photos"));
    let mut app = PhotocraftApp::new(session(&f), services(&asked));
    let ctx = egui::Context::default();
    app.run("home.folderEdit", json!({"id": "flyers", "exports": s_of(&exports), "resources": s_of(&resources), "create": true})).unwrap();
    // Open… on the folder's view starts in its Resources.
    set_view(&mut app, &json!("folder:flyers")).unwrap();
    app.open_dialog_file().unwrap();
    assert_eq!(last_asked(&mut app, &asked), open_in(true, &resources));
    // New Document Here… makes a document of the folder.
    apply(&mut app, Action::NewDocument(Some("flyers".into())));
    let d = app.ui.dialogs.last().unwrap().id;
    crate::dialogs::confirm(&mut app, d).unwrap();
    assert_eq!(app.session.execute("home.documentFolder", json!({})).unwrap(), json!("flyers"));
    assert!(!shown(&app), "the new document leaves Home");
    let dir_of = |s: String| Path::new(&s).parent().map(s_of).unwrap_or_default();
    // Save As and Save a Copy start in Projects…
    menus::invoke(&mut app, &ctx, "file.saveAs", json!({})).unwrap();
    assert_eq!(suggested(last_asked(&mut app, &asked)), s_of(&f.flyers.join("Untitled-1.psd")));
    menus::invoke(&mut app, &ctx, "file.saveACopy", json!({})).unwrap();
    assert_eq!(dir_of(suggested(last_asked(&mut app, &asked))), s_of(&f.flyers));
    // …Export As, Quick Export and Save for Web in Exports…
    let dialog = menus::invoke(&mut app, &ctx, "file.export.exportAs", json!({})).unwrap()["dialog"].as_u64().unwrap();
    crate::dialogs::confirm(&mut app, dialog).unwrap();
    assert_eq!(dir_of(suggested(last_asked(&mut app, &asked))), s_of(&exports));
    menus::invoke(&mut app, &ctx, "file.export.quickExportAsPng", json!({})).unwrap();
    assert_eq!(dir_of(suggested(last_asked(&mut app, &asked))), s_of(&exports));
    let dialog = menus::invoke(&mut app, &ctx, "file.export.saveForWebLegacy", json!({})).unwrap()["dialog"].as_u64().unwrap();
    crate::dialogs::confirm(&mut app, dialog).unwrap();
    assert_eq!(dir_of(suggested(last_asked(&mut app, &asked))), s_of(&exports));
    // …and Open and Place in Resources.
    app.open_dialog_file().unwrap();
    assert_eq!(last_asked(&mut app, &asked), open_in(true, &resources));
    menus::invoke(&mut app, &ctx, "file.placeEmbedded", json!({})).unwrap();
    assert_eq!(last_asked(&mut app, &asked), open_in(false, &resources));
    // A document outside every folder: where its file is.
    let other = f.dir.join("Elsewhere");
    std::fs::create_dir_all(&other).unwrap();
    app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
    app.session.active_mut().unwrap().path = Some(s_of(&other.join("card.psd")));
    menus::invoke(&mut app, &ctx, "file.saveAs", json!({})).unwrap();
    assert_eq!(suggested(last_asked(&mut app, &asked)), s_of(&other.join("card.psd")));
    app.open_dialog_file().unwrap();
    assert_eq!(last_asked(&mut app, &asked), open_in(true, &other));
    let _ = std::fs::remove_dir_all(&f.dir);
}

/// The folder dialogs of Home's own dialogs answer on a later frame and fill in the dialog they
/// were asked from.
#[test]
fn folder_dialogs_fill_in_the_home_dialog_that_asked() {
    let f = files("folder-picks");
    let chosen = f.dir.join("Chosen");
    std::fs::create_dir_all(&chosen).unwrap();
    let asked: Asked = Rc::default();
    let log = asked.clone();
    let answer = s_of(&chosen);
    let mut services = services(&asked);
    services.file_dialog = Some(Box::new(move |request, _parent, reply| {
        log.borrow_mut().push(request);
        reply.send(Some(crate::FileDialogAnswer::Paths(vec![answer.clone()])));
    }));
    let mut app = PhotocraftApp::new(session(&f), services);
    // A path chip re-points the folder's Exports path (starting from the listing on screen).
    refresh(&mut app, 0.0);
    apply(&mut app, Action::PickDir("flyers".into(), DirKind::Exports));
    assert_eq!(last_asked(&mut app, &asked), FileDialogRequest::Folder { initial_dir: Some(s_of(&f.flyers)) });
    assert_eq!(app.session.prefs().home.folders.iter().find(|x| x.id == "flyers").and_then(|x| x.exports.clone()), Some(s_of(&chosen)));
    // New Folder › Use an existing folder: Choose… fills in the path, the mode and the name.
    crate::home_dialogs::open_new_folder(&mut app);
    let d = app.ui.dialogs.last().unwrap().clone();
    crate::home_dialogs::pick(&mut app, &d.fields, &s_of(&f.dir), crate::home_dialogs::link_chosen);
    assert_eq!(last_asked(&mut app, &asked), FileDialogRequest::Folder { initial_dir: Some(s_of(&f.dir)) });
    let fields = &app.ui.dialogs.last().unwrap().fields;
    assert_eq!((fields["path"].clone(), fields["mode"].clone(), fields["name"].clone()), (json!(s_of(&chosen)), json!("link"), json!("Chosen")));
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn saving_and_opening_write_thumbnails_and_record_recents() {
    let f = files("thumbs");
    let asked: Asked = Rc::default();
    let mut app = PhotocraftApp::new(session(&f), services(&asked));
    // A new document's first save makes it a recent file with a thumbnail.
    app.run("file.new", json!({"width": 640, "height": 320, "background": "white"})).unwrap();
    let saved = s_of(&f.dir.join("New.psd"));
    app.save_as(Some(saved.clone())).unwrap();
    assert!(app.home.wait_thumbnails(std::time::Duration::from_secs(20)));
    let thumb = photocraft_engine::home_thumbs::thumbnail_path(&app.session, &saved).unwrap();
    let img = photocraft_codecs::decode(&std::fs::read(&thumb).unwrap()).unwrap();
    assert_eq!((img.width(), img.height()), (360, 180));
    assert_eq!(app.ui.recent_files.first(), Some(&saved));
    assert!(app.session.prefs().home.recent_opened.contains_key(&saved), "a first save dates it");
    // Opening a file without a thumbnail renders one.
    let alpha_thumb = photocraft_engine::home_thumbs::thumbnail_path(&app.session, &f.alpha).unwrap();
    assert!(!alpha_thumb.exists());
    app.open_path(&f.alpha).unwrap();
    assert!(app.home.wait_thumbnails(std::time::Duration::from_secs(20)));
    assert!(alpha_thumb.exists(), "opened without one: rendered");
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn ui_set_home_switches_views_and_ui_inspect_reports_them() {
    let f = files("control");
    let asked: Asked = Rc::default();
    let mut app = PhotocraftApp::new(session(&f), services(&asked));
    let ctx = egui::Context::default();
    let call = |app: &mut PhotocraftApp, method: &str, params: Value| {
        let (req, _rx) = crate::ControlRequest::new(method, params);
        match crate::control::handle(app, &ctx, &req) {
            crate::control::Outcome::Done(v) => v,
            _ => panic!("{method}: expected an immediate reply"),
        }
    };
    app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
    assert_eq!(call(&mut app, "ui.inspect", json!({}))["result"]["home"]["shown"], json!(false));
    let r = call(&mut app, "ui.set", json!({"home": "folder:flyers"}));
    assert_eq!(r["ok"], json!(true), "{r}");
    let home = call(&mut app, "ui.inspect", json!({}))["result"]["home"].clone();
    assert_eq!((home["shown"].clone(), home["view"].clone(), home["folder"].clone()), (json!(true), json!("folder:flyers"), json!("flyers")));
    for bad in [json!("folder:nope"), json!(5), json!("files")] {
        let r = call(&mut app, "ui.set", json!({"home": bad.clone(), "tool": "move"}));
        assert_eq!(r["ok"], json!(false), "{bad}: {r}");
    }
    assert_eq!(app.ui.tool, crate::state::Tool::Brush, "a bad home value changes nothing");
    assert_eq!(call(&mut app, "ui.set", json!({"home": "recent"}))["ok"], json!(true));
    assert_eq!(app.ui.chrome.home_folder, None);
    assert_eq!(call(&mut app, "ui.set", json!({"home": false}))["ok"], json!(true));
    assert!(!shown(&app));
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn home_dialogs_create_rename_and_repoint_folders() {
    let f = files("folder-dialogs");
    let asked: Asked = Rc::default();
    let mut app = PhotocraftApp::new(session(&f), services(&asked));
    // New Folder, created in a chosen projects folder.
    crate::home_dialogs::open_new_folder(&mut app);
    let d = app.ui.dialogs.last().unwrap().clone();
    assert!(!crate::home_dialogs::ok_enabled(&d.fields), "a folder needs a name");
    let root = f.dir.join("Projects");
    {
        let fields = &mut app.ui.dialog_mut(d.id).unwrap().fields;
        fields.insert("name".into(), json!("Domingos"));
        fields.insert("root".into(), json!(s_of(&root)));
    }
    assert!(crate::home_dialogs::ok_enabled(&app.ui.dialogs.last().unwrap().fields));
    crate::dialogs::confirm(&mut app, d.id).unwrap();
    assert!(root.join("Domingos").is_dir());
    assert_eq!(app.ui.chrome.home_folder.as_deref(), Some("domingos"), "Home shows the new folder");
    assert_eq!(photocraft_engine::home_cmds::projects_root(&app.session), Some(s_of(&root)), "the chosen place is remembered");
    // Link an existing folder.
    crate::home_dialogs::open_new_folder(&mut app);
    let d = app.ui.dialogs.last().unwrap().id;
    let linked = f.dir.join("CONTENIDOS");
    std::fs::create_dir_all(&linked).unwrap();
    {
        let fields = &mut app.ui.dialog_mut(d).unwrap().fields;
        fields.insert("name".into(), json!("Contenidos"));
        fields.insert("mode".into(), json!("link"));
        fields.insert("path".into(), json!(s_of(&linked)));
    }
    crate::dialogs::confirm(&mut app, d).unwrap();
    assert_eq!(app.session.prefs().home.folders.last().map(|f| f.path.clone()), Some(s_of(&linked)));
    // Rename.
    crate::home_dialogs::open_rename(&mut app, "domingos");
    let d = app.ui.dialogs.last().unwrap().id;
    app.ui.dialog_mut(d).unwrap().fields.insert("name".into(), json!("Domingos 2026"));
    crate::dialogs::confirm(&mut app, d).unwrap();
    assert!(app.session.prefs().home.folders.iter().any(|f| f.id == "domingos" && f.name == "Domingos 2026"));
    // Folder Paths: a custom Exports path, then back to the Projects path.
    crate::home_dialogs::open_paths(&mut app, "domingos");
    let d = app.ui.dialogs.last().unwrap().id;
    app.ui.dialog_mut(d).unwrap().fields.insert("exports".into(), json!(s_of(&linked)));
    crate::dialogs::confirm(&mut app, d).unwrap();
    let folder = |app: &PhotocraftApp| app.session.prefs().home.folders.iter().find(|f| f.id == "domingos").cloned().unwrap();
    assert_eq!(folder(&app).exports, Some(s_of(&linked)));
    crate::home_dialogs::open_paths(&mut app, "domingos");
    let d = app.ui.dialogs.last().unwrap().id;
    app.ui.dialog_mut(d).unwrap().fields.insert("exports".into(), json!(""));
    crate::dialogs::confirm(&mut app, d).unwrap();
    assert_eq!(folder(&app).exports, None);
    let _ = std::fs::remove_dir_all(&f.dir);
}
