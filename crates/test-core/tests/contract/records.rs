// SPDX-License-Identifier: AGPL-3.0-or-later
//! Battery memory over the storage in memory: a record written by one
//! machine is read by the next, it is written at `finish` and after ten
//! virtual seconds, and a failure planted in storage surfaces from `run` as
//! the contract says.

use crate::support::{create_on, run_to};
use unemu_core_api::test_support::{MemoryStorage, Operation};
use unemu_core_api::{
    Content, Error, ErrorKind, MediaAction, NewMedium, ResetKind, RunLimit, SlotId, Status, Ticks,
    WriteTarget,
};
use unemu_test_core::BATTERY_RECORD;

/// 1 000 ticks a second, so ten virtual seconds are 10 000 ticks.
const BATTERY: &str = "unemu-test-core 1\nclock 1000\nresets hard\nbattery\n";

fn starts(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("the record is text")
}

#[test]
fn a_record_written_by_one_machine_is_read_by_the_next() {
    let storage = MemoryStorage::new();
    for start in 1..=3u64 {
        let mut machine = create_on(BATTERY, &storage).expect("create");
        assert!(
            storage.is_open(BATTERY_RECORD),
            "the machine holds its record"
        );
        run_to(machine.as_mut(), 500);
        machine.finish().expect("finish");
        assert!(!storage.is_open(BATTERY_RECORD), "finish releases it");
        drop(machine);
        storage.power_cut();
        assert_eq!(
            starts(&storage.durable(BATTERY_RECORD).expect("written")),
            format!("unemu-test-core battery 1\nstarts {start}\n")
        );
    }
    // A hard reset is a start too.
    let mut machine = create_on(BATTERY, &storage).expect("create");
    machine.reset(ResetKind::Hard).expect("hard reset");
    machine.finish().expect("finish");
    assert!(starts(&storage.durable(BATTERY_RECORD).unwrap()).ends_with("starts 5\n"));
}

#[test]
fn a_machine_without_battery_opens_no_record() {
    let storage = MemoryStorage::new();
    let mut machine = create_on("unemu-test-core 1\n", &storage).expect("create");
    run_to(machine.as_mut(), 20_000);
    machine.finish().expect("finish");
    assert!(storage.names().is_empty());
}

#[test]
fn changed_memory_is_written_after_ten_virtual_seconds() {
    let storage = MemoryStorage::new();
    let mut machine = create_on(BATTERY, &storage).expect("create");
    run_to(machine.as_mut(), 9_999);
    assert_eq!(storage.durable(BATTERY_RECORD), Some(Vec::new()), "not yet");
    run_to(machine.as_mut(), 10_000);
    assert!(starts(&storage.durable(BATTERY_RECORD).unwrap()).ends_with("starts 1\n"));
    // Unchanged memory is not written again; changed memory waits ten more
    // seconds from the last write.
    machine.reset(ResetKind::Hard).expect("a second start");
    run_to(machine.as_mut(), 19_999);
    assert!(starts(&storage.durable(BATTERY_RECORD).unwrap()).ends_with("starts 1\n"));
    run_to(machine.as_mut(), 20_000);
    assert!(starts(&storage.durable(BATTERY_RECORD).unwrap()).ends_with("starts 2\n"));
    // Dropped without finish, what the last write made durable stays.
    machine.reset(ResetKind::Hard).expect("a third start");
    drop(machine);
    storage.power_cut();
    assert!(starts(&storage.durable(BATTERY_RECORD).unwrap()).ends_with("starts 2\n"));
}

#[test]
fn a_record_held_elsewhere_or_foreign_refuses_the_machine() {
    let storage = MemoryStorage::new();
    let first = create_on(BATTERY, &storage).expect("create");
    let held = create_on(BATTERY, &storage)
        .err()
        .expect("the record is held");
    assert_eq!(held.kind, ErrorKind::Usage);
    drop(first);
    storage.insert(BATTERY_RECORD, b"not ours");
    let foreign = create_on(BATTERY, &storage).err().expect("foreign bytes");
    assert_eq!(foreign.kind, ErrorKind::Storage);
}

#[test]
fn a_failed_battery_write_fails_the_run() {
    for (kind, uncertain) in [
        (ErrorKind::Storage, None),
        (ErrorKind::StorageUncertain, Some(false)),
        (ErrorKind::StorageUncertain, Some(true)),
    ] {
        let storage = MemoryStorage::new();
        let mut machine = create_on(BATTERY, &storage).expect("create");
        match uncertain {
            None => storage.fail_next(
                BATTERY_RECORD,
                Operation::Commit,
                Error::new(ErrorKind::Storage, "the disk is full"),
            ),
            Some(takes_effect) => storage.uncertain_commit(BATTERY_RECORD, takes_effect),
        }
        let error = machine
            .run(RunLimit::until(Ticks(10_000)))
            .expect_err("the write at ten seconds fails");
        assert_eq!(error.kind, kind);
        assert_eq!(machine.status(), Status::Failed);
        assert_eq!(
            machine
                .run(RunLimit::until(Ticks(20_000)))
                .unwrap_err()
                .kind,
            ErrorKind::Usage
        );
        assert_eq!(machine.finish().unwrap_err().kind, ErrorKind::Usage);
        // The last picture stays readable.
        machine
            .frame(unemu_core_api::DisplayId(0))
            .expect("the picture");
        drop(machine);
        storage.power_cut();
        let durable = storage.durable(BATTERY_RECORD).unwrap();
        // The record holds the old contents or the new, whole.
        assert!(
            durable.is_empty() || starts(&durable).ends_with("starts 1\n"),
            "{durable:?}"
        );
    }
}

#[test]
fn a_failed_write_at_finish_fails_finish() {
    let storage = MemoryStorage::new();
    let mut machine = create_on(BATTERY, &storage).expect("create");
    storage.fail_next(
        BATTERY_RECORD,
        Operation::Replace,
        Error::new(ErrorKind::Storage, "no space"),
    );
    assert_eq!(machine.finish().unwrap_err().kind, ErrorKind::Storage);
    assert_eq!(machine.status(), Status::Failed);
}

const SLOT: &str = "unemu-test-core 1\nslot 0 floppy removable utc\nmedia-changes\n";

fn kept(machine: &mut dyn unemu_core_api::Machine, record: &str) -> Result<usize, Error> {
    machine.add_medium(
        SlotId(0),
        NewMedium::new(
            "disk",
            Content::Bytes(vec![0; 8]),
            [3; 32],
            8,
            "utc",
            WriteTarget::Kept { record },
        ),
    )
}

#[test]
fn a_kept_record_that_cannot_be_opened_fails_the_next_run() {
    let storage = MemoryStorage::new();
    let mut machine = create_on(SLOT, &storage).expect("create");
    let index = kept(machine.as_mut(), "kept-a").expect("a kept medium");
    storage.fail_next(
        "kept-a",
        Operation::Open,
        Error::new(ErrorKind::Storage, "cannot open"),
    );
    machine
        .change_medium(SlotId(0), MediaAction::Insert(index))
        .expect("queued");
    assert!(
        !storage.is_open("kept-a"),
        "opened at the boundary, not before"
    );
    let error = machine
        .run(RunLimit::until(Ticks(10)))
        .expect_err("the record could not be opened");
    assert_eq!(error.kind, ErrorKind::Storage);
    assert_eq!(machine.status(), Status::Failed);
}

#[test]
fn a_kept_record_is_held_while_inserted_and_made_durable_at_finish() {
    let storage = MemoryStorage::new();
    let mut machine = create_on(SLOT, &storage).expect("create");
    assert_eq!(
        kept(machine.as_mut(), "battery").unwrap_err().kind,
        ErrorKind::Usage,
        "a kept record's name is the front end's, under kept-"
    );
    let index = kept(machine.as_mut(), "kept-a").expect("a kept medium");
    machine
        .change_medium(SlotId(0), MediaAction::Insert(index))
        .expect("queued");
    run_to(machine.as_mut(), 10);
    assert!(storage.is_open("kept-a"));
    let states = machine.slot_states();
    assert_eq!(states[0].inserted, Some(index));
    assert!(states[0].active, "the change was activity");
    assert!(
        !machine.slot_states()[0].active,
        "reading takes the activity"
    );
    storage.fail_next(
        "kept-a",
        Operation::Sync,
        Error::new(ErrorKind::StorageUncertain, "unknown"),
    );
    assert_eq!(
        machine.finish().unwrap_err().kind,
        ErrorKind::StorageUncertain
    );
    let mut machine = create_on(SLOT, &MemoryStorage::new()).expect("create");
    let index = kept(machine.as_mut(), "kept-b").expect("a kept medium");
    machine
        .change_medium(SlotId(0), MediaAction::Insert(index))
        .unwrap();
    run_to(machine.as_mut(), 10);
    machine
        .change_medium(SlotId(0), MediaAction::Eject)
        .unwrap();
    run_to(machine.as_mut(), 20);
    assert_eq!(machine.slot_states()[0].inserted, None);
    machine.finish().expect("finish");
}

#[test]
fn a_halted_machine_finishes_and_writes_its_memory() {
    let storage = MemoryStorage::new();
    let mut machine = create_on(&format!("{BATTERY}at 500 halt\n"), &storage).expect("create");
    assert_eq!(
        run_to(machine.as_mut(), 1_000),
        unemu_core_api::Stop::Halted
    );
    assert_eq!(machine.status(), Status::Halted);
    assert_eq!(storage.durable(BATTERY_RECORD), Some(Vec::new()), "not yet");
    machine.finish().expect("a halted machine finishes");
    assert!(starts(&storage.durable(BATTERY_RECORD).unwrap()).ends_with("starts 1\n"));
    assert_eq!(
        machine.run(RunLimit::until(Ticks(2_000))).unwrap_err().kind,
        ErrorKind::Usage,
        "a finished session does not run"
    );
}
