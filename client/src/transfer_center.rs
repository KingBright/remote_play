use remote_core::file_transfer_runtime::FileTransferEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    Outgoing,
    Incoming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferStatus {
    Running,
    Completed,
    Cancelled,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferCancelTarget {
    Transfer(u64),
    Group(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferEntrySnapshot {
    pub direction: TransferDirection,
    pub label: String,
    pub detail: String,
    pub transferred_bytes: u64,
    pub total_bytes: u64,
    pub status: TransferStatus,
    pub cancel_target: Option<TransferCancelTarget>,
}

impl TransferEntrySnapshot {
    pub fn progress(&self) -> f32 {
        if self.total_bytes == 0 {
            return if self.status == TransferStatus::Completed {
                1.0
            } else {
                0.0
            };
        }
        (self.transferred_bytes as f32 / self.total_bytes as f32).clamp(0.0, 1.0)
    }

    pub fn is_running(&self) -> bool {
        self.status == TransferStatus::Running
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TransferKey {
    Transfer {
        direction: TransferDirection,
        transfer_id: u64,
    },
    Group {
        direction: TransferDirection,
        group_id: u64,
    },
}

#[derive(Debug, Clone)]
struct TransferEntry {
    key: TransferKey,
    snapshot: TransferEntrySnapshot,
    updated_seq: u64,
}

#[derive(Debug, Clone)]
struct ChildProgress {
    direction: TransferDirection,
    transfer_id: u64,
    group_id: Option<u64>,
    transferred_bytes: u64,
    total_bytes: u64,
}

#[derive(Debug, Default, Clone)]
pub struct TransferCenterState {
    entries: Vec<TransferEntry>,
    child_progress: Vec<ChildProgress>,
    seq: u64,
}

impl TransferCenterState {
    pub fn apply_event(&mut self, event: &FileTransferEvent) {
        match event {
            FileTransferEvent::OutgoingGroupStarted {
                group_id,
                file_count,
                total_size_bytes,
            } => self.upsert_group(
                TransferDirection::Outgoing,
                *group_id,
                format!("{file_count} files"),
                *total_size_bytes,
                TransferStatus::Running,
            ),
            FileTransferEvent::OutgoingStarted {
                transfer_id,
                group,
                name,
                size_bytes,
                ..
            } => {
                if let Some(group) = group {
                    self.record_child(
                        TransferDirection::Outgoing,
                        *transfer_id,
                        Some(group.group_id),
                        0,
                        *size_bytes,
                    );
                } else {
                    self.upsert_transfer(
                        *transfer_id,
                        TransferEntrySnapshot {
                            direction: TransferDirection::Outgoing,
                            label: name.clone(),
                            detail: String::new(),
                            transferred_bytes: 0,
                            total_bytes: *size_bytes,
                            status: TransferStatus::Running,
                            cancel_target: Some(TransferCancelTarget::Transfer(*transfer_id)),
                        },
                    );
                }
            }
            FileTransferEvent::OutgoingProgress {
                transfer_id,
                sent_bytes,
                total_size,
                ..
            } => self.record_progress(
                TransferDirection::Outgoing,
                *transfer_id,
                *sent_bytes,
                *total_size,
            ),
            FileTransferEvent::OutgoingCompleted {
                transfer_id, group, ..
            } => {
                if let Some(group) = group {
                    self.mark_group_if_known(
                        TransferDirection::Outgoing,
                        group.group_id,
                        TransferStatus::Completed,
                    );
                } else {
                    self.mark_transfer(
                        TransferDirection::Outgoing,
                        *transfer_id,
                        TransferStatus::Completed,
                    );
                }
            }
            FileTransferEvent::OutgoingGroupCompleted { group_id, .. } => {
                self.mark_group_if_known(
                    TransferDirection::Outgoing,
                    *group_id,
                    TransferStatus::Completed,
                );
            }
            FileTransferEvent::OutgoingCancelled {
                transfer_id, group, ..
            } => {
                if let Some(group) = group {
                    self.mark_group_if_known(
                        TransferDirection::Outgoing,
                        group.group_id,
                        TransferStatus::Cancelled,
                    );
                } else {
                    self.mark_transfer(
                        TransferDirection::Outgoing,
                        *transfer_id,
                        TransferStatus::Cancelled,
                    );
                }
            }
            FileTransferEvent::OutgoingGroupCancelled { group_id } => {
                self.mark_group_if_known(
                    TransferDirection::Outgoing,
                    *group_id,
                    TransferStatus::Cancelled,
                );
            }
            FileTransferEvent::IncomingStarted {
                transfer_id,
                group,
                name,
                size_bytes,
                path,
                ..
            } => {
                if let Some(group) = group {
                    self.upsert_group(
                        TransferDirection::Incoming,
                        group.group_id,
                        format!("{} files", group.file_count),
                        group.group_total_size_bytes,
                        TransferStatus::Running,
                    );
                    self.record_child(
                        TransferDirection::Incoming,
                        *transfer_id,
                        Some(group.group_id),
                        0,
                        *size_bytes,
                    );
                } else {
                    self.upsert_transfer(
                        *transfer_id,
                        TransferEntrySnapshot {
                            direction: TransferDirection::Incoming,
                            label: name.clone(),
                            detail: path.display().to_string(),
                            transferred_bytes: 0,
                            total_bytes: *size_bytes,
                            status: TransferStatus::Running,
                            cancel_target: Some(TransferCancelTarget::Transfer(*transfer_id)),
                        },
                    );
                }
            }
            FileTransferEvent::IncomingProgress {
                transfer_id,
                received_bytes,
                total_size,
                ..
            } => self.record_progress(
                TransferDirection::Incoming,
                *transfer_id,
                *received_bytes,
                *total_size,
            ),
            FileTransferEvent::IncomingCompleted {
                transfer_id,
                group,
                size_bytes,
                ..
            } => {
                if let Some(_group) = group {
                    self.record_progress(
                        TransferDirection::Incoming,
                        *transfer_id,
                        *size_bytes,
                        *size_bytes,
                    );
                } else {
                    self.mark_transfer(
                        TransferDirection::Incoming,
                        *transfer_id,
                        TransferStatus::Completed,
                    );
                }
            }
            FileTransferEvent::IncomingGroupCompleted { group_id, .. } => {
                self.mark_group_if_known(
                    TransferDirection::Incoming,
                    *group_id,
                    TransferStatus::Completed,
                );
            }
            FileTransferEvent::IncomingCancelled {
                transfer_id, group, ..
            } => {
                if let Some(group) = group {
                    self.mark_group_if_known(
                        TransferDirection::Incoming,
                        group.group_id,
                        TransferStatus::Cancelled,
                    );
                } else {
                    self.mark_transfer(
                        TransferDirection::Incoming,
                        *transfer_id,
                        TransferStatus::Cancelled,
                    );
                }
            }
            FileTransferEvent::IncomingGroupCancelled { group_id } => {
                self.mark_group_if_known(
                    TransferDirection::Incoming,
                    *group_id,
                    TransferStatus::Cancelled,
                );
            }
            FileTransferEvent::Error {
                transfer_id: Some(transfer_id),
                message,
            } => {
                self.mark_transfer(
                    TransferDirection::Outgoing,
                    *transfer_id,
                    TransferStatus::Error,
                );
                self.update_detail_for_transfer(*transfer_id, message.clone());
            }
            FileTransferEvent::Error { .. } => {}
        }
    }

    pub fn snapshots(&self) -> Vec<TransferEntrySnapshot> {
        let mut entries = self.entries.clone();
        entries.sort_by(|left, right| right.updated_seq.cmp(&left.updated_seq));
        entries
            .into_iter()
            .map(|entry| entry.snapshot)
            .collect::<Vec<_>>()
    }

    fn upsert_group(
        &mut self,
        direction: TransferDirection,
        group_id: u64,
        detail: String,
        total_bytes: u64,
        status: TransferStatus,
    ) {
        let label = format!("Group #{group_id}");
        let cancel_target =
            (status == TransferStatus::Running).then_some(TransferCancelTarget::Group(group_id));
        let transferred_bytes = if status == TransferStatus::Completed {
            total_bytes
        } else {
            self.group_transferred_bytes(direction, group_id)
        };
        self.upsert_entry(
            TransferKey::Group {
                direction,
                group_id,
            },
            TransferEntrySnapshot {
                direction,
                label,
                detail,
                transferred_bytes,
                total_bytes,
                status,
                cancel_target,
            },
        );
    }

    fn upsert_transfer(&mut self, transfer_id: u64, snapshot: TransferEntrySnapshot) {
        let direction = snapshot.direction;
        self.upsert_entry(
            TransferKey::Transfer {
                direction,
                transfer_id,
            },
            snapshot,
        );
    }

    fn upsert_entry(&mut self, key: TransferKey, snapshot: TransferEntrySnapshot) {
        let updated_seq = self.next_seq();
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.key == key) {
            entry.snapshot = snapshot;
            entry.updated_seq = updated_seq;
            return;
        }
        self.entries.push(TransferEntry {
            key,
            snapshot,
            updated_seq,
        });
    }

    fn record_child(
        &mut self,
        direction: TransferDirection,
        transfer_id: u64,
        group_id: Option<u64>,
        transferred_bytes: u64,
        total_bytes: u64,
    ) {
        if let Some(child) = self
            .child_progress
            .iter_mut()
            .find(|child| child.direction == direction && child.transfer_id == transfer_id)
        {
            child.group_id = group_id;
            child.transferred_bytes = transferred_bytes;
            child.total_bytes = total_bytes;
            return;
        }
        self.child_progress.push(ChildProgress {
            direction,
            transfer_id,
            group_id,
            transferred_bytes,
            total_bytes,
        });
    }

    fn record_progress(
        &mut self,
        direction: TransferDirection,
        transfer_id: u64,
        transferred_bytes: u64,
        total_bytes: u64,
    ) {
        let group_id = self
            .child_progress
            .iter()
            .find(|child| child.direction == direction && child.transfer_id == transfer_id)
            .and_then(|child| child.group_id);
        self.record_child(
            direction,
            transfer_id,
            group_id,
            transferred_bytes,
            total_bytes,
        );

        if let Some(group_id) = group_id {
            self.refresh_group_progress(direction, group_id);
        } else {
            let updated_seq = self.next_seq();
            if let Some(entry) = self.find_transfer_mut(direction, transfer_id) {
                entry.snapshot.transferred_bytes = transferred_bytes;
                entry.snapshot.total_bytes = total_bytes;
                entry.updated_seq = updated_seq;
            }
        }
    }

    fn refresh_group_progress(&mut self, direction: TransferDirection, group_id: u64) {
        let transferred_bytes = self.group_transferred_bytes(direction, group_id);
        let updated_seq = self.next_seq();
        if let Some(entry) = self.find_group_mut(direction, group_id) {
            entry.snapshot.transferred_bytes = transferred_bytes;
            entry.updated_seq = updated_seq;
        }
    }

    fn group_transferred_bytes(&self, direction: TransferDirection, group_id: u64) -> u64 {
        self.child_progress
            .iter()
            .filter(|child| child.direction == direction && child.group_id == Some(group_id))
            .map(|child| child.transferred_bytes)
            .sum()
    }

    fn mark_transfer(
        &mut self,
        direction: TransferDirection,
        transfer_id: u64,
        status: TransferStatus,
    ) {
        let updated_seq = self.next_seq();
        if let Some(entry) = self.find_transfer_mut(direction, transfer_id) {
            entry.snapshot.status = status;
            entry.snapshot.cancel_target = None;
            if status == TransferStatus::Completed {
                entry.snapshot.transferred_bytes = entry.snapshot.total_bytes;
            }
            entry.updated_seq = updated_seq;
        }
    }

    fn mark_group_if_known(
        &mut self,
        direction: TransferDirection,
        group_id: u64,
        status: TransferStatus,
    ) {
        let updated_seq = self.next_seq();
        if let Some(entry) = self.find_group_mut(direction, group_id) {
            entry.snapshot.status = status;
            entry.snapshot.cancel_target = None;
            if status == TransferStatus::Completed {
                entry.snapshot.transferred_bytes = entry.snapshot.total_bytes;
            }
            entry.updated_seq = updated_seq;
        }
    }

    fn update_detail_for_transfer(&mut self, transfer_id: u64, detail: String) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| {
            matches!(
                entry.key,
                TransferKey::Transfer {
                    transfer_id: id,
                    ..
                } if id == transfer_id
            )
        }) {
            entry.snapshot.detail = detail;
        }
    }

    fn find_transfer_mut(
        &mut self,
        direction: TransferDirection,
        transfer_id: u64,
    ) -> Option<&mut TransferEntry> {
        self.entries.iter_mut().find(|entry| {
            entry.key
                == TransferKey::Transfer {
                    direction,
                    transfer_id,
                }
        })
    }

    fn find_group_mut(
        &mut self,
        direction: TransferDirection,
        group_id: u64,
    ) -> Option<&mut TransferEntry> {
        self.entries.iter_mut().find(|entry| {
            entry.key
                == TransferKey::Group {
                    direction,
                    group_id,
                }
        })
    }

    fn next_seq(&mut self) -> u64 {
        self.seq = self.seq.saturating_add(1);
        self.seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::FileTransferGroup;
    use std::path::PathBuf;

    #[test]
    fn tracks_single_file_progress_and_completion() {
        let mut state = TransferCenterState::default();
        state.apply_event(&FileTransferEvent::OutgoingStarted {
            transfer_id: 1,
            file_object_id: 2,
            group: None,
            name: "movie.mov".to_string(),
            size_bytes: 100,
            total_chunks: 4,
        });
        state.apply_event(&FileTransferEvent::OutgoingProgress {
            transfer_id: 1,
            file_object_id: 2,
            sent_chunks: 2,
            total_chunks: 4,
            sent_bytes: 50,
            total_size: 100,
        });
        state.apply_event(&FileTransferEvent::OutgoingCompleted {
            transfer_id: 1,
            file_object_id: 2,
            group: None,
        });

        let entries = state.snapshots();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "movie.mov");
        assert_eq!(entries[0].status, TransferStatus::Completed);
        assert_eq!(entries[0].progress(), 1.0);
        assert_eq!(entries[0].cancel_target, None);
    }

    #[test]
    fn aggregates_group_progress_and_cancellation() {
        let mut state = TransferCenterState::default();
        let group = FileTransferGroup {
            group_id: 7,
            file_index: 0,
            file_count: 2,
            relative_path: "Folder/a.txt".to_string(),
            group_total_size_bytes: 300,
            group_checksum_crc32: 0,
        };
        state.apply_event(&FileTransferEvent::OutgoingGroupStarted {
            group_id: 7,
            file_count: 2,
            total_size_bytes: 300,
        });
        state.apply_event(&FileTransferEvent::OutgoingStarted {
            transfer_id: 10,
            file_object_id: 11,
            group: Some(group.clone()),
            name: "a.txt".to_string(),
            size_bytes: 100,
            total_chunks: 1,
        });
        state.apply_event(&FileTransferEvent::OutgoingProgress {
            transfer_id: 10,
            file_object_id: 11,
            sent_chunks: 1,
            total_chunks: 1,
            sent_bytes: 100,
            total_size: 100,
        });
        state.apply_event(&FileTransferEvent::OutgoingGroupCancelled { group_id: 7 });

        let entries = state.snapshots();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "Group #7");
        assert_eq!(entries[0].transferred_bytes, 100);
        assert_eq!(entries[0].total_bytes, 300);
        assert_eq!(entries[0].status, TransferStatus::Cancelled);
        assert_eq!(entries[0].cancel_target, None);
    }

    #[test]
    fn exposes_running_cancel_targets() {
        let mut state = TransferCenterState::default();
        state.apply_event(&FileTransferEvent::IncomingStarted {
            transfer_id: 3,
            file_object_id: 4,
            group: None,
            name: "paste.bin".to_string(),
            size_bytes: 10,
            path: PathBuf::from("/tmp/paste.bin"),
        });

        let entries = state.snapshots();
        assert_eq!(
            entries[0].cancel_target,
            Some(TransferCancelTarget::Transfer(3))
        );
        assert!(entries[0].is_running());
    }
}
