//! Collapses a debouncer batch into one event per kind.

use std::collections::HashMap;
use std::path::PathBuf;

use notify::event::EventKind;
use notify_debouncer_full::DebouncedEvent;

use crate::event::FsEventKind;

/// Raw OS-level event from the debouncer, before `event.rs` gives it meaning.
#[derive(Debug, Clone)]
pub(crate) struct RawFsEvent {
    pub paths: Vec<PathBuf>,
    pub kind: FsEventKind,
}

/// `None` for kinds we never surface, so the public enum has no unobservable
/// variants.
pub(crate) fn map_event_kind(kind: &EventKind) -> Option<FsEventKind> {
    use notify::event::ModifyKind;
    match kind {
        EventKind::Create(_) => Some(FsEventKind::Created),
        EventKind::Modify(ModifyKind::Name(_)) => Some(FsEventKind::Renamed),
        EventKind::Modify(_) => Some(FsEventKind::Modified),
        EventKind::Remove(_) => Some(FsEventKind::Removed),
        EventKind::Access(_) | EventKind::Any | EventKind::Other => None,
    }
}

pub(crate) fn merge_events(events: impl IntoIterator<Item = DebouncedEvent>) -> Vec<RawFsEvent> {
    let mut by_path: HashMap<PathBuf, FsEventKind> = HashMap::new();
    // Rename events preserve original path ordering from the OS ([old, new]),
    // so they bypass the HashMap merge and are emitted directly.
    let mut rename_events: Vec<RawFsEvent> = Vec::new();

    for event in events.into_iter() {
        let Some(kind) = map_event_kind(&event.kind) else {
            continue;
        };

        if kind == FsEventKind::Renamed {
            rename_events.push(RawFsEvent {
                paths: event.event.paths,
                kind: FsEventKind::Renamed,
            });
            continue;
        }

        let paths = event.event.paths;
        for path in paths {
            by_path
                .entry(path)
                .and_modify(|existing| match (*existing, kind) {
                    (_, FsEventKind::Removed) => *existing = FsEventKind::Removed,
                    (FsEventKind::Created, FsEventKind::Modified) => {}
                    (FsEventKind::Modified, FsEventKind::Created) => {
                        *existing = FsEventKind::Created
                    }
                    _ => {}
                })
                .or_insert(kind);
        }
    }

    let mut result: HashMap<FsEventKind, Vec<PathBuf>> = HashMap::new();
    for (path, kind) in by_path {
        result.entry(kind).or_default().push(path);
    }

    let mut merged: Vec<RawFsEvent> = result
        .into_iter()
        .map(|(kind, paths)| RawFsEvent { paths, kind })
        .collect();
    merged.extend(rename_events);
    merged
}
