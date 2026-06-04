// PR #2 sets up the trait + types so PR #3+ can plug in view kinds.
// In PR #2 the bin build sees no live `ViewKind` implementor (only the
// `#[cfg(test)] TestPlaceholderView`), so several items below are
// dead code by design until PR #3.
#![allow(dead_code)]

use std::borrow::Cow;
use std::path::PathBuf;

use ratatui::layout::Rect;
use ratatui::Frame;

use crate::input::TerminalKey;

/// What a view pane's input handler decided about a key/mouse event.
#[derive(Debug, Clone, Copy)]
pub enum ViewKeyOutcome {
    Handled,
    NotHandled,
}

/// A pluggable view-pane kind (diff / file / tree / ...). PR #2 defines
/// the trait; PR #3+ provide implementors.
pub trait ViewKind: Send {
    /// Title shown in the pane border.
    fn title(&self) -> Cow<'_, str>;

    /// Render the view into `rect`. `focused` is true when this pane has
    /// keyboard focus and the app is in Terminal mode. View kinds are
    /// responsible for their own unfocused/dim styling — the renderer
    /// does not dim view-pane contents the way it dims PTY contents.
    fn render(&self, frame: &mut Frame<'_>, rect: Rect, focused: bool);

    /// Handle a keyboard event routed to this pane. In PR #2 the outcome
    /// is informational only: there is no fallback path once dispatch
    /// reaches the view arm. PR #3 may propagate `NotHandled` up to
    /// `AppState` for global keybinds.
    fn handle_key(&mut self, key: &TerminalKey) -> ViewKeyOutcome;

    /// Handle a mouse event routed to this pane. `event.column` /
    /// `event.row` are screen-absolute; `rect` is the pane's inner rect.
    /// Translate as needed.
    fn handle_mouse(&mut self, event: &crossterm::event::MouseEvent, rect: Rect) -> ViewKeyOutcome;

    fn on_files_changed(&mut self, paths: &[PathBuf]);
    fn refresh(&mut self);
}

pub struct ViewPaneState {
    pub(crate) kind: Box<dyn ViewKind>,
}

impl ViewPaneState {
    pub fn new(kind: Box<dyn ViewKind>) -> Self {
        Self { kind }
    }

    pub fn kind(&self) -> &dyn ViewKind {
        self.kind.as_ref()
    }

    pub fn kind_mut(&mut self) -> &mut dyn ViewKind {
        self.kind.as_mut()
    }
}

#[cfg(test)]
#[derive(Default, Clone)]
pub(crate) struct TestPlaceholderViewHandle {
    pub(crate) keys: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub(crate) mouse: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
impl TestPlaceholderViewHandle {
    pub(crate) fn key_count(&self) -> usize {
        self.keys.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub(crate) fn mouse_count(&self) -> usize {
        self.mouse.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
pub(crate) struct TestPlaceholderView {
    handle: TestPlaceholderViewHandle,
}

#[cfg(test)]
impl TestPlaceholderView {
    pub(crate) fn new() -> (Self, TestPlaceholderViewHandle) {
        let handle = TestPlaceholderViewHandle::default();
        (
            Self {
                handle: handle.clone(),
            },
            handle,
        )
    }
}

#[cfg(test)]
impl Default for TestPlaceholderView {
    fn default() -> Self {
        Self::new().0
    }
}

#[cfg(test)]
impl ViewKind for TestPlaceholderView {
    fn title(&self) -> Cow<'_, str> {
        Cow::Borrowed("view (test)")
    }
    fn render(&self, _frame: &mut Frame<'_>, _rect: Rect, _focused: bool) {
        tracing::debug!("view pane render: TestPlaceholderView");
    }
    fn handle_key(&mut self, _key: &TerminalKey) -> ViewKeyOutcome {
        self.handle
            .keys
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        ViewKeyOutcome::Handled
    }
    fn handle_mouse(
        &mut self,
        _event: &crossterm::event::MouseEvent,
        _rect: Rect,
    ) -> ViewKeyOutcome {
        self.handle
            .mouse
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        ViewKeyOutcome::Handled
    }
    fn on_files_changed(&mut self, _paths: &[PathBuf]) {}
    fn refresh(&mut self) {}
}
