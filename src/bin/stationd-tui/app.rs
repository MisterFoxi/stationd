use std::time::Instant;

use ratatui::widgets::ListState;
use stationd::proto::{schedule, station};

use super::client::{ReadResult, Snapshot};

/// Keep the last good value, but explicitly mark it stale after a failed read.
pub struct Resource<T> {
    pub value: Option<T>,
    pub error: Option<String>,
    pub updated: Option<Instant>,
}

impl<T> Default for Resource<T> {
    fn default() -> Self {
        Self { value: None, error: None, updated: None }
    }
}

impl<T> Resource<T> {
    pub fn apply(&mut self, result: ReadResult<T>) {
        match result {
            Ok(value) => {
                self.value = Some(value);
                self.error = None;
                self.updated = Some(Instant::now());
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub fn label(&self) -> String {
        let age = self.updated.map(|t| format!("{}s ago", t.elapsed().as_secs()));
        match (&self.error, age) {
            (Some(error), Some(age)) => format!("STALE - last success {age} | {error}"),
            (Some(error), None) => format!("UNAVAILABLE | {error}"),
            (None, Some(age)) => format!("Updated {age}"),
            (None, None) => "Waiting for first response...".into(),
        }
    }
}

#[derive(Default)]
pub struct App {
    pub tab: usize,
    pub status: Resource<station::StatusReply>,
    pub playlists: Resource<Vec<station::PlaylistSummary>>,
    pub rules: Resource<Vec<schedule::Rule>>,
    pub playlist_selection: ListState,
    pub rule_selection: ListState,
    pub detail_scroll: u16,
    pub auto: bool,
    pub loading: bool,
    pub help: bool,
}

fn preserve_selection(state: &mut ListState, ids: &[&str], previous_id: Option<&str>) {
    let selected = previous_id.and_then(|id| ids.iter().position(|candidate| *candidate == id))
        .or_else(|| if ids.is_empty() { None } else { Some(state.selected().unwrap_or(0).min(ids.len() - 1)) });
    state.select(selected);
}

impl App {
    pub fn apply(&mut self, snapshot: Snapshot) {
        let playlist_id = self.playlists.value.as_ref()
            .and_then(|items| self.playlist_selection.selected().and_then(|i| items.get(i)))
            .map(|p| p.id.clone());
        let rule_id = self.rules.value.as_ref()
            .and_then(|items| self.rule_selection.selected().and_then(|i| items.get(i)))
            .map(|r| r.id.clone());
        self.status.apply(snapshot.status);
        self.playlists.apply(snapshot.playlists);
        self.rules.apply(snapshot.rules);
        if let Some(items) = &self.playlists.value {
            let ids: Vec<&str> = items.iter().map(|p| p.id.as_str()).collect();
            preserve_selection(&mut self.playlist_selection, &ids, playlist_id.as_deref());
        }
        if let Some(items) = &self.rules.value {
            let ids: Vec<&str> = items.iter().map(|r| r.id.as_str()).collect();
            preserve_selection(&mut self.rule_selection, &ids, rule_id.as_deref());
        }
        self.loading = false;
    }

    pub fn select_tab(&mut self, tab: usize) {
        self.tab = tab % 3;
        self.detail_scroll = 0;
    }

    pub fn move_selection(&mut self, delta: isize) {
        let (selection, len) = match self.tab {
            1 => (&mut self.playlist_selection, self.playlists.value.as_ref().map_or(0, Vec::len)),
            2 => (&mut self.rule_selection, self.rules.value.as_ref().map_or(0, Vec::len)),
            _ => return,
        };
        selection.select(if len == 0 { None } else {
            Some(selection.selected().unwrap_or(0).saturating_add_signed(delta).min(len - 1))
        });
        self.detail_scroll = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_tracks_identity_across_reorder_and_removal() {
        let mut state = ListState::default().with_selected(Some(1));
        preserve_selection(&mut state, &["b", "a"], Some("b"));
        assert_eq!(state.selected(), Some(0));
        preserve_selection(&mut state, &["a"], Some("b"));
        assert_eq!(state.selected(), Some(0));
        preserve_selection(&mut state, &[], Some("a"));
        assert_eq!(state.selected(), None);
    }

    #[test]
    fn failure_keeps_last_good_value_and_recovery_clears_error() {
        let mut resource = Resource::default();
        resource.apply(Ok(42));
        let updated = resource.updated;
        resource.apply(Err("connection refused".into()));
        assert_eq!(resource.value, Some(42));
        assert_eq!(resource.updated, updated);
        assert!(resource.label().starts_with("STALE"));
        resource.apply(Ok(43));
        assert_eq!(resource.value, Some(43));
        assert!(resource.error.is_none());
    }
}
