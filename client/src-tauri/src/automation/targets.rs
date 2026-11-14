//! Host-discovered application paths. Model names never become executable paths.
use serde::Serialize;
#[cfg(any(target_os = "windows", test))]
#[path = "windows_catalog.rs"]
mod windows_catalog;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Instant,
};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledApplication {
    pub id: String,
    pub name: String,
    pub path: String,
    pub aliases: Vec<String>,
    pub identifier: Option<String>,
}
impl InstalledApplication {
    pub fn matches(&self, query: &str) -> bool {
        let query = query.to_lowercase();
        self.id.to_lowercase() == query
            || self
                .aliases
                .iter()
                .chain(std::iter::once(&self.name))
                .chain(self.identifier.iter())
                .any(|n| n.to_lowercase().contains(&query))
    }
    pub fn approval_details(&self) -> serde_json::Value {
        serde_json::json!({"appPath":self.path,"application":self.name,"applicationIdentifier":self.identifier,"applicationAliases":self.aliases})
    }
}
static CATALOG: OnceLock<Mutex<(Instant, Vec<InstalledApplication>)>> = OnceLock::new();

pub fn list() -> Vec<InstalledApplication> {
    let cache = CATALOG.get_or_init(|| Mutex::new((Instant::now(), discover())));
    let mut cache = cache.lock().unwrap();
    if cache.0.elapsed().as_secs() > 30 {
        *cache = (Instant::now(), discover());
    }
    cache.1.clone()
}
pub fn resolve(id_or_name: &str) -> Result<InstalledApplication, String> {
    let found: Vec<_> = list()
        .into_iter()
        .filter(|a| {
            a.id == id_or_name
                || a.name.eq_ignore_ascii_case(id_or_name)
                || a.aliases.iter().any(|n| n.eq_ignore_ascii_case(id_or_name))
                || a.identifier.as_deref() == Some(id_or_name)
        })
        .collect();
    match found.as_slice() {
        [a] => Ok(a.clone()),
        [] => Err("application_not_found: choose an installed application".into()),
        _ => Err("ambiguous_application: choose the exact application ID".into()),
    }
}
fn discover() -> Vec<InstalledApplication> {
    let mut roots = vec![];
    #[cfg(target_os = "macos")]
    {
        roots.extend([
            (PathBuf::from("/Applications"), 4),
            (PathBuf::from("/System/Applications"), 4),
        ]);
        if let Some(home) = std::env::var_os("HOME") {
            roots.push((PathBuf::from(home).join("Applications"), 4));
        }
    }
    #[cfg(target_os = "windows")]
    {
        roots.extend(windows_catalog::roots(|name| std::env::var_os(name)));
    }
    let mut result = vec![];
    let mut seen = HashSet::new();
    #[cfg(target_os = "windows")]
    for path in windows_catalog::registered_paths() {
        add_application(&path, &mut seen, &mut result);
    }
    for (root, depth) in roots {
        let mut visited = 0;
        scan(
            &root,
            depth,
            &mut visited,
            &mut seen,
            &mut result,
            depth == 0,
        );
    }
    result.sort_by_key(|a| a.name.to_lowercase());
    result
}
fn scan(
    dir: &Path,
    remaining_depth: usize,
    visited: &mut usize,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<InstalledApplication>,
    _system_gui_only: bool,
) {
    if *visited > 12000 || out.len() >= 512 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        *visited += 1;
        if *visited > 12000 || out.len() >= 512 {
            break;
        }
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        let app =
            cfg!(target_os = "macos") && kind.is_dir() && extension.eq_ignore_ascii_case("app")
                || cfg!(target_os = "windows")
                    && kind.is_file()
                    && extension.eq_ignore_ascii_case("exe");
        if app {
            #[cfg(target_os = "windows")]
            if _system_gui_only && !windows_catalog::is_gui(&path) {
                continue;
            }
            add_application(&path, seen, out);
        } else if kind.is_dir() && remaining_depth > 0 {
            scan(
                &path,
                remaining_depth - 1,
                visited,
                seen,
                out,
                _system_gui_only,
            );
        }
    }
}

fn add_application(path: &Path, seen: &mut HashSet<PathBuf>, out: &mut Vec<InstalledApplication>) {
    if out.len() >= 512 {
        return;
    }
    let Ok(info) = std::fs::symlink_metadata(path) else {
        return;
    };
    if info.file_type().is_symlink()
        || !(info.is_file()
            && path
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("exe"))
            || info.is_dir()
                && path
                    .extension()
                    .is_some_and(|s| s.eq_ignore_ascii_case("app")))
    {
        return;
    }
    let Ok(path) = path.canonicalize() else {
        return;
    };
    if !seen.insert(path.clone()) {
        return;
    }
    let fallback = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let (name, aliases, identifier) = metadata(&path, fallback);
    out.push(InstalledApplication {
        id: format!("installed:{}", path.to_string_lossy()),
        name,
        path: path.to_string_lossy().into_owned(),
        aliases,
        identifier,
    });
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn metadata(_: &Path, name: String) -> (String, Vec<String>, Option<String>) {
    (name.clone(), vec![name], None)
}
#[cfg(target_os = "windows")]
fn metadata(path: &Path, _: String) -> (String, Vec<String>, Option<String>) {
    let (name, aliases) = super::accessibility::windows_application_names(&path.to_string_lossy());
    (name, aliases, Some(path.to_string_lossy().into_owned()))
}

/// Resolve a native executable/bundle identity to host-owned localized aliases.
/// Never accept aliases supplied by a tool caller as an expansion of scope.
pub fn scope_mentions_application(scope: &str, target: &str) -> bool {
    list()
        .iter()
        .any(|app| app_scope_matches(app, scope, target))
}
fn app_scope_matches(app: &InstalledApplication, scope: &str, target: &str) -> bool {
    let details = app.approval_details();
    super::policy::target(&details) == target
        && super::policy::target_names(&details)
            .iter()
            .any(|name| mentioned(scope, name))
}
#[cfg(target_os = "macos")]
fn metadata(path: &Path, fallback: String) -> (String, Vec<String>, Option<String>) {
    use objc2::{class, msg_send, rc::Retained, runtime::AnyObject};
    use objc2_foundation::NSString;
    objc2::rc::autoreleasepool(|_| unsafe {
        let path_string = NSString::from_str(&path.to_string_lossy());
        let bundle: Option<Retained<AnyObject>> =
            msg_send![class!(NSBundle),bundleWithPath:&*path_string];
        let mut aliases = vec![fallback.clone()];
        let Some(bundle) = bundle else {
            return (fallback, aliases, None);
        };
        let value = |key: &str| -> Option<String> {
            let key = NSString::from_str(key);
            let value: Option<Retained<AnyObject>> =
                msg_send![&*bundle,objectForInfoDictionaryKey:&*key];
            value
                .and_then(|v| v.downcast::<NSString>().ok())
                .map(|v| v.to_string())
                .filter(|v| !v.trim().is_empty() && v.len() <= 512)
        };
        let display = value("CFBundleDisplayName").or_else(|| value("CFBundleName"));
        for key in ["CFBundleDisplayName", "CFBundleName", "CFBundleExecutable"] {
            if let Some(value) = value(key) {
                aliases.push(value);
            }
        }
        let identifier = value("CFBundleIdentifier");
        if let Some(id) = &identifier {
            let apps = objc2_app_kit::NSRunningApplication::runningApplicationsWithBundleIdentifier(
                &NSString::from_str(id),
            );
            for app in apps {
                if app
                    .bundleURL()
                    .and_then(|u| u.path())
                    .and_then(|p| std::fs::canonicalize(p.to_string()).ok())
                    .as_deref()
                    == Some(path)
                {
                    if let Some(name) = app.localizedName() {
                        aliases.push(name.to_string());
                    }
                }
            }
        }
        aliases.retain(|v| !v.trim().is_empty() && v.len() <= 512);
        let mut seen = HashSet::new();
        aliases.retain(|v| seen.insert(v.to_lowercase()));
        (display.unwrap_or(fallback), aliases, identifier)
    })
}

/// Derive task target checks from host observations, never a caller-provided `approved` flag.
pub fn mentioned(scope: &str, name: &str) -> bool {
    let normalized = scope.to_lowercase();
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name)
        .to_lowercase();
    !stem.is_empty()
        && normalized.match_indices(&stem).any(|(start, _)| {
            if !stem.is_ascii() {
                return true;
            }
            let end = start + stem.len();
            let boundary =
                |ch: Option<char>| ch.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
            boundary(normalized[..start].chars().next_back())
                && boundary(normalized[end..].chars().next())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_deduplicates_physical_executables_and_rejects_missing_paths() {
        let path = std::env::temp_dir().join(format!("echo-catalog-{}.exe", uuid::Uuid::now_v7()));
        std::fs::write(&path, b"discovery does not execute files").unwrap();
        let mut seen = HashSet::new();
        let mut apps = vec![];
        add_application(&path, &mut seen, &mut apps);
        add_application(&path, &mut seen, &mut apps);
        add_application(&path.with_extension("missing.exe"), &mut seen, &mut apps);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].path, path.canonicalize().unwrap().to_string_lossy());
        std::fs::remove_file(path).unwrap();
    }
    #[cfg(target_os = "windows")]
    #[test]
    fn cold_system_application_has_a_discovered_launch_identity() {
        let system = windows_catalog::roots(|key| std::env::var_os(key));
        let notepad = system
            .iter()
            .map(|(dir, _)| dir.join("notepad.exe"))
            .find(|p| p.is_file());
        if let Some(path) = notepad {
            let canonical = path.canonicalize().unwrap();
            let app = list()
                .into_iter()
                .find(|app| Path::new(&app.path) == canonical)
                .expect("system Notepad must be discoverable without a running process");
            assert!(app
                .aliases
                .iter()
                .any(|name| name.eq_ignore_ascii_case("notepad.exe")));
            assert_eq!(resolve(&app.id).unwrap().path, app.path);
        }
    }
    #[test]
    fn application_scope_does_not_authorize_neighbors() {
        assert!(mentioned("在 TextEdit 中输入", "TextEdit.app"));
        assert!(!mentioned("在 TextEdit 中输入", "Terminal"));
        assert!(!mentioned("在 TextEditor 中输入", "TextEdit"));
        assert!(!mentioned("在 Data 中输入", "a"));
    }
    #[test]
    fn observed_localized_aliases_match_queries_and_scope_without_authorizing_neighbors() {
        let app = InstalledApplication {
            id: "installed:/Applications/WeChat.app".into(),
            name: "WeChat".into(),
            path: "/Applications/WeChat.app".into(),
            aliases: vec!["WeChat".into(), "微信".into()],
            identifier: Some("com.tencent.xinWeChat".into()),
        };
        assert!(app.matches("微信"));
        assert!(app.matches("wechat"));
        assert!(!app.matches("Terminal"));
        let details = app.approval_details();
        assert!(super::super::policy::target_names(&details)
            .iter()
            .any(|name| mentioned("请打开微信发送消息", name)));
        assert!(!super::super::policy::target_names(&details)
            .iter()
            .any(|name| mentioned("在 TextEdit 输入", name)));
        assert_eq!(
            super::super::policy::target(&details),
            "com.tencent.xinWeChat"
        );
        assert!(app_scope_matches(
            &app,
            "请打开微信发送消息",
            "com.tencent.xinWeChat"
        ));
        assert!(!app_scope_matches(
            &app,
            "请打开微信发送消息",
            "other.application"
        ));
        assert!(!app_scope_matches(
            &app,
            "在 TextEdit 编辑文稿",
            "com.tencent.xinWeChat"
        ));
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_metadata_uses_actual_bundle_values() {
        let path = Path::new("/System/Applications/TextEdit.app");
        if !path.is_dir() {
            return;
        }
        let (_, aliases, id) = metadata(path, "TextEdit".into());
        assert_eq!(id.as_deref(), Some("com.apple.TextEdit"));
        assert!(aliases.iter().any(|s| s == "TextEdit"));
    }
}
