//! In-memory Files fixtures for the gallery and unit tests.

use super::files;

pub(crate) struct FixtureFs {
    pub(crate) kind: festerm_sftp::PaneKind,
    pub(crate) home: &'static str,
    pub(crate) entries: Vec<festerm_sftp::Entry>,
}

pub(crate) fn fixture_entry(
    name: &str,
    directory: bool,
    size: u64,
    modified: i64,
) -> festerm_sftp::Entry {
    festerm_sftp::Entry {
        name: name.to_owned(),
        display_name: name.to_owned(),
        kind: if directory {
            festerm_sftp::EntryKind::Directory
        } else {
            festerm_sftp::EntryKind::File
        },
        size: (!directory).then_some(size),
        modified: Some(modified),
        permissions: None,
        symlink: None,
        hidden: false,
        name_safe: true,
    }
}

impl festerm_sftp::PaneFs for FixtureFs {
    fn kind(&self) -> festerm_sftp::PaneKind {
        self.kind
    }
    fn home(&self) -> String {
        self.home.to_owned()
    }
    fn list(
        &self,
        path: &str,
        _: &festerm_sftp::ListOptions,
    ) -> festerm_sftp::Result<festerm_sftp::Listing> {
        Ok(festerm_sftp::Listing {
            path: path.to_owned(),
            entries: self.entries.clone(),
            scanned: self.entries.len(),
            hidden_count: 0,
            truncated: false,
        })
    }
    fn stat(&self, path: &str) -> festerm_sftp::Result<festerm_sftp::Entry> {
        Ok(fixture_entry(path, true, 0, 0))
    }
    fn mkdir(&self, _: &str) -> festerm_sftp::Result<()> {
        Ok(())
    }
    fn rename(&self, _: &str, _: &str) -> festerm_sftp::Result<()> {
        Ok(())
    }
    fn delete(&self, _: &str, _: bool) -> festerm_sftp::Result<()> {
        Ok(())
    }
    fn join(&self, directory: &str, name: &str) -> festerm_sftp::Result<String> {
        Ok(format!("{}/{name}", directory.trim_end_matches('/')))
    }
    fn parent(&self, path: &str) -> Option<String> {
        path.trim_end_matches('/')
            .rsplit_once('/')
            .map(|(parent, _)| if parent.is_empty() { "/" } else { parent }.to_owned())
    }
}

pub(crate) struct FixtureTransfers {
    pub(crate) rows: Vec<files::TransferRow>,
    pub(crate) resolved: std::sync::Mutex<Vec<(u64, festerm_sftp::ConflictPolicy, bool)>>,
}

impl FixtureTransfers {
    pub(crate) fn new(rows: Vec<files::TransferRow>) -> Self {
        Self {
            rows,
            resolved: Default::default(),
        }
    }
}

impl files::Transfers for FixtureTransfers {
    fn rows(&self) -> Vec<files::TransferRow> {
        self.rows.clone()
    }
    fn enqueue(&self, _: festerm_sftp::Direction, _: &str, _: &str) -> Result<u64, String> {
        Err("Fixture transfers are static".to_owned())
    }
    fn cancel(&self, _: u64) {}
    fn resolve(&self, id: u64, policy: festerm_sftp::ConflictPolicy, remember: bool) {
        self.resolved.lock().unwrap().push((id, policy, remember));
    }
    fn clear_finished(&self) {}
}

pub(crate) fn transfer_rows(conflict: bool) -> Vec<files::TransferRow> {
    use festerm_sftp::Direction;
    use files::{ConflictView, FileFacts, RowState, TransferRow};
    let mut rows = vec![
        TransferRow {
            id: 1,
            name: "app-2.4.2.tar.gz".to_owned(),
            direction: Direction::Upload,
            target: "/srv/app/releases".to_owned(),
            state: RowState::Active,
            done: 29_400_000,
            total: Some(38_400_000),
            rate: 3_200_000.0,
            conflict: None,
            error: None,
        },
        TransferRow {
            id: 2,
            name: "deploy.log".to_owned(),
            direction: Direction::Download,
            target: "This iPhone".to_owned(),
            state: RowState::Done,
            done: 182_000,
            total: Some(182_000),
            rate: 0.0,
            conflict: None,
            error: None,
        },
        TransferRow {
            id: 3,
            name: "config.yaml".to_owned(),
            direction: Direction::Upload,
            target: "/srv/app/releases".to_owned(),
            state: RowState::Failed,
            done: 0,
            total: Some(2_100),
            rate: 0.0,
            conflict: None,
            error: Some("Permission denied".to_owned()),
        },
    ];
    if conflict {
        rows.insert(
            0,
            TransferRow {
                id: 4,
                name: "app-2.4.1.tar.gz".to_owned(),
                direction: Direction::Upload,
                target: "/srv/app/releases".to_owned(),
                state: RowState::Awaiting,
                done: 0,
                total: Some(38_900_000),
                rate: 0.0,
                conflict: Some(ConflictView {
                    direction: Direction::Upload,
                    iphone: FileFacts {
                        size: Some(38_900_000),
                        modified: Some(1_781_900_000),
                    },
                    remote: FileFacts {
                        size: Some(38_400_000),
                        modified: Some(1_781_800_000),
                    },
                    suggested_name: "app-2.4.1 (1).tar.gz".to_owned(),
                    existing_is_directory: false,
                    likely_identical: false,
                }),
                error: None,
            },
        );
    }
    rows
}
