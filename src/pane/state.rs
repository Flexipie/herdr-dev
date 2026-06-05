use crate::pane::view::ViewPaneState;
use crate::terminal::TerminalId;

/// What's attached to a pane: a live PTY terminal, or a non-PTY view kind
/// (diff / file / tree / ...).
pub enum PaneAttachment {
    Pty { terminal_id: TerminalId },
    View(ViewPaneState),
}

/// Viewport state for a pane.
///
/// Terminal identity, cwd, labels, and agent metadata live in TerminalState.
pub struct PaneState {
    pub(crate) attachment: PaneAttachment,
    /// Whether the user has seen this pane since its last state change to Idle.
    /// False = "Done" (agent finished while user was in another workspace).
    pub seen: bool,
}

impl PaneState {
    pub fn new_pty(terminal_id: TerminalId) -> Self {
        Self {
            attachment: PaneAttachment::Pty { terminal_id },
            seen: true,
        }
    }

    pub fn new_view(kind: Box<dyn crate::pane::ViewKind>) -> Self {
        Self {
            attachment: PaneAttachment::View(ViewPaneState::new(kind)),
            seen: true,
        }
    }

    pub fn terminal_id(&self) -> Option<&TerminalId> {
        match &self.attachment {
            PaneAttachment::Pty { terminal_id } => Some(terminal_id),
            PaneAttachment::View(_) => None,
        }
    }

    pub fn attachment(&self) -> &PaneAttachment {
        &self.attachment
    }

    pub fn attachment_mut(&mut self) -> &mut PaneAttachment {
        &mut self.attachment
    }

    pub fn into_attachment(self) -> PaneAttachment {
        self.attachment
    }

    #[cfg(test)]
    pub(crate) fn new_view_placeholder() -> Self {
        use crate::pane::view::TestPlaceholderView;
        Self {
            attachment: PaneAttachment::View(ViewPaneState::new(Box::new(
                TestPlaceholderView::default(),
            ))),
            seen: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::TerminalId;

    #[test]
    fn pty_constructor_sets_attachment() {
        let tid = TerminalId::alloc();
        let pane = PaneState::new_pty(tid.clone());
        assert_eq!(pane.terminal_id(), Some(&tid));
        assert!(matches!(pane.attachment(), PaneAttachment::Pty { .. }));
    }

    #[test]
    fn view_placeholder_has_no_terminal() {
        let pane = PaneState::new_view_placeholder();
        assert_eq!(pane.terminal_id(), None);
        assert!(matches!(pane.attachment(), PaneAttachment::View(_)));
    }
}
