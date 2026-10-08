//! Home: recents newest opened first, missing files, forgetting, thumbnails, folders (CRUD,
//! listings, added files), the folder each document belongs to and where file dialogs start.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;
use crate::Session;

/// A fresh scratch directory for one test.
fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pc-home-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn s_of(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// A session with a thumbnail folder under `dir`.
fn session(dir: &Path) -> Session {
    let mut s = Session::new();
    s.home.data_dir = Some(dir.join("data"));
    s
}

/// A small PNG at `path`.
fn png_file(path: &Path, w: u32, h: u32) {
    let img = photocraft_codecs::Image::from_u8(w, h, photocraft_codecs::ChannelLayout::Rgba, vec![200u8; (w * h * 4) as usize]).unwrap();
    std::fs::write(path, photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap()).unwrap();
}

fn paths(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|r| r["path"].as_str().unwrap().to_string()).collect()
}

#[test]
fn recents_list_newest_opened_first_with_missing_files_and_forgetting() {
    let dir = tmp("recent");
    let mut s = session(&dir);
    let (a, b, c) = (s_of(&dir.join("a.png")), s_of(&dir.join("b.psd")), s_of(&dir.join("gone.jpg")));
    png_file(Path::new(&a), 4, 4);
    std::fs::write(&b, b"not really a psd").unwrap();
    note_recent(&mut s, &a, Stamp::Opened, Some(3_000));
    note_recent(&mut s, &b, Stamp::Opened, Some(1_000));
    note_recent(&mut s, &c, Stamp::Opened, Some(2_000));
    // The list itself is newest-pushed first (File › Open Recent)...
    assert_eq!(s.prefs().file_handling.recent_files, vec![c.clone(), b.clone(), a.clone()]);
    // ...Home orders by when each was opened.
    let l = s.execute("home.recent", json!({})).unwrap();
    assert_eq!(paths(&l), vec![a.clone(), c.clone(), b.clone()]);
    assert_eq!(l[0]["name"], json!("a.png"));
    assert_eq!(l[0]["stem"], json!("a"));
    assert_eq!(l[0]["ext"], json!("png"));
    assert_eq!(l[0]["exists"], json!(true));
    assert_eq!(l[0]["openedAt"], json!(3_000));
    assert!(l[0]["modifiedAt"].as_i64().is_some());
    assert_eq!(l[1]["exists"], json!(false), "a moved file stays listed, marked missing");
    assert_eq!(l[0]["open"], json!(false));
    assert_eq!(l[0]["folder"], Value::Null);

    // A save dates only a file with no open time yet; an open re-dates it.
    note_recent(&mut s, &a, Stamp::Saved, Some(9_000));
    assert_eq!(s.prefs().home.recent_opened.get(&a), Some(&3_000));
    note_recent(&mut s, &b, Stamp::Opened, Some(9_000));
    assert_eq!(paths(&s.execute("home.recent", json!({})).unwrap())[0], b);

    // Forgetting drops the entry, its time and its thumbnail; the file stays.
    let thumb = crate::home_thumbs::thumbnail_path(&s, &a).unwrap();
    crate::home_thumbs::write_png(&thumb, b"x").unwrap();
    let l = s.execute("home.forgetRecent", json!({"path": a})).unwrap();
    assert_eq!(paths(&l), vec![b.clone(), c.clone()]);
    assert!(!thumb.exists(), "its cached thumbnail goes");
    assert!(Path::new(&a).exists(), "the file itself is never touched");
    assert!(!s.prefs().home.recent_opened.contains_key(&a));
    assert!(s.execute("home.forgetRecent", json!({"path": a})).is_err(), "not in the list any more");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_recent_list_is_capped_and_forgets_stale_times() {
    let mut s = Session::new();
    s.prefs.edit(|p| p.file_handling.recent_file_count = 5);
    for i in 0..12 {
        note_recent(&mut s, &format!("/p/{i}.psd"), Stamp::Opened, Some(i));
    }
    assert_eq!(s.prefs().file_handling.recent_files.len(), 5);
    assert_eq!(s.prefs().file_handling.recent_files[0], "/p/11.psd");
    assert_eq!(s.prefs().home.recent_opened.len(), 5, "times of dropped files go too");
    assert!(!s.prefs().home.recent_opened.contains_key("/p/0.psd"));
    clear_recent(&mut s);
    assert!(s.prefs().file_handling.recent_files.is_empty() && s.prefs().home.recent_opened.is_empty());
    // Recent File List Contains 0: nothing is remembered.
    s.prefs.edit(|p| p.file_handling.recent_file_count = 0);
    note_recent(&mut s, "/p/x.psd", Stamp::Opened, Some(1));
    assert!(s.prefs().file_handling.recent_files.is_empty() && s.prefs().home.recent_opened.is_empty());
}

#[test]
fn open_documents_are_flagged_and_thumbnails_are_written_for_them() {
    let dir = tmp("thumbs");
    let mut s = session(&dir);
    s.execute("file.new", json!({"width": 800, "height": 400, "background": "white"})).unwrap();
    let path = s_of(&dir.join("poster.psd"));
    // Without a file there is nothing to key a thumbnail on.
    assert_eq!(crate::home_thumbs::write_thumbnail(&s, 0).unwrap(), None);
    s.active_mut().unwrap().path = Some(path.clone());
    note_recent(&mut s, &path, Stamp::Saved, Some(10));
    let out = crate::home_thumbs::write_thumbnail(&s, 0).unwrap().unwrap();
    let png = std::fs::read(&out).unwrap();
    assert_eq!(&png[1..4], b"PNG");
    let img = photocraft_codecs::decode(&png).unwrap();
    assert_eq!((img.width(), img.height()), (360, 180), "360 px on the longer side");
    let l = s.execute("home.recent", json!({})).unwrap();
    assert_eq!(l[0]["open"], json!(true));
    assert_eq!(l[0]["document"], json!(0));
    assert_eq!(l[0]["thumbnail"], json!(s_of(&out)));
    assert_eq!(l[0]["exists"], json!(false), "never written to disk in this test");
    // Without a data directory (headless, web) there are no thumbnails.
    s.home.data_dir = None;
    assert_eq!(crate::home_thumbs::write_thumbnail(&s, 0).unwrap(), None);
    assert_eq!(s.execute("home.recent", json!({})).unwrap()[0]["thumbnail"], Value::Null);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn files_never_opened_get_cheap_previews() {
    let dir = tmp("preview");
    let big = dir.join("wide.png");
    png_file(&big, 1000, 500);
    let out = dir.join("t/one.png");
    assert!(crate::home_thumbs::write_file_preview(&s_of(&big), &out).unwrap());
    let img = photocraft_codecs::decode(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!((img.width(), img.height()), (360, 180));
    // Types without a cheap preview keep the placeholder.
    let raw = dir.join("x.dng");
    std::fs::write(&raw, b"raw").unwrap();
    assert!(!crate::home_thumbs::write_file_preview(&s_of(&raw), &dir.join("t/two.png")).unwrap());
    // A broken image is an error, not a panic.
    let broken = dir.join("broken.jpg");
    std::fs::write(&broken, b"\xff\xd8\xff garbage").unwrap();
    assert!(crate::home_thumbs::write_file_preview(&s_of(&broken), &dir.join("t/three.png")).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A minimal PSD head: header, empty colour mode data, and the resources given as (id, data).
fn psd_head(resources: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let mut b = b"8BPS".to_vec();
    b.extend_from_slice(&1u16.to_be_bytes());
    b.extend_from_slice(&[0; 6]);
    b.extend_from_slice(&3u16.to_be_bytes());
    b.extend_from_slice(&10u32.to_be_bytes());
    b.extend_from_slice(&10u32.to_be_bytes());
    b.extend_from_slice(&8u16.to_be_bytes());
    b.extend_from_slice(&3u16.to_be_bytes());
    b.extend_from_slice(&0u32.to_be_bytes());
    let mut res = Vec::new();
    for (id, data) in resources {
        res.extend_from_slice(b"8BIM");
        res.extend_from_slice(&id.to_be_bytes());
        res.extend_from_slice(&[0, 0]);
        res.extend_from_slice(&(data.len() as u32).to_be_bytes());
        res.extend_from_slice(data);
        if data.len() % 2 == 1 {
            res.push(0);
        }
    }
    b.extend_from_slice(&(res.len() as u32).to_be_bytes());
    b.extend_from_slice(&res);
    b
}

#[test]
fn psd_thumbnails_come_from_the_embedded_resource() {
    let mut thumb = 1u32.to_be_bytes().to_vec();
    thumb.extend_from_slice(&[0; 24]);
    thumb.extend_from_slice(b"JPEGDATA");
    let head = psd_head(&[(1005, vec![1, 2, 3]), (1036, thumb)]);
    assert_eq!(crate::home_thumbs::psd_thumbnail_jpeg(&head), Some(&b"JPEGDATA"[..]));
    assert_eq!(crate::home_thumbs::psd_thumbnail_jpeg(&psd_head(&[(1005, vec![1])])), None);
    // Truncated or hostile heads never panic.
    for n in 0..head.len() {
        let _ = crate::home_thumbs::psd_thumbnail_jpeg(&head[..n]);
    }
    let mut lying = head.clone();
    lying[26..30].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(crate::home_thumbs::psd_thumbnail_jpeg(&lying), None);
    assert_eq!(crate::home_thumbs::psd_thumbnail_jpeg(b"8BPS"), None);
}

#[test]
fn folders_are_created_linked_edited_and_removed_without_touching_files() {
    let dir = tmp("folders");
    let mut s = session(&dir);
    let root = dir.join("Projects");
    s.execute("home.setProjectsRoot", json!({"path": s_of(&root), "create": true})).unwrap();
    // New: created in the projects root.
    let f = s.execute("home.folderNew", json!({"name": "  Flyers  "})).unwrap();
    assert_eq!(f["id"], json!("flyers"));
    assert_eq!(f["name"], json!("Flyers"));
    assert_eq!(f["path"], json!(s_of(&root.join("Flyers"))));
    assert!(root.join("Flyers").is_dir(), "the directory is made");
    assert_eq!(f["exports"], f["path"], "exports default to the projects path");
    assert_eq!(f["resources"], f["path"]);
    assert_eq!(f["count"], json!(0));
    // Link: an existing directory, not created.
    let existing = dir.join("CONTENIDOS/FLYERS");
    std::fs::create_dir_all(&existing).unwrap();
    png_file(&existing.join("one.png"), 2, 2);
    std::fs::write(existing.join("notes.txt"), b"x").unwrap();
    std::fs::write(existing.join(".hidden.png"), b"x").unwrap();
    std::fs::create_dir_all(existing.join("sub")).unwrap();
    png_file(&existing.join("sub/deep.png"), 2, 2);
    let g = s.execute("home.folderNew", json!({"name": "Flyers", "path": s_of(&existing), "create": false})).unwrap();
    assert_eq!(g["id"], json!("flyers-2"), "ids stay unique");
    assert_eq!(g["count"], json!(1), "only images and documents, not hidden files or subfolders");
    assert!(s.execute("home.folderNew", json!({"name": "Again", "path": s_of(&existing), "create": false})).is_err(), "already in Home");
    assert!(s.execute("home.folderNew", json!({"name": "Nope", "path": s_of(&dir.join("missing")), "create": false})).is_err(), "linking needs a directory");
    assert!(!dir.join("missing").exists());
    // Edit: rename, custom exports, reset.
    let exports = dir.join("Exports");
    let e = s.execute("home.folderEdit", json!({"id": "flyers-2", "name": "Flyers 2026", "exports": s_of(&exports), "create": true})).unwrap();
    assert_eq!(e["name"], json!("Flyers 2026"));
    assert_eq!(e["id"], json!("flyers-2"), "a rename keeps the id");
    assert_eq!(e["exports"], json!(s_of(&exports)));
    assert_eq!(e["exportsCustom"], json!(true));
    assert_eq!(e["resources"], json!(s_of(&existing)));
    let e = s.execute("home.folderEdit", json!({"id": "flyers-2", "exports": ""})).unwrap();
    assert_eq!(e["exports"], json!(s_of(&existing)), "\"\" resets to the projects path");
    assert_eq!(e["exportsCustom"], json!(false));
    let list = s.execute("home.folders", json!({})).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(list[1]["exists"], json!(true));
    // Delete: only the list changes.
    let list = s.execute("home.folderDelete", json!({"id": "flyers"})).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert!(root.join("Flyers").is_dir(), "removing a folder from Home leaves the directory");
    assert!(existing.join("one.png").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn folder_files_list_the_directory_and_added_files_newest_first() {
    let dir = tmp("files");
    let mut s = session(&dir);
    let folder = dir.join("Flyers");
    std::fs::create_dir_all(&folder).unwrap();
    let old = folder.join("old.png");
    let new = folder.join("new.psd");
    png_file(&old, 2, 2);
    std::fs::write(&new, b"psd").unwrap();
    // Make `old` clearly older.
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options().write(true).open(&old).unwrap().set_modified(past).unwrap();
    s.execute("home.folderNew", json!({"name": "Flyers", "path": s_of(&folder), "create": false})).unwrap();
    // A file from elsewhere is added without being moved.
    let outside = dir.join("elsewhere/logo.png");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    png_file(&outside, 2, 2);
    std::fs::File::options().write(true).open(&outside).unwrap().set_modified(past - std::time::Duration::from_secs(60)).unwrap();
    s.execute("home.folderAdd", json!({"id": "flyers", "path": s_of(&outside)})).unwrap();
    assert!(outside.exists());
    assert!(s.execute("home.folderAdd", json!({"id": "flyers", "path": s_of(&dir.join("elsewhere/readme.txt"))})).is_err(), "only images and documents");
    assert!(s.execute("home.folderAdd", json!({"id": "flyers", "path": s_of(&dir.join("elsewhere/nope.png"))})).is_err(), "must exist");
    let files = s.execute("home.folderFiles", json!({"id": "flyers"})).unwrap();
    assert_eq!(paths(&files), vec![s_of(&new), s_of(&old), s_of(&outside)], "newest modified first");
    assert_eq!(files[2]["folder"], json!("flyers"), "added files belong to the folder");
    assert_eq!(s.execute("home.folders", json!({})).unwrap()[0]["count"], json!(3));
    // Recents inside a folder say so.
    note_recent(&mut s, &s_of(&old), Stamp::Opened, Some(5));
    assert_eq!(s.execute("home.recent", json!({})).unwrap()[0]["folder"], json!("flyers"));
    // Removing the added file only edits the list; a moved one shows as missing.
    s.execute("home.folderRemove", json!({"id": "flyers", "path": s_of(&outside)})).unwrap();
    assert!(outside.exists());
    assert!(s.execute("home.folderRemove", json!({"id": "flyers", "path": s_of(&outside)})).is_err());
    s.execute("home.folderAdd", json!({"id": "flyers", "path": s_of(&outside)})).unwrap();
    std::fs::remove_file(&outside).unwrap();
    let files = s.execute("home.folderFiles", json!({"id": "flyers"})).unwrap();
    assert_eq!(files[2]["exists"], json!(false), "missing files sort last and stay removable");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dialogs_start_in_the_folder_of_the_document() {
    let dir = tmp("dirs");
    let mut s = session(&dir);
    let (proj, exp, res) = (dir.join("Flyers"), dir.join("Flyers/Out"), dir.join("Photos"));
    for d in [&proj, &exp, &res] {
        std::fs::create_dir_all(d).unwrap();
    }
    s.execute("home.folderNew", json!({"name": "Flyers", "path": s_of(&proj), "create": false})).unwrap();
    s.execute("home.folderEdit", json!({"id": "flyers", "exports": s_of(&exp), "resources": s_of(&res)})).unwrap();
    let dd = |s: &mut Session, kind: &str| s.execute("home.defaultDir", json!({"kind": kind})).unwrap();
    // No document, no recents: nothing to go on.
    assert_eq!(dd(&mut s, "projects"), Value::Null);
    // The folder shown on Home wins.
    assert_eq!(s.execute("home.defaultDir", json!({"kind": "resources", "folder": "flyers"})).unwrap(), json!(s_of(&res)));
    // A new document assigned to the folder.
    s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
    assert_eq!(s.execute("home.documentFolder", json!({})).unwrap(), Value::Null);
    s.execute("home.setDocumentFolder", json!({"folder": "flyers"})).unwrap();
    assert_eq!(s.execute("home.documentFolder", json!({})).unwrap(), json!("flyers"));
    assert_eq!(dd(&mut s, "projects"), json!(s_of(&proj)));
    assert_eq!(dd(&mut s, "exports"), json!(s_of(&exp)));
    assert_eq!(dd(&mut s, "resources"), json!(s_of(&res)));
    // A document whose file is inside a folder's directory belongs to it.
    s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
    s.active_mut().unwrap().path = Some(s_of(&res.join("beach.jpg")));
    assert_eq!(s.execute("home.documentFolder", json!({})).unwrap(), json!("flyers"));
    assert_eq!(dd(&mut s, "projects"), json!(s_of(&proj)), "Save As of a resource starts in Projects");
    // Elsewhere: the document's own directory.
    let other = dir.join("Other");
    std::fs::create_dir_all(&other).unwrap();
    s.active_mut().unwrap().path = Some(s_of(&other.join("x.png")));
    assert_eq!(dd(&mut s, "exports"), json!(s_of(&other)));
    // Unassigning and closing.
    s.execute("home.setDocumentFolder", json!({"document": 0, "folder": Value::Null})).unwrap();
    assert_eq!(s.execute("home.documentFolder", json!({"document": 0})).unwrap(), Value::Null);
    // No document: the folder of the most recent file that still exists.
    s.execute("file.closeAll", json!({})).unwrap();
    note_recent(&mut s, &s_of(&dir.join("gone/x.png")), Stamp::Opened, Some(2));
    note_recent(&mut s, &s_of(&other.join("y.png")), Stamp::Opened, Some(1));
    assert_eq!(dd(&mut s, "resources"), json!(s_of(&other)));
    // A folder whose directories moved falls through.
    std::fs::remove_dir_all(&res).unwrap();
    assert_eq!(s.execute("home.defaultDir", json!({"kind": "resources", "folder": "flyers"})).unwrap(), json!(s_of(&other)));
    // Deleting the folder drops document assignments to it.
    s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
    s.execute("home.setDocumentFolder", json!({"folder": "flyers"})).unwrap();
    s.execute("home.folderDelete", json!({"id": "flyers"})).unwrap();
    assert_eq!(s.execute("home.documentFolder", json!({})).unwrap(), Value::Null);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_default_projects_root_is_documents_photocraft() {
    let mut s = Session::new();
    let root = projects_root(&s);
    if let Some(r) = &root {
        assert!(r.ends_with(&s_of(&Path::new("Documents").join("PhotoCraft"))), "{r}");
    }
    let dir = tmp("root");
    s.execute("home.setProjectsRoot", json!({"path": s_of(&dir)})).unwrap();
    assert_eq!(projects_root(&s), Some(s_of(&dir)));
    let r = s.execute("home.setProjectsRoot", json!({"path": Value::Null})).unwrap();
    assert_eq!(r["projectsRoot"], json!(root));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn folder_names_and_ids() {
    for bad in ["", "   ", "a/b", "a\\b", "..", ".", "x:y", "tab\there", &"x".repeat(NAME_MAX_CHARS + 1)] {
        assert!(clean_name("t", Some(&json!(bad))).is_err(), "{bad:?}");
    }
    assert_eq!(clean_name("t", Some(&json!(" Diseño 2026 "))).unwrap(), "Diseño 2026");
    assert_eq!(unique_id("Diseño & Flyers!", &[]), "diseño-flyers");
    assert_eq!(unique_id("!!!", &[]), "folder");
    let taken = vec![ProjectFolder { id: "a".into(), ..Default::default() }, ProjectFolder { id: "a-2".into(), ..Default::default() }];
    assert_eq!(unique_id("A", &taken), "a-3");
}

#[test]
fn bad_params_are_errors_not_panics() {
    let dir = tmp("bad");
    let mut s = session(&dir);
    s.execute("home.folderNew", json!({"name": "F", "path": s_of(&dir.join("F"))})).unwrap();
    let cases: Vec<(&str, Value)> = vec![
        ("home.forgetRecent", json!({})),
        ("home.forgetRecent", json!({"path": 5})),
        ("home.forgetRecent", json!({"path": ""})),
        ("home.folderNew", json!({})),
        ("home.folderNew", json!({"name": 3})),
        ("home.folderNew", json!({"name": "x", "path": "relative/dir"})),
        ("home.folderNew", json!({"name": "x", "path": 12})),
        ("home.folderNew", json!({"name": "x", "path": "/a\u{0}b"})),
        ("home.folderNew", json!({"name": "x", "create": "yes"})),
        ("home.folderEdit", json!({})),
        ("home.folderEdit", json!({"id": "nope", "name": "x"})),
        ("home.folderEdit", json!({"id": "f"})),
        ("home.folderEdit", json!({"id": "f", "name": ""})),
        ("home.folderEdit", json!({"id": "f", "exports": "not/absolute"})),
        ("home.folderEdit", json!({"id": "f", "resources": s_of(&dir.join("missing"))})),
        ("home.folderDelete", json!({"id": 1})),
        ("home.folderAdd", json!({"id": "f"})),
        ("home.folderRemove", json!({"id": "f", "path": "/x.png"})),
        ("home.folderFiles", json!({"id": ""})),
        ("home.setDocumentFolder", json!({"folder": "f"})),
        ("home.documentFolder", json!({"document": 99})),
        ("home.defaultDir", json!({})),
        ("home.defaultDir", json!({"kind": "music"})),
        ("home.defaultDir", json!({"kind": "projects", "folder": "nope"})),
        ("home.setProjectsRoot", json!({})),
        ("home.setProjectsRoot", json!({"path": "relative"})),
    ];
    for (id, p) in cases {
        assert!(s.execute(id, p.clone()).is_err(), "{id} {p} should fail");
    }
    s.execute("file.new", json!({"width": 4, "height": 4})).unwrap();
    for p in [json!({}), json!({"folder": 7}), json!({"folder": "nope"}), json!({"folder": "f", "document": 3}), json!({"folder": "f", "document": -1})] {
        assert!(s.execute("home.setDocumentFolder", p.clone()).is_err(), "{p}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn home_is_saved_with_the_preferences() {
    let dir = tmp("persist");
    let mut s = session(&dir);
    s.execute("home.folderNew", json!({"name": "Kept", "path": s_of(&dir.join("Kept"))})).unwrap();
    note_recent(&mut s, "/x/a.png", Stamp::Opened, Some(42));
    let text = s.prefs_to_json();
    let mut t = Session::new();
    t.load_prefs_json(&text).unwrap();
    assert_eq!(t.prefs().home, s.prefs().home);
    assert_eq!(t.prefs().home.recent_opened.get("/x/a.png"), Some(&42));
    // Older preference files without `home` still load.
    let mut u = Session::new();
    u.load_prefs_json(r#"{"general": {}}"#).unwrap();
    assert!(u.prefs().home.folders.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn psd_files_preview_their_merged_composite() {
    let dir = tmp("psd");
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 720, "height": 360, "background": "white"})).unwrap();
    let doc = s.active().unwrap().doc.clone();
    let psd = dir.join("poster.psd");
    std::fs::write(&psd, photocraft_io::export(&doc, "poster.psd", &Default::default()).unwrap().bytes).unwrap();
    let out = dir.join("t/poster.png");
    assert!(crate::home_thumbs::write_file_preview(&s_of(&psd), &out).unwrap());
    let img = photocraft_codecs::decode(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!((img.width(), img.height()), (360, 180));
    // A broken PSD is an error or no preview, never a panic.
    std::fs::write(&psd, b"8BPS\0\x01garbage").unwrap();
    let _ = crate::home_thumbs::write_file_preview(&s_of(&psd), &out);
    let _ = std::fs::remove_dir_all(&dir);
}
