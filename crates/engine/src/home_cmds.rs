//! Home, the start page: recent files, newest opened first, and project folders.
//!
//! **Recents** are File › Open Recent's list (`fileHandling.recentFiles`) plus when each file was
//! last opened (`home.recentOpened`, Unix seconds, pruned with the list). Each row says whether
//! the file still exists, when it was modified, whether a document has it open, the folder it
//! belongs to and its thumbnail: a 360 px PNG cached in `<data dir>/thumbnails/<fnv1a>.png`
//! ([`crate::home_thumbs`]), written when a document is saved or opened.
//!
//! **Folders** (`home.folders`) are real directories on disk. Each has a Projects path (where
//! Save As and Save a Copy start for its documents), an Exports path (Export As, Quick Export,
//! Save for Web) and a Resources path (Open, Place); the last two are the Projects path until
//! set. Files from elsewhere can be listed in a folder without moving them (`extras`). Nothing
//! here moves or deletes a user file: removing a folder or a recent entry only edits the lists.
//!
//! The engine persists nothing itself: the lists are preferences, saved by the frontend with the
//! rest, and the thumbnail directory comes from the frontend ([`HomeState::data_dir`]).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use photocraft_doc::DocId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always};
use crate::file_cmds::native;
use crate::{EngineError, Result, Session};

/// Longest a folder's listing gets (`home.folderFiles`), newest first.
pub const FOLDER_FILES_CAP: usize = 500;
/// Directory entries looked at per listing, so a folder of 100k files can't stall the UI.
#[cfg(not(target_arch = "wasm32"))]
const SCAN_CAP: usize = 20_000;
/// Longest folder name, in characters.
pub const NAME_MAX_CHARS: usize = 80;
/// Longest path accepted in a parameter, in bytes.
const PATH_MAX_BYTES: usize = 4096;

/// File types a folder lists: what File › Open reads (images and documents, not presets).
pub const LISTED: &[&str] = &[
    "pcraft", "psd", "psb", "psdt", "png", "jpg", "jpeg", "tif", "tiff", "webp", "gif", "bmp", "tga", "ico", "qoi", "exr", "hdr", "pbm", "pgm", "ppm", "pam",
    "pfm", "heic", "heif", "hif", "dng", "cr2", "cr3", "nef", "nrw", "arw", "pef", "orf", "rw2", "raf", "svg", "svgz",
];

/// Which of a folder's paths a file dialog starts in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirKind {
    /// Save As, Save a Copy (.pcraft, .psd).
    Projects,
    /// Export As, Quick Export, Save for Web (PNG, JPEG…).
    Exports,
    /// Open, Place.
    Resources,
}

impl DirKind {
    pub const ALL: [DirKind; 3] = [DirKind::Projects, DirKind::Exports, DirKind::Resources];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "projects" => Some(Self::Projects),
            "exports" => Some(Self::Exports),
            "resources" => Some(Self::Resources),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Projects => "projects",
            Self::Exports => "exports",
            Self::Resources => "resources",
        }
    }
}

/// Home's saved state (the `home` preference): folders, recent open times, the projects root.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HomePrefs {
    /// The folders in the sidebar, in order.
    pub folders: Vec<ProjectFolder>,
    /// When each recent file was last opened (Unix seconds), by path; pruned with the list.
    pub recent_opened: BTreeMap<String, i64>,
    /// Where new folders are created; `None` is `~/Documents/PhotoCraft`.
    pub projects_root: Option<String>,
}

/// A Home folder: a directory on disk with the paths file dialogs start in.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProjectFolder {
    /// Stable id (from the first name; a rename keeps it).
    pub id: String,
    pub name: String,
    /// The folder's directory, where its projects are saved.
    pub path: String,
    /// Where exports go; `None` is [`Self::path`].
    pub exports: Option<String>,
    /// Where Open and Place start; `None` is [`Self::path`].
    pub resources: Option<String>,
    /// Files from elsewhere shown in the folder (never moved).
    pub extras: Vec<String>,
}

impl ProjectFolder {
    /// The directory of `kind` (exports and resources fall back to the projects path).
    pub fn dir(&self, kind: DirKind) -> &str {
        let custom = match kind {
            DirKind::Projects => None,
            DirKind::Exports => self.exports.as_deref(),
            DirKind::Resources => self.resources.as_deref(),
        };
        custom.filter(|d| !d.is_empty()).unwrap_or(&self.path)
    }

    /// Is `path` inside one of the folder's directories, or listed in it?
    pub fn holds(&self, path: &str) -> bool {
        self.owns(path) || [DirKind::Exports, DirKind::Resources].iter().any(|k| inside(path, self.dir(*k)))
    }

    /// Is `path` one of the folder's own files: in its directory, or added to it?
    pub fn owns(&self, path: &str) -> bool {
        self.extras.iter().any(|e| e == path) || inside(path, &self.path)
    }
}

/// Is `path` inside the directory `dir` (at any depth)?
fn inside(path: &str, dir: &str) -> bool {
    let p = Path::new(path);
    !dir.is_empty() && p != Path::new(dir) && p.starts_with(dir)
}

/// Home's runtime state on the session (not saved).
#[derive(Clone, Debug, Default)]
pub struct HomeState {
    /// Where thumbnails are cached (`<data dir>/thumbnails`): the desktop app's settings folder.
    /// `None` (headless sessions, the web) keeps no thumbnails.
    pub data_dir: Option<PathBuf>,
    /// The folder each open document belongs to (New Document in a folder, a file opened from
    /// one), by document id. Save As then starts in that folder's Projects path.
    pub doc_folders: HashMap<DocId, String>,
}

/// Now, in Unix seconds (`None` on the web, which has no clock in std).
pub fn unix_now() -> Option<i64> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().and_then(|d| i64::try_from(d.as_secs()).ok())
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

/// How [`note_recent`] dates a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stamp {
    /// Just opened: its open time becomes `now`.
    Opened,
    /// Just saved: dated only if it has no open time yet (a new document's first save).
    Saved,
}

/// Put `path` at the top of the recent list (File › Open Recent and Home), capped at Preferences ›
/// File Handling › Recent File List Contains, and date it. Open times of files that left the
/// list are dropped.
pub fn note_recent(s: &mut Session, path: &str, stamp: Stamp, now: Option<i64>) {
    if path.is_empty() {
        return;
    }
    let cap = s.prefs().file_handling.recent_file_count.min(100) as usize;
    s.prefs.edit(|p| {
        let files = &mut p.file_handling.recent_files;
        files.retain(|x| x != path);
        files.insert(0, path.to_string());
        files.truncate(cap);
        if let Some(now) = now {
            match stamp {
                Stamp::Opened => {
                    p.home.recent_opened.insert(path.to_string(), now);
                }
                Stamp::Saved => {
                    p.home.recent_opened.entry(path.to_string()).or_insert(now);
                }
            }
        }
        let files = &p.file_handling.recent_files;
        p.home.recent_opened.retain(|k, _| files.contains(k));
    });
}

/// File › Open Recent › Clear Recent File List: the list and its open times.
pub fn clear_recent(s: &mut Session) {
    s.prefs.edit(|p| {
        p.file_handling.recent_files.clear();
        p.home.recent_opened.clear();
    });
}

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| path.to_string())
}

fn extension(path: &str) -> String {
    Path::new(path).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// Can a folder list this file (an image or document File › Open reads)?
pub fn is_listed(path: &str) -> bool {
    LISTED.contains(&extension(path).as_str())
}

/// What the file system says about a path.
#[derive(Clone, Copy, Debug, Default)]
struct FileInfo {
    exists: bool,
    modified: Option<i64>,
    size: Option<u64>,
}

#[cfg(not(target_arch = "wasm32"))]
fn mtime(m: &std::fs::Metadata) -> Option<i64> {
    let d = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(d.as_secs()).ok()
}

#[cfg(not(target_arch = "wasm32"))]
fn file_info(path: &str) -> FileInfo {
    match std::fs::metadata(path) {
        // A .pcraft bundle may be a directory.
        Ok(m) => FileInfo { exists: true, modified: mtime(&m), size: m.is_file().then_some(m.len()) },
        Err(_) => FileInfo::default(),
    }
}
#[cfg(target_arch = "wasm32")]
fn file_info(_path: &str) -> FileInfo {
    FileInfo::default()
}

/// Is `path` a directory on disk (never on the web)?
#[cfg(not(target_arch = "wasm32"))]
pub fn is_dir(path: &str) -> bool {
    !path.is_empty() && Path::new(path).is_dir()
}
#[cfg(target_arch = "wasm32")]
pub fn is_dir(_path: &str) -> bool {
    false
}

/// The folder `path` belongs to: the first whose directory holds it or that lists it, else the
/// first whose Exports or Resources path holds it.
pub fn folder_of<'a>(home: &'a HomePrefs, path: &str) -> Option<&'a ProjectFolder> {
    home.folders.iter().find(|f| f.owns(path)).or_else(|| home.folders.iter().find(|f| f.holds(path)))
}

fn find<'a>(s: &'a Session, id: &str) -> Option<&'a ProjectFolder> {
    s.prefs().home.folders.iter().find(|f| f.id == id)
}

/// One file as Home shows it (recents and folder listings share the shape).
fn file_row(s: &Session, path: &str, opened: Option<i64>, info: FileInfo) -> Value {
    let (thumbnail, stale) = crate::home_thumbs::cached(s, path, info.modified);
    let document = s.documents().iter().position(|d| d.path.as_deref() == Some(path));
    let name = file_name(path);
    let stem = Path::new(&name).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| name.clone());
    json!({
        "path": path,
        "name": name,
        "stem": stem,
        "ext": extension(path),
        "exists": info.exists,
        "openedAt": opened,
        "modifiedAt": info.modified,
        "size": info.size,
        "thumbnail": thumbnail.map(|t| t.to_string_lossy().into_owned()),
        "thumbnailStale": stale,
        "open": document.is_some(),
        "document": document,
        "folder": folder_of(&s.prefs().home, path).map(|f| f.id.clone()),
    })
}

/// `home.recent`: the recent files, most recently opened first (files without an open time sort
/// by their modification time).
pub fn recent(s: &Session) -> Value {
    let p = s.prefs();
    let mut rows: Vec<(i64, usize, Value)> = p
        .file_handling
        .recent_files
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let opened = p.home.recent_opened.get(path).copied();
            let info = file_info(path);
            let key = opened.or(info.modified).unwrap_or(i64::MIN);
            (key, i, file_row(s, path, opened, info))
        })
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    Value::Array(rows.into_iter().map(|r| r.2).collect())
}

fn str_arg<'a>(cmd: &str, p: &'a Value, key: &str) -> Result<&'a str> {
    match p.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s),
        Some(Value::String(_)) => Err(bad(cmd, format!("`{key}` is empty"))),
        Some(_) => Err(bad(cmd, format!("`{key}` must be a string"))),
        None => Err(bad(cmd, format!("missing `{key}`"))),
    }
}

/// `home.forgetRecent {path}`: drop a file from the recent list and its cached thumbnail. The
/// file itself stays where it is.
fn forget(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "home.forgetRecent";
    let path = str_arg(cmd, p, "path")?.to_string();
    if !s.prefs().file_handling.recent_files.contains(&path) {
        return Err(bad(cmd, format!("{path} is not in the recent list")));
    }
    s.prefs.edit(|pr| {
        pr.file_handling.recent_files.retain(|x| *x != path);
        pr.home.recent_opened.remove(&path);
    });
    crate::home_thumbs::remove(s, &path);
    Ok(recent(s))
}

/// A folder name: trimmed, 1–80 characters, usable as a directory name.
fn clean_name(cmd: &str, v: Option<&Value>) -> Result<String> {
    let raw = match v {
        Some(Value::String(s)) => s,
        Some(_) => return Err(bad(cmd, "`name` must be a string")),
        None => return Err(bad(cmd, "missing `name`")),
    };
    let name = raw.trim();
    if name.is_empty() {
        return Err(bad(cmd, "the folder needs a name"));
    }
    if name.chars().count() > NAME_MAX_CHARS {
        return Err(bad(cmd, format!("the name is longer than {NAME_MAX_CHARS} characters")));
    }
    if name == "." || name == ".." || name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')) {
        return Err(bad(cmd, "a folder name can't be . or .. or contain / \\ : * ? \" < > |"));
    }
    Ok(name.to_string())
}

/// An absolute path parameter, without trailing separators.
fn clean_path(cmd: &str, key: &str, v: &Value) -> Result<String> {
    let raw = v.as_str().ok_or_else(|| bad(cmd, format!("`{key}` must be a path")))?.trim();
    if raw.is_empty() {
        return Err(bad(cmd, format!("`{key}` is empty")));
    }
    if raw.len() > PATH_MAX_BYTES || raw.contains('\0') {
        return Err(bad(cmd, format!("`{key}` is not a usable path")));
    }
    if !Path::new(raw).is_absolute() {
        return Err(bad(cmd, format!("`{key}` must be an absolute path (got {raw})")));
    }
    let trimmed = raw.trim_end_matches(['/', '\\']);
    Ok(if trimmed.is_empty() || trimmed.ends_with(':') { raw.to_string() } else { trimmed.to_string() })
}

/// The id a folder named `name` gets: lower-case letters and digits joined by dashes, made
/// unique among `taken`.
fn unique_id(name: &str, taken: &[ProjectFolder]) -> String {
    let mut base = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            base.push(c);
        } else if !base.ends_with('-') && !base.is_empty() {
            base.push('-');
        }
    }
    let base: String = base.trim_end_matches('-').chars().take(40).collect();
    let base = if base.is_empty() { "folder".to_string() } else { base };
    let free = |id: &str| !taken.iter().any(|f| f.id == id);
    if free(&base) {
        return base;
    }
    (2..).map(|n| format!("{base}-{n}")).find(|id| free(id)).unwrap_or(base)
}

/// The user's home directory (for the default projects root).
#[cfg(not(target_arch = "wasm32"))]
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).filter(|h| !h.is_empty()).map(PathBuf::from)
}
#[cfg(target_arch = "wasm32")]
fn home_dir() -> Option<PathBuf> {
    None
}

/// Where New Folder creates folders: the `home.projectsRoot` preference, else
/// `~/Documents/PhotoCraft`.
pub fn projects_root(s: &Session) -> Option<String> {
    if let Some(r) = s.prefs().home.projects_root.as_deref().filter(|r| !r.is_empty()) {
        return Some(r.to_string());
    }
    Some(home_dir()?.join("Documents").join("PhotoCraft").to_string_lossy().into_owned())
}

/// Make sure `dir` is a directory: create it when `create`, else it must exist.
#[cfg(not(target_arch = "wasm32"))]
fn ensure_dir(cmd: &str, dir: &str, create: bool) -> Result<()> {
    let p = Path::new(dir);
    if p.is_dir() {
        return Ok(());
    }
    if p.exists() {
        return Err(bad(cmd, format!("{dir} is a file, not a folder")));
    }
    if !create {
        return Err(bad(cmd, format!("{dir} doesn't exist")));
    }
    std::fs::create_dir_all(p).map_err(|e| EngineError::Other(format!("couldn't create {dir}: {e}")))
}
#[cfg(target_arch = "wasm32")]
fn ensure_dir(cmd: &str, dir: &str, _create: bool) -> Result<()> {
    Err(bad(cmd, format!("{dir}: no file system on the web")))
}

fn bool_arg(cmd: &str, p: &Value, key: &str, default: bool) -> Result<bool> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(bad(cmd, format!("`{key}` must be true or false"))),
    }
}

fn same_dir(a: &str, b: &str) -> bool {
    Path::new(a) == Path::new(b)
}

/// The supported files directly inside `dir` (no hidden files, no recursion).
#[cfg(not(target_arch = "wasm32"))]
fn scan_dir(dir: &str) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.take(SCAN_CAP)
        .filter_map(|e| e.ok())
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path().to_string_lossy().into_owned())
        .filter(|p| is_listed(p))
        .collect()
}
#[cfg(target_arch = "wasm32")]
fn scan_dir(_dir: &str) -> Vec<String> {
    Vec::new()
}

/// The folder's files: its directory's, then the files added from elsewhere (once each).
fn folder_paths(f: &ProjectFolder) -> Vec<String> {
    let mut paths = scan_dir(&f.path);
    for e in &f.extras {
        if !paths.iter().any(|p| Path::new(p) == Path::new(e)) {
            paths.push(e.clone());
        }
    }
    paths
}

fn folder_row(f: &ProjectFolder) -> Value {
    json!({
        "id": f.id,
        "name": f.name,
        "path": f.path,
        "exports": f.dir(DirKind::Exports),
        "resources": f.dir(DirKind::Resources),
        "exportsCustom": f.exports.is_some(),
        "resourcesCustom": f.resources.is_some(),
        "extras": f.extras,
        "exists": is_dir(&f.path),
        "count": folder_paths(f).len(),
    })
}

/// `home.folders`: the folders, in sidebar order.
pub fn folders(s: &Session) -> Value {
    Value::Array(s.prefs().home.folders.iter().map(folder_row).collect())
}

fn folder_index(cmd: &str, s: &Session, p: &Value) -> Result<usize> {
    let id = str_arg(cmd, p, "id")?;
    s.prefs().home.folders.iter().position(|f| f.id == id).ok_or_else(|| bad(cmd, format!("no folder `{id}`")))
}

/// `home.folderNew {name, path?, create?}`.
fn folder_new(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "home.folderNew";
    let name = clean_name(cmd, p.get("name"))?;
    let create = bool_arg(cmd, p, "create", true)?;
    let path = match p.get("path") {
        None | Some(Value::Null) => {
            let root = projects_root(s).ok_or_else(|| bad(cmd, "no projects folder to create it in: pass `path`"))?;
            Path::new(&root).join(&name).to_string_lossy().into_owned()
        }
        Some(v) => clean_path(cmd, "path", v)?,
    };
    if let Some(f) = s.prefs().home.folders.iter().find(|f| same_dir(&f.path, &path)) {
        return Err(bad(cmd, format!("{path} is already in Home as “{}”", f.name)));
    }
    ensure_dir(cmd, &path, create)?;
    let folder = ProjectFolder { id: unique_id(&name, &s.prefs().home.folders), name, path, ..Default::default() };
    let row = folder_row(&folder);
    s.prefs.edit(|pr| pr.home.folders.push(folder));
    Ok(row)
}

/// A directory parameter for `home.folderEdit`: `""` or `null` resets it (`None`).
fn dir_arg(cmd: &str, key: &str, v: &Value, create: bool) -> Result<Option<String>> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) if s.trim().is_empty() => Ok(None),
        v => {
            let d = clean_path(cmd, key, v)?;
            ensure_dir(cmd, &d, create)?;
            Ok(Some(d))
        }
    }
}

/// `home.folderEdit {id, name?, path?, exports?, resources?, create?}`.
fn folder_edit(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "home.folderEdit";
    let i = folder_index(cmd, s, p)?;
    if ["name", "path", "exports", "resources"].iter().all(|k| p.get(*k).is_none()) {
        return Err(bad(cmd, "give `name`, `path`, `exports` or `resources`"));
    }
    let create = bool_arg(cmd, p, "create", false)?;
    let mut f = s.prefs().home.folders.get(i).cloned().ok_or_else(|| bad(cmd, "no such folder"))?;
    if p.get("name").is_some() {
        f.name = clean_name(cmd, p.get("name"))?;
    }
    if let Some(v) = p.get("path") {
        let path = clean_path(cmd, "path", v)?;
        if let Some(other) = s.prefs().home.folders.iter().enumerate().find(|(j, o)| *j != i && same_dir(&o.path, &path)).map(|(_, o)| o.name.clone()) {
            return Err(bad(cmd, format!("{path} is already in Home as “{other}”")));
        }
        ensure_dir(cmd, &path, create)?;
        f.path = path;
    }
    for key in ["exports", "resources"] {
        let Some(v) = p.get(key) else { continue };
        let dir = dir_arg(cmd, key, v, create)?.filter(|d| !same_dir(d, &f.path));
        match key {
            "exports" => f.exports = dir,
            _ => f.resources = dir,
        }
    }
    let row = folder_row(&f);
    s.prefs.edit(|pr| {
        if let Some(slot) = pr.home.folders.get_mut(i) {
            *slot = f;
        }
    });
    Ok(row)
}

/// `home.folderDelete {id}`: remove the folder from Home; its files stay where they are.
fn folder_delete(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "home.folderDelete";
    let i = folder_index(cmd, s, p)?;
    let id = s.prefs().home.folders.get(i).map(|f| f.id.clone()).unwrap_or_default();
    s.prefs.edit(|pr| {
        if i < pr.home.folders.len() {
            pr.home.folders.remove(i);
        }
    });
    s.home.doc_folders.retain(|_, f| *f != id);
    Ok(folders(s))
}

/// `home.folderAdd {id, path}`: list a file from elsewhere in the folder (it isn't moved).
fn folder_add(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "home.folderAdd";
    let i = folder_index(cmd, s, p)?;
    let path = clean_path(cmd, "path", p.get("path").unwrap_or(&Value::Null))?;
    if !is_listed(&path) {
        return Err(bad(cmd, format!("{} isn't an image or document PhotoCraft opens", file_name(&path))));
    }
    if !file_info(&path).exists {
        return Err(bad(cmd, format!("{path} doesn't exist")));
    }
    s.prefs.edit(|pr| {
        if let Some(f) = pr.home.folders.get_mut(i)
            && !f.extras.contains(&path)
        {
            f.extras.push(path);
        }
    });
    s.prefs().home.folders.get(i).map(folder_row).ok_or_else(|| bad(cmd, "no such folder"))
}

/// `home.folderRemove {id, path}`: take an added file out of the folder (the file stays).
fn folder_remove(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "home.folderRemove";
    let i = folder_index(cmd, s, p)?;
    let path = str_arg(cmd, p, "path")?.to_string();
    let listed = s.prefs().home.folders.get(i).is_some_and(|f| f.extras.contains(&path));
    if !listed {
        return Err(bad(cmd, format!("{path} was not added to this folder")));
    }
    s.prefs.edit(|pr| {
        if let Some(f) = pr.home.folders.get_mut(i) {
            f.extras.retain(|e| *e != path);
        }
    });
    s.prefs().home.folders.get(i).map(folder_row).ok_or_else(|| bad(cmd, "no such folder"))
}

/// `home.folderFiles {id}`: the images and documents in the folder's directory (not its
/// subfolders) and the files added to it, newest modified first, at most 500.
fn folder_files(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "home.folderFiles";
    let i = folder_index(cmd, s, p)?;
    let f = s.prefs().home.folders.get(i).cloned().ok_or_else(|| bad(cmd, "no such folder"))?;
    let opened = &s.prefs().home.recent_opened;
    let mut rows: Vec<(bool, i64, String, FileInfo)> = folder_paths(&f)
        .into_iter()
        .map(|path| {
            let info = file_info(&path);
            (info.exists, info.modified.unwrap_or(i64::MIN), path, info)
        })
        .collect();
    // Existing files first, newest first; then by name so equal times keep a stable order.
    rows.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    rows.truncate(FOLDER_FILES_CAP);
    Ok(Value::Array(rows.into_iter().map(|(_, _, path, info)| file_row(s, &path, opened.get(&path).copied(), info)).collect()))
}

fn document_index(cmd: &str, s: &Session, p: &Value) -> Result<usize> {
    match p.get("document") {
        None | Some(Value::Null) => s.active_index().ok_or(EngineError::NoDocument),
        Some(v) => {
            let i = v.as_u64().ok_or_else(|| bad(cmd, "`document` must be a document index"))?;
            usize::try_from(i).ok().filter(|i| *i < s.documents().len()).ok_or(EngineError::NoDocument)
        }
    }
}

/// The folder an open document belongs to: the one it was assigned (New Document in a folder, a
/// file opened from one), else the one holding its file.
pub fn document_folder(s: &Session, index: usize) -> Option<&ProjectFolder> {
    let d = s.documents().get(index)?;
    if let Some(f) = s.home.doc_folders.get(&d.doc.id).and_then(|id| find(s, id)) {
        return Some(f);
    }
    folder_of(&s.prefs().home, d.path.as_deref()?)
}

/// `home.setDocumentFolder {folder, document?}`: the folder a document belongs to (`null`:
/// none), for this session.
fn set_document_folder(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "home.setDocumentFolder";
    let i = document_index(cmd, s, p)?;
    let id = s.documents().get(i).map(|d| d.doc.id).ok_or(EngineError::NoDocument)?;
    match p.get("folder") {
        Some(Value::Null) => {
            s.home.doc_folders.remove(&id);
        }
        Some(Value::String(f)) => {
            if find(s, f).is_none() {
                return Err(bad(cmd, format!("no folder `{f}`")));
            }
            s.home.doc_folders.insert(id, f.clone());
        }
        Some(_) => return Err(bad(cmd, "`folder` must be a folder id or null")),
        None => return Err(bad(cmd, "missing `folder` (a folder id, or null for none)")),
    }
    Ok(json!({"document": i, "folder": document_folder(s, i).map(|f| f.id.clone())}))
}

/// `home.documentFolder {document?}`: the folder a document belongs to, or null.
fn get_document_folder(s: &mut Session, p: &Value) -> Result<Value> {
    let i = document_index("home.documentFolder", s, p)?;
    Ok(json!(document_folder(s, i).map(|f| f.id.clone())))
}

/// Where a file dialog of `kind` starts: the folder `folder` (the one shown on Home) when given;
/// else the active document's folder; else the active document's own directory; else the
/// directory of the most recent file that still has one. Only directories that exist count.
pub fn default_dir(s: &Session, kind: DirKind, folder: Option<&str>) -> Option<String> {
    let usable = |d: &str| is_dir(d).then(|| d.to_string());
    if let Some(d) = folder.and_then(|id| find(s, id)).and_then(|f| usable(f.dir(kind))) {
        return Some(d);
    }
    if let Some(i) = s.active_index() {
        if let Some(d) = document_folder(s, i).and_then(|f| usable(f.dir(kind))) {
            return Some(d);
        }
        let own = s.documents().get(i).and_then(|d| d.path.as_deref()).and_then(|p| Path::new(p).parent()).map(|d| d.to_string_lossy().into_owned());
        if let Some(d) = own.as_deref().and_then(usable) {
            return Some(d);
        }
    }
    s.prefs().file_handling.recent_files.iter().filter_map(|p| Path::new(p).parent()).map(|d| d.to_string_lossy().into_owned()).find(|d| is_dir(d))
}

/// `home.defaultDir {kind, folder?}`.
fn default_dir_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "home.defaultDir";
    let kind = str_arg(cmd, p, "kind")?;
    let kind = DirKind::parse(kind).ok_or_else(|| bad(cmd, format!("`kind` must be projects, exports or resources (got {kind})")))?;
    let folder = match p.get("folder") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) => {
            if find(s, id).is_none() {
                return Err(bad(cmd, format!("no folder `{id}`")));
            }
            Some(id.as_str())
        }
        Some(_) => return Err(bad(cmd, "`folder` must be a folder id")),
    };
    Ok(json!(default_dir(s, kind, folder)))
}

/// `home.setProjectsRoot {path}`: where New Folder creates folders (`null` or `""`: the default,
/// `~/Documents/PhotoCraft`).
fn set_projects_root(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "home.setProjectsRoot";
    let root = match p.get("path") {
        None => return Err(bad(cmd, "missing `path` (null for the default)")),
        Some(Value::Null) => None,
        Some(Value::String(v)) if v.trim().is_empty() => None,
        Some(v) => {
            let d = clean_path(cmd, "path", v)?;
            ensure_dir(cmd, &d, bool_arg(cmd, p, "create", false)?)?;
            Some(d)
        }
    };
    s.prefs.edit(|pr| pr.home.projects_root = root);
    Ok(json!({"projectsRoot": projects_root(s)}))
}

macro_rules! spec {
    ($id:literal, $label:literal, $params:literal, $en:expr, $run:expr) => {
        CommandSpec { id: $id, label: $label, menu: &[], shortcut: None, params: $params, enabled: $en, run: $run, journal: false }
    };
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "home.recent",
            "Recent Files",
            r#"{} → [{path, name, stem, ext, exists, openedAt, modifiedAt, size, thumbnail, thumbnailStale, open, document, folder}], most recently opened first"#,
            always,
            |s, _| Ok(recent(s))
        ),
        spec!("home.forgetRecent", "Remove from Recent", r#"{"path":str} (the recent entry; the file itself stays)"#, always, forget),
        spec!(
            "home.folders",
            "Home Folders",
            r#"{} → [{id, name, path, exports, resources, exportsCustom, resourcesCustom, extras, exists, count}]"#,
            always,
            |s, _| Ok(folders(s))
        ),
        spec!(
            "home.folderNew",
            "New Folder",
            r#"{"name":str,"path":str?,"create":bool?} (path: absolute; default <projects root>/<name>; create (default true) makes the directory, false links an existing one)"#,
            native,
            folder_new
        ),
        spec!(
            "home.folderEdit",
            "Edit Folder",
            r#"{"id":str,"name":str?,"path":str?,"exports":str?,"resources":str?,"create":bool?} (absolute directories; "" or null resets exports/resources to the folder's path)"#,
            always,
            folder_edit
        ),
        spec!("home.folderDelete", "Remove Folder from Home", r#"{"id":str} (the directory and its files stay)"#, always, folder_delete),
        spec!(
            "home.folderAdd",
            "Add to Folder",
            r#"{"id":str,"path":str} (an image or document from elsewhere, listed without moving it)"#,
            native,
            folder_add
        ),
        spec!("home.folderRemove", "Remove from Folder", r#"{"id":str,"path":str} (a file added with home.folderAdd)"#, always, folder_remove),
        spec!(
            "home.folderFiles",
            "Folder Files",
            r#"{"id":str} → the folder's images and documents (not subfolders) and added files, newest first, at most 500"#,
            native,
            folder_files
        ),
        spec!(
            "home.setDocumentFolder",
            "Set Document Folder",
            r#"{"folder":str|null,"document":index?} (this session; Save As then starts in the folder's Projects path)"#,
            has_doc,
            set_document_folder
        ),
        spec!("home.documentFolder", "Document Folder", r#"{"document":index?} → folder id or null"#, has_doc, get_document_folder),
        spec!(
            "home.defaultDir",
            "Default Dialog Folder",
            r#"{"kind":"projects|exports|resources","folder":str?} → the directory a file dialog starts in, or null"#,
            always,
            default_dir_cmd
        ),
        spec!(
            "home.setProjectsRoot",
            "Set Projects Root",
            r#"{"path":str|null,"create":bool?} (where New Folder creates folders; null: ~/Documents/PhotoCraft)"#,
            native,
            set_projects_root
        ),
    ]
}

#[cfg(test)]
#[path = "home_cmds_tests.rs"]
mod tests;
