use std::borrow::Cow;
use std::path::PathBuf;

use ratatui::layout::Rect;
use ratatui::Frame;

use crate::input::TerminalKey;

pub mod diff;

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
    /// `palette` provides theme tokens so view kinds stay theme-aware
    /// without having to cache anything.
    fn render(
        &self,
        frame: &mut Frame<'_>,
        rect: Rect,
        focused: bool,
        palette: &crate::app::state::Palette,
    );

    /// How many rows of content sit above the current viewport (i.e. how
    /// far down the content the view has scrolled). Default 0 for kinds
    /// that don't scroll. Used to build a synthetic `ScrollMetrics` so
    /// selection coordinates stay stable while the user scrolls the view.
    fn scroll_offset(&self) -> u16 {
        0
    }

    /// Extract the text covered by `sel` for clipboard copy. Default
    /// returns `None` so kinds that don't support selection fall through
    /// to "nothing copied".
    fn extract_selection(&self, _sel: &crate::selection::Selection) -> Option<String> {
        None
    }

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
    /// Manual refresh hook (e.g. an 'r' keybind). PR #3 implements it on
    /// `DiffView` but nothing calls into it yet; future PRs route a
    /// global "refresh" keybinding to view panes through this method.
    #[allow(dead_code)]
    fn refresh(&mut self);

    /// Returns the wire-level identifier and serialized options for this
    /// kind so the server can describe it back to clients and persist it.
    /// `kind_id` is the snake_case discriminator (e.g. `"diff"`); `options`
    /// is the JSON payload the kind's wire constructor would accept.
    fn wire_descriptor(&self) -> (String, serde_json::Value);
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
    pub(crate) files_changed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
impl TestPlaceholderViewHandle {
    pub(crate) fn key_count(&self) -> usize {
        self.keys.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub(crate) fn mouse_count(&self) -> usize {
        self.mouse.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub(crate) fn files_changed_count(&self) -> usize {
        self.files_changed
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
pub(crate) struct TestPlaceholderView {
    handle: TestPlaceholderViewHandle,
    selection_text: Option<String>,
}

#[cfg(test)]
impl TestPlaceholderView {
    pub(crate) fn new() -> (Self, TestPlaceholderViewHandle) {
        let handle = TestPlaceholderViewHandle::default();
        (
            Self {
                handle: handle.clone(),
                selection_text: None,
            },
            handle,
        )
    }

    /// Configure a fixed string for `extract_selection` to return,
    /// regardless of the actual `Selection`. Used to drive the
    /// view-pane copy path in tests.
    pub(crate) fn with_selection_text(mut self, text: impl Into<String>) -> Self {
        self.selection_text = Some(text.into());
        self
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
    fn render(
        &self,
        _frame: &mut Frame<'_>,
        _rect: Rect,
        _focused: bool,
        _palette: &crate::app::state::Palette,
    ) {
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
    fn on_files_changed(&mut self, _paths: &[PathBuf]) {
        self.handle
            .files_changed
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    fn refresh(&mut self) {}
    fn extract_selection(&self, _sel: &crate::selection::Selection) -> Option<String> {
        self.selection_text.clone()
    }
    fn wire_descriptor(&self) -> (String, serde_json::Value) {
        ("test".to_string(), serde_json::json!({}))
    }
}
