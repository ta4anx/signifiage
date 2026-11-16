//! Queue service behaviour: transport walk, undo, invariants and import merge.

use signifiage::model::{QueueData, QueueRow, QueueStatus};
use signifiage::services::queue::{ImportRow, QueueError, QueueService};

fn row(name: &str, status: QueueStatus) -> QueueRow {
    QueueRow {
        name: name.to_string(),
        status,
        ..QueueRow::default()
    }
}

fn row_with_ticket(name: &str, ticket: &str, status: QueueStatus) -> QueueRow {
    QueueRow {
        name: name.to_string(),
        ticket: ticket.to_string(),
        status,
        ..QueueRow::default()
    }
}

fn service(rows: Vec<QueueRow>) -> QueueService {
    QueueService::new(QueueData {
        rows,
        ..QueueData::default()
    })
}

fn names(rows: &[QueueRow]) -> Vec<String> {
    rows.iter().map(|row| row.name.clone()).collect()
}

fn import(key: &str, name: &str, note: &str, ticket: &str) -> ImportRow {
    ImportRow {
        source_key: key.to_string(),
        name: name.to_string(),
        note: note.to_string(),
        ticket: ticket.to_string(),
        status: None,
    }
}

#[test]
fn next_recall_done_and_call_walk() {
    let mut queue = service(vec![
        row("A", QueueStatus::Waiting),
        row("B", QueueStatus::Waiting),
        row("C", QueueStatus::Waiting),
    ]);

    assert!(queue.next());
    assert_eq!(queue.data().current().map(|r| r.name.as_str()), Some("A"));
    assert_eq!(queue.data().current().unwrap().call_count, 1);
    assert!(queue.data().current().unwrap().called_at.is_some());

    // Recall bumps the counter and timestamp but nobody moves.
    let first_called_at = queue.data().current().unwrap().called_at.clone();
    assert!(queue.recall());
    assert_eq!(queue.data().current().unwrap().call_count, 2);
    assert_eq!(
        queue.data().current().unwrap().called_at,
        first_called_at,
        "recall writes a fresh timestamp with second precision"
    );

    assert!(queue.next());
    assert_eq!(
        queue.data().current().map(|r| r.name.as_str()),
        Some("B"),
        "the previous current row is marked done"
    );
    assert_eq!(queue.data().count(QueueStatus::Done), 1);
    assert_eq!(
        queue.data().next_ticket,
        1,
        "call_promote does not touch tickets"
    );

    assert!(queue.done());
    assert!(queue.data().current().is_none());
    assert_eq!(queue.data().count(QueueStatus::Done), 2);

    assert!(queue.call(&queue.data().rows[2].id.clone()));
    assert_eq!(queue.data().current().map(|r| r.name.as_str()), Some("C"));
    assert_eq!(queue.data().count(QueueStatus::Waiting), 0);
}

#[test]
fn empty_queue_actions_are_noops() {
    let mut queue = QueueService::new(QueueData::default());
    assert!(!queue.next());
    assert!(!queue.recall());
    assert!(!queue.done());
    assert!(!queue.skip());
    assert!(!queue.call("missing"));
    assert!(!queue.undo());
    assert!(!queue.can_undo());
    assert_eq!(queue.snapshot(), queue.snapshot());
    assert_eq!(queue.snapshot().current, None);
}

#[test]
fn skip_promotes_next_or_clears() {
    let mut queue = service(vec![
        row("A", QueueStatus::Called),
        row("B", QueueStatus::Waiting),
    ]);
    assert!(queue.skip());
    assert_eq!(queue.data().rows[0].status, QueueStatus::Skipped);
    assert_eq!(queue.data().current().map(|r| r.name.as_str()), Some("B"));
    assert_eq!(queue.data().current().unwrap().call_count, 1);

    let mut solo = service(vec![row("Only", QueueStatus::Called)]);
    assert!(solo.skip());
    assert!(solo.data().current().is_none());
    assert_eq!(solo.data().count(QueueStatus::Skipped), 1);

    let mut idle = service(vec![row("Waiting", QueueStatus::Waiting)]);
    assert!(!idle.skip());
}

#[test]
fn call_only_accepts_waiting_rows() {
    let mut queue = service(vec![
        row_with_ticket("A", "001", QueueStatus::Called),
        row_with_ticket("B", "002", QueueStatus::Waiting),
        row_with_ticket("C", "003", QueueStatus::Done),
    ]);
    let done_id = queue.data().rows[2].id.clone();
    assert!(!queue.call(&done_id));
    assert!(!queue.call("nope"));

    let waiting_id = queue.data().rows[1].id.clone();
    assert!(queue.call(&waiting_id));
    assert_eq!(queue.data().current().map(|r| r.name.as_str()), Some("B"));
    assert_eq!(queue.data().rows[0].status, QueueStatus::Done);
    assert_eq!(queue.data().rows[1].call_count, 1);
}

#[test]
fn recall_records_no_undo() {
    let mut queue = service(vec![
        row_with_ticket("A", "001", QueueStatus::Called),
        row_with_ticket("B", "002", QueueStatus::Waiting),
    ]);
    let before_next = queue.data().clone();
    assert!(queue.next());
    assert!(queue.recall());
    assert!(queue.undo());
    assert_eq!(
        queue.data(),
        &before_next,
        "undo removed the next snapshot, proving recall added nothing"
    );
    assert!(!queue.can_undo());
}

#[test]
fn every_mutating_action_is_undoable() {
    let mut queue = service(vec![
        row_with_ticket("A", "001", QueueStatus::Waiting),
        row_with_ticket("B", "002", QueueStatus::Waiting),
    ]);
    assert!(queue.next());
    let after_next = queue.data().clone();
    assert!(queue.skip());
    assert!(queue.undo());
    assert_eq!(queue.data(), &after_next);

    assert!(queue.done());
    assert!(queue.undo());
    assert_eq!(queue.data(), &after_next);

    let id = queue.data().current().unwrap().id.clone();
    assert!(queue.edit(&id, "Renamed", "note", "001").is_ok());
    assert!(queue.undo());
    assert_eq!(queue.data(), &after_next);

    assert!(queue.remove(&id).is_ok());
    assert_eq!(queue.data().rows.len(), 1);
    assert!(queue.undo());
    assert_eq!(queue.data(), &after_next);

    let added = queue.add("New", "").unwrap();
    assert!(queue.undo());
    assert_eq!(queue.data(), &after_next);
    assert!(queue.data().rows.iter().all(|r| r.id != added.id));
}

#[test]
fn undo_stack_is_capped_at_fifty() {
    let mut queue = QueueService::new(QueueData::default());
    for index in 0..60 {
        queue.add(&format!("Person {index}"), "").unwrap();
    }
    assert_eq!(queue.data().rows.len(), 60);

    let mut undone = 0;
    while queue.undo() {
        undone += 1;
    }
    assert_eq!(undone, 50);
    assert_eq!(queue.data().rows.len(), 10);
    assert_eq!(
        names(&queue.data().rows),
        (0..10).map(|i| format!("Person {i}")).collect::<Vec<_>>()
    );
    assert!(!queue.can_undo());
}

#[test]
fn replace_normalizes_to_at_most_one_called_and_clears_undo() {
    let mut queue = service(vec![row("A", QueueStatus::Called)]);
    assert!(queue.add("B", "").is_ok());
    assert!(queue.can_undo());

    queue.replace(QueueData {
        next_ticket: 0,
        rows: vec![
            row("A", QueueStatus::Called),
            row("B", QueueStatus::Called),
            row("C", QueueStatus::Called),
        ],
        ..QueueData::default()
    });
    assert_eq!(queue.data().next_ticket, 1);
    assert_eq!(queue.data().count(QueueStatus::Called), 1);
    assert_eq!(queue.snapshot().called, 1);
    assert!(!queue.can_undo());
}

#[test]
fn ticket_generation_skips_collisions() {
    let mut queue = service(vec![row_with_ticket("A", "001", QueueStatus::Waiting)]);
    let second = queue.add("B", "").unwrap();
    assert_eq!(second.ticket, "002");
    let third = queue.add("C", "").unwrap();
    assert_eq!(third.ticket, "003");
    assert_eq!(queue.data().next_ticket, 4);

    assert!(queue.ticket_taken("002", None));
    assert!(!queue.ticket_taken("002", Some(&second.id)));
    assert!(!queue.ticket_taken("999", None));
}

#[test]
fn add_and_edit_validation() {
    let mut queue = service(vec![row_with_ticket("A", "001", QueueStatus::Waiting)]);
    assert_eq!(queue.add("   ", "note"), Err(QueueError::EmptyName));
    assert_eq!(queue.add("", ""), Err(QueueError::EmptyName));

    let a_id = queue.data().rows[0].id.clone();
    let b = queue.add("B", "note").unwrap();
    assert_eq!(
        queue.edit(&b.id, "   ", "", "004"),
        Err(QueueError::EmptyName)
    );
    assert_eq!(
        queue.edit(&b.id, "B2", "", "001"),
        Err(QueueError::DuplicateTicket)
    );
    assert_eq!(
        queue.edit("missing", "B2", "", ""),
        Err(QueueError::UnknownRow)
    );
    assert_eq!(queue.remove("missing"), Err(QueueError::UnknownRow));

    // Editing a row's own ticket is allowed.
    assert!(queue.edit(&a_id, "A", "", "001").is_ok());
}

#[test]
fn edit_regenerates_blank_ticket_and_needs_no_undo_when_unchanged() {
    let mut queue = service(vec![row_with_ticket("A", "", QueueStatus::Waiting)]);
    let id = queue.data().rows[0].id.clone();
    assert!(queue.edit(&id, "A", "", "").is_ok());
    assert_eq!(queue.data().rows[0].ticket, "001");
    assert!(queue.can_undo());

    queue.clear_undo();
    assert!(queue.edit(&id, "A", "", "001").is_ok());
    assert!(
        !queue.can_undo(),
        "identical edit must not journal an undo entry"
    );
}

#[test]
fn edit_never_touches_status_or_call_history() {
    let mut queue = service(vec![row_with_ticket("A", "001", QueueStatus::Called)]);
    queue.recall();
    let called_at = queue.data().rows[0].called_at.clone();
    let id = queue.data().rows[0].id.clone();
    assert!(queue.edit(&id, "Renamed", "new note", "009").is_ok());
    let row = &queue.data().rows[0];
    assert_eq!(row.status, QueueStatus::Called);
    assert_eq!(row.call_count, 1);
    assert_eq!(row.called_at, called_at);
    assert_eq!(row.name, "Renamed");
    assert_eq!(row.note, "new note");
    assert_eq!(row.ticket, "009");
}

#[test]
fn merge_refreshes_names_but_preserves_statuses() {
    let mut existing = row_with_ticket("Old Name", "7", QueueStatus::Called);
    existing.note = "old note".into();
    existing.call_count = 3;
    existing.called_at = Some("2020-01-01T00:00:00+00:00".into());
    existing.source_key = "7".into();
    let mut queue = service(vec![existing]);

    let report = queue.merge_import(&[ImportRow {
        source_key: "7".into(),
        name: "New Name".into(),
        note: "new note".into(),
        ticket: "7".into(),
        status: Some(QueueStatus::Done),
    }]);
    assert_eq!(
        report,
        signifiage::services::queue::ImportReport {
            added: 0,
            updated: 1,
            changed: true,
        }
    );
    let row = &queue.data().rows[0];
    assert_eq!(row.name, "New Name");
    assert_eq!(row.note, "new note");
    assert_eq!(
        row.status,
        QueueStatus::Called,
        "status is never overwritten"
    );
    assert_eq!(row.call_count, 3);
    assert_eq!(row.called_at.as_deref(), Some("2020-01-01T00:00:00+00:00"));

    assert!(queue.undo());
    assert_eq!(queue.data().rows[0].name, "Old Name");
    assert_eq!(queue.data().rows[0].note, "old note");
    assert!(!queue.can_undo(), "one merge equals one undo entry");
}

#[test]
fn merge_adds_rows_with_unique_tickets_and_status() {
    let mut other = row_with_ticket("Someone", "5", QueueStatus::Waiting);
    other.source_key = "other".into();
    let mut queue = service(vec![
        row_with_ticket("Existing", "5", QueueStatus::Waiting),
        other,
    ]);

    let mut called = import("7", "Called Person", "", "5");
    called.status = Some(QueueStatus::Called);
    let blank = import("8", "Blank Ticket", "", "");
    let skipped = import("9", "  ", "", "010");

    let report = queue.merge_import(&[called, blank, skipped]);
    assert_eq!(report.added, 2);
    assert_eq!(report.updated, 0);
    assert!(report.changed);

    let called_row = queue
        .data()
        .rows
        .iter()
        .find(|r| r.name == "Called Person")
        .unwrap();
    assert_eq!(called_row.ticket, "5-2", "collision gets a suffix");
    assert_eq!(called_row.status, QueueStatus::Called);
    assert_eq!(called_row.call_count, 0);
    assert_eq!(called_row.source_key, "7");

    let blank_row = queue
        .data()
        .rows
        .iter()
        .find(|r| r.name == "Blank Ticket")
        .unwrap();
    assert_eq!(blank_row.ticket, "001");
    assert_eq!(blank_row.status, QueueStatus::Waiting);
    assert!(queue.data().rows.iter().all(|r| r.name != "  "));

    assert!(queue.undo());
    assert_eq!(queue.data().rows.len(), 2);
    assert!(!queue.can_undo());
}

#[test]
fn merge_second_called_row_is_demoted_to_waiting() {
    let mut queue = service(vec![row("Current", QueueStatus::Called)]);
    let mut first = import("1", "First New", "", "");
    first.status = Some(QueueStatus::Called);
    let mut second = import("2", "Second New", "", "");
    second.status = Some(QueueStatus::Called);

    queue.merge_import(&[first, second]);
    assert_eq!(
        queue.snapshot().called,
        1,
        "at most one called row survives any merge"
    );
    assert_eq!(
        queue.data().current().map(|r| r.name.as_str()),
        Some("Current")
    );
}

#[test]
fn noop_merge_reports_unchanged_and_journals_nothing() {
    let mut existing = row_with_ticket("Same", "001", QueueStatus::Done);
    existing.note = "note".into();
    existing.source_key = "1".into();
    let mut queue = service(vec![existing]);

    let report = queue.merge_import(&[
        ImportRow {
            source_key: "1".into(),
            name: "Same".into(),
            note: "note".into(),
            ticket: "001".into(),
            status: Some(QueueStatus::Waiting),
        },
        import("ignored", "   ", "x", "9"),
    ]);
    assert_eq!(report.added, 0);
    assert_eq!(report.updated, 0);
    assert!(!report.changed);
    assert!(!queue.can_undo());
    assert_eq!(queue.data().rows[0].status, QueueStatus::Done);
}

#[test]
fn merge_backfills_empty_source_key_on_legacy_rows() {
    let mut queue = service(vec![row_with_ticket("Ayşe", "1", QueueStatus::Waiting)]);
    let report = queue.merge_import(&[import("1", "Ayşe", "", "1")]);
    assert_eq!(report.updated, 1);
    assert_eq!(queue.data().rows[0].source_key, "1");

    // Name-based fallback when the sheet has no ticket column.
    let mut by_name = service(vec![row_with_ticket("Mehmet", "", QueueStatus::Waiting)]);
    let report = by_name.merge_import(&[ImportRow {
        source_key: "mehmet".into(),
        name: "Mehmet".into(),
        note: "kontrol".into(),
        ticket: String::new(),
        status: None,
    }]);
    assert_eq!(report.updated, 1);
    assert_eq!(by_name.data().rows[0].source_key, "mehmet");
    assert_eq!(by_name.data().rows[0].note, "kontrol");
    assert_eq!(
        by_name.data().rows[0].ticket,
        "",
        "merge never rewrites a matched row's ticket"
    );
}

#[test]
fn source_key_prefers_ticket_then_lowercased_name() {
    assert_eq!(QueueService::source_key(" 007 ", "Ignored"), "007");
    assert_eq!(
        QueueService::source_key("", "  Ayşe Yılmaz "),
        "ayşe yılmaz"
    );
    assert_eq!(QueueService::source_key("   ", ""), "");
}

#[test]
fn snapshot_counts_and_upcoming_limit() {
    let mut rows = vec![
        row("current", QueueStatus::Called),
        row("done", QueueStatus::Done),
        row("skipped", QueueStatus::Skipped),
    ];
    for index in 0..7 {
        rows.push(row(&format!("waiting {index}"), QueueStatus::Waiting));
    }
    let queue = service(rows);
    let snapshot = queue.snapshot();
    assert_eq!(
        snapshot.current.map(|r| r.name),
        Some("current".to_string())
    );
    assert_eq!(snapshot.upcoming.len(), 5);
    assert_eq!(snapshot.waiting, 7);
    assert_eq!(snapshot.called, 1);
    assert_eq!(snapshot.done, 1);
    assert_eq!(snapshot.skipped, 1);
    assert_eq!(
        names(&snapshot.upcoming),
        (0..5).map(|i| format!("waiting {i}")).collect::<Vec<_>>()
    );
}
