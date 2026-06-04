//! Per-workspace filesystem watcher. Emits debounced
//! `WorkspaceFilesChanged` events to the global `EventHub`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::event::{ModifyKind, RenameMode};
use notify::{EventKind as NotifyEventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::api::schema::{
    EventData, EventEnvelope, EventKind as SchemaEventKind, FilesChangedKind,
};
use crate::api::EventHub;

const QUIET_WINDOW: Duration = Duration::from_millis(50);
const MAX_BATCH_WINDOW: Duration = Duration::from_millis(500);
const IDLE_POLL: Duration = Duration::from_millis(200);

pub struct WorkspaceWatcher {
    shutdown: Arc<AtomicBool>,
    // Detached on Drop; the run loop polls `shutdown` and exits within ~IDLE_POLL.
    _handle: Option<std::thread::JoinHandle<()>>,
}

impl WorkspaceWatcher {
    pub fn spawn(workspace_id: String, cwd: PathBuf, hub: EventHub) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();
        let handle = std::thread::Builder::new()
            .name(format!("herdr-fs-watch-{workspace_id}"))
            .spawn(move || run(workspace_id, cwd, hub, shutdown_clone));
        let handle = match handle {
            Ok(h) => Some(h),
            Err(err) => {
                tracing::warn!("workspace.files_changed: failed to spawn watcher thread: {err}");
                None
            }
        };
        Self {
            shutdown,
            _handle: handle,
        }
    }
}

impl Drop for WorkspaceWatcher {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

fn run(workspace_id: String, cwd: PathBuf, hub: EventHub, shutdown: Arc<AtomicBool>) {
    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = match RecommendedWatcher::new(tx, notify::Config::default()) {
        Ok(watcher) => watcher,
        Err(err) => {
            tracing::warn!(
                "workspace.files_changed: failed to construct watcher for {}: {err}",
                cwd.display()
            );
            return;
        }
    };

    if let Err(err) = watcher.watch(&cwd, RecursiveMode::Recursive) {
        tracing::warn!(
            "workspace.files_changed: failed to watch {}: {err}",
            cwd.display()
        );
        return;
    }

    // FSEvents on macOS may report events on the watched root itself (and
    // canonicalizes through /private/...). Use the canonical form for the
    // root-suppression check below so we don't emit noise events for the
    // workspace dir's own metadata changes.
    let cwd_canonical = std::fs::canonicalize(&cwd).unwrap_or_else(|_| cwd.clone());

    let mut paths: HashSet<PathBuf> = HashSet::new();
    let mut kind = FilesChangedKind::default();
    let mut batch_start: Option<Instant> = None;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        let timeout = match batch_start {
            Some(start) => {
                let remaining = MAX_BATCH_WINDOW.saturating_sub(start.elapsed());
                remaining.min(QUIET_WINDOW)
            }
            None => IDLE_POLL,
        };

        let recv = rx.recv_timeout(timeout);
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        match recv {
            Ok(Ok(event)) => {
                let mut accepted_any = false;
                for path in &event.paths {
                    if path == &cwd_canonical || path == &cwd {
                        continue;
                    }
                    if noise_filter(path) {
                        paths.insert(path.clone());
                        accepted_any = true;
                    }
                }
                if accepted_any {
                    apply_kind(&mut kind, &event.kind);
                    if batch_start.is_none() {
                        batch_start = Some(Instant::now());
                    }
                }
            }
            Ok(Err(err)) => {
                tracing::debug!("workspace.files_changed: watcher reported error: {err}");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if batch_start.is_some() && !paths.is_empty() {
                    emit(&hub, &workspace_id, &mut paths, &mut kind);
                    batch_start = None;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }

        if let Some(start) = batch_start {
            if start.elapsed() >= MAX_BATCH_WINDOW && !paths.is_empty() {
                emit(&hub, &workspace_id, &mut paths, &mut kind);
                batch_start = None;
            }
        }
    }
}

fn emit(
    hub: &EventHub,
    workspace_id: &str,
    paths: &mut HashSet<PathBuf>,
    kind: &mut FilesChangedKind,
) {
    let mut path_strings: Vec<String> = paths
        .drain()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    path_strings.sort();
    let envelope = EventEnvelope {
        event: SchemaEventKind::WorkspaceFilesChanged,
        data: EventData::WorkspaceFilesChanged {
            workspace_id: workspace_id.to_string(),
            paths: path_strings,
            kind: std::mem::take(kind),
        },
    };
    hub.push(envelope);
}

fn apply_kind(acc: &mut FilesChangedKind, event_kind: &NotifyEventKind) {
    match event_kind {
        NotifyEventKind::Create(_) => acc.created = true,
        NotifyEventKind::Remove(_) => acc.deleted = true,
        NotifyEventKind::Modify(ModifyKind::Name(mode)) => match mode {
            RenameMode::From => acc.deleted = true,
            RenameMode::To => acc.created = true,
            RenameMode::Both | RenameMode::Any | RenameMode::Other => acc.renamed = true,
        },
        NotifyEventKind::Modify(_) => acc.modified = true,
        NotifyEventKind::Access(_) | NotifyEventKind::Other | NotifyEventKind::Any => {}
    }
}

fn noise_filter(path: &Path) -> bool {
    const NOISE: &[&str] = &[
        "target",
        "node_modules",
        "dist",
        "build",
        ".next",
        ".venv",
        "__pycache__",
    ];
    for comp in path.components() {
        if let std::path::Component::Normal(seg) = comp {
            if let Some(s) = seg.to_str() {
                if NOISE.contains(&s) {
                    return false;
                }
            }
        }
    }

    if let Some(idx) = path.components().position(
        |c| matches!(c, std::path::Component::Normal(s) if s == std::ffi::OsStr::new(".git")),
    ) {
        let rest: PathBuf = path.components().skip(idx + 1).collect();
        let rest_str = rest.to_string_lossy();
        return rest_str == "HEAD" || rest_str == "index" || rest_str.starts_with("refs/heads/");
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn wait_for_event<F>(hub: &EventHub, matcher: F, timeout: Duration) -> Option<EventEnvelope>
    where
        F: Fn(&EventEnvelope) -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            for (_, env) in hub.events_after(0) {
                if matcher(&env) {
                    return Some(env);
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn file_event_count(hub: &EventHub, workspace_id: &str) -> usize {
        hub.events_after(0)
            .into_iter()
            .filter(|(_, env)| {
                matches!(
                    &env.data,
                    EventData::WorkspaceFilesChanged { workspace_id: wid, .. }
                        if wid == workspace_id
                )
            })
            .count()
    }

    fn collect_paths(env: &EventEnvelope) -> Vec<String> {
        match &env.data {
            EventData::WorkspaceFilesChanged { paths, .. } => paths.clone(),
            _ => Vec::new(),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn emits_event_on_file_write() {
        let dir = tempfile::tempdir().unwrap();
        let hub = EventHub::default();
        let watcher = WorkspaceWatcher::spawn("ws-1".into(), dir.path().into(), hub.clone());
        tokio::time::sleep(Duration::from_millis(150)).await;
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();

        let event = tokio::task::spawn_blocking({
            let hub = hub.clone();
            move || {
                wait_for_event(
                    &hub,
                    |env| {
                        matches!(
                            &env.data,
                            EventData::WorkspaceFilesChanged { workspace_id, .. }
                                if workspace_id == "ws-1"
                        )
                    },
                    Duration::from_millis(1500),
                )
            }
        })
        .await
        .unwrap();

        assert!(event.is_some(), "expected workspace.files_changed event");
        drop(watcher);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn debounces_burst() {
        let dir = tempfile::tempdir().unwrap();
        let hub = EventHub::default();
        let watcher = WorkspaceWatcher::spawn("ws-burst".into(), dir.path().into(), hub.clone());
        tokio::time::sleep(Duration::from_millis(150)).await;

        for i in 0..100 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
        }

        tokio::time::sleep(Duration::from_millis(800)).await;
        let count = file_event_count(&hub, "ws-burst");
        assert!(
            (1..=2).contains(&count),
            "expected 1 (or at most 2 if MAX_BATCH_WINDOW triggers) events, got {count}",
        );
        drop(watcher);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ignores_noise_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        let hub = EventHub::default();
        let watcher = WorkspaceWatcher::spawn("ws-noise".into(), dir.path().into(), hub.clone());
        tokio::time::sleep(Duration::from_millis(150)).await;

        std::fs::write(dir.path().join("target/foo"), "x").unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(file_event_count(&hub, "ws-noise"), 0);
        drop(watcher);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn emits_on_git_head_change() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let hub = EventHub::default();
        let watcher = WorkspaceWatcher::spawn("ws-git".into(), dir.path().into(), hub.clone());
        tokio::time::sleep(Duration::from_millis(150)).await;

        std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        let event = tokio::task::spawn_blocking({
            let hub = hub.clone();
            move || {
                wait_for_event(
                    &hub,
                    |env| {
                        matches!(
                            &env.data,
                            EventData::WorkspaceFilesChanged { workspace_id, .. }
                                if workspace_id == "ws-git"
                        )
                    },
                    Duration::from_millis(1500),
                )
            }
        })
        .await
        .unwrap();

        let env = event.expect(".git/HEAD write should emit event");
        let paths = collect_paths(&env);
        assert!(
            paths.iter().any(|p| p.ends_with(".git/HEAD")),
            "expected .git/HEAD path in event paths, got {paths:?}",
        );
        drop(watcher);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handles_missing_cwd_gracefully() {
        let missing = PathBuf::from("/this/path/should/not/exist/at/all/herdr-watch-test");
        let hub = EventHub::default();
        let watcher = WorkspaceWatcher::spawn("ws-missing".into(), missing, hub.clone());
        tokio::time::sleep(Duration::from_millis(100)).await;
        // no panic, no events
        assert_eq!(file_event_count(&hub, "ws-missing"), 0);
        drop(watcher);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drop_aborts_task() {
        let dir = tempfile::tempdir().unwrap();
        let hub = EventHub::default();
        let watcher = WorkspaceWatcher::spawn("ws-drop".into(), dir.path().into(), hub.clone());
        tokio::time::sleep(Duration::from_millis(150)).await;
        drop(watcher);
        // give the runtime a beat to process the abort
        tokio::time::sleep(Duration::from_millis(100)).await;
        // a write after drop must not produce events
        let before = file_event_count(&hub, "ws-drop");
        std::fs::write(dir.path().join("after.txt"), "x").unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        let after = file_event_count(&hub, "ws-drop");
        assert_eq!(before, after, "watcher should not emit after drop");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cross_worktree_isolation() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let hub = EventHub::default();
        let watcher_a = WorkspaceWatcher::spawn("ws-a".into(), dir_a.path().into(), hub.clone());
        let watcher_b = WorkspaceWatcher::spawn("ws-b".into(), dir_b.path().into(), hub.clone());
        tokio::time::sleep(Duration::from_millis(200)).await;

        std::fs::write(dir_a.path().join("only-a.txt"), "x").unwrap();

        let _ = tokio::task::spawn_blocking({
            let hub = hub.clone();
            move || {
                wait_for_event(
                    &hub,
                    |env| {
                        matches!(
                            &env.data,
                            EventData::WorkspaceFilesChanged { workspace_id, .. }
                                if workspace_id == "ws-a"
                        )
                    },
                    Duration::from_millis(1500),
                )
            }
        })
        .await
        .unwrap();

        // give B a fair chance to (incorrectly) fire
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(file_event_count(&hub, "ws-a") >= 1);
        assert_eq!(file_event_count(&hub, "ws-b"), 0);
        drop(watcher_a);
        drop(watcher_b);
    }

    #[test]
    fn noise_filter_rejects_known_noise() {
        assert!(!noise_filter(Path::new("/repo/target/debug/x")));
        assert!(!noise_filter(Path::new("/repo/node_modules/a/b")));
        assert!(!noise_filter(Path::new("/repo/.venv/lib/x")));
    }

    #[test]
    fn noise_filter_allows_curated_git() {
        assert!(noise_filter(Path::new("/repo/.git/HEAD")));
        assert!(noise_filter(Path::new("/repo/.git/index")));
        assert!(noise_filter(Path::new("/repo/.git/refs/heads/main")));
    }

    #[test]
    fn noise_filter_rejects_other_git() {
        assert!(!noise_filter(Path::new("/repo/.git/objects/ab/cd")));
        assert!(!noise_filter(Path::new("/repo/.git/logs/HEAD")));
    }

    #[test]
    fn noise_filter_allows_regular_files() {
        assert!(noise_filter(Path::new("/repo/src/main.rs")));
        assert!(noise_filter(Path::new("/repo/Cargo.toml")));
    }
}
