//! What the programs of one terminal told of themselves with OSC 7501:
//! a record for each id, and one for the program itself.

use std::collections::HashMap;

pub use benterm_core::ProgramRecord;
use benterm_core::{ProgramState, ProgramStatus};

/// The most records one terminal keeps. A program cannot make the
/// terminal keep more, however many ids it reports under.
const RECORD_LIMIT: usize = 256;

#[derive(Default)]
pub(crate) struct ProgramRecords {
    /// By id; the id of the program itself is the empty one.
    records: HashMap<String, ProgramRecord>,
    serial: u64,
    /// How many times the program itself began to work: told that it
    /// is working when its record did not say so.
    work: u64,
}

impl ProgramRecords {
    /// Takes a report: it replaces all its record held. Past the limit,
    /// the record that was told least recently makes room.
    pub(crate) fn report(&mut self, status: ProgramStatus) {
        self.serial += 1;
        let id = status.id.clone().unwrap_or_default();
        if self.records.len() >= RECORD_LIMIT && !self.records.contains_key(&id) {
            let oldest = self
                .records
                .iter()
                .min_by_key(|(_, record)| record.serial)
                .map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                self.records.remove(&oldest);
            }
        }
        // Working told again while it works, as an agent does whose
        // turn is over while a command it left behind runs, is the same
        // work.
        let working = |record: &ProgramRecord| record.status.state == ProgramState::Working;
        if id.is_empty()
            && status.state == ProgramState::Working
            && !self.root().is_some_and(working)
        {
            self.work += 1;
        }
        let record = ProgramRecord {
            status,
            serial: self.serial,
            work: self.work,
        };
        self.records.insert(id, record);
    }

    /// Removes the record `id` and every record beneath it, or every
    /// record for `None`.
    pub(crate) fn clear(&mut self, id: Option<&str>) {
        match id {
            None => self.records.clear(),
            Some(id) => self.records.retain(|key, _| {
                let beneath = key
                    .strip_prefix(id)
                    .is_some_and(|rest| rest.starts_with('/'));
                key != id && !beneath
            }),
        }
    }

    /// The program ended or a prompt started: what was going on is over,
    /// and what was reached stays to be found.
    pub(crate) fn end_run(&mut self) {
        self.records.retain(|_, record| {
            !matches!(
                record.status.state,
                ProgramState::Working | ProgramState::Blocked
            )
        });
    }

    /// What the program itself told last.
    pub(crate) fn root(&self) -> Option<&ProgramRecord> {
        self.records.get("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: ProgramState, id: Option<&str>) -> ProgramStatus {
        ProgramStatus {
            state,
            id: id.map(str::to_owned),
            kind: None,
            progress: None,
            app: None,
            title: None,
            msg: None,
        }
    }

    fn ids(records: &ProgramRecords) -> Vec<&str> {
        let mut ids: Vec<&str> = records.records.keys().map(String::as_str).collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn clear_removes_a_record_and_those_beneath_it_only() {
        let mut records = ProgramRecords::default();
        for id in [None, Some("build"), Some("build/test"), Some("builder")] {
            records.report(status(ProgramState::Working, id));
        }
        records.clear(Some("build"));
        assert_eq!(ids(&records), ["", "builder"]);
        records.clear(Some("missing"));
        assert_eq!(ids(&records), ["", "builder"]);
        records.clear(None);
        assert!(ids(&records).is_empty());
    }

    #[test]
    fn a_report_replaces_all_its_record_held() {
        let mut records = ProgramRecords::default();
        records.report(ProgramStatus {
            app: Some("cargo".into()),
            ..status(ProgramState::Working, None)
        });
        records.report(status(ProgramState::Done, None));
        let root = records.root().unwrap();
        assert_eq!(root.status, status(ProgramState::Done, None));
        assert_eq!(root.serial, 2);
    }

    #[test]
    fn a_record_tells_how_many_times_the_program_itself_began_to_work() {
        let mut records = ProgramRecords::default();
        records.report(status(ProgramState::Working, None));
        records.report(status(ProgramState::Working, Some("part")));
        records.report(status(ProgramState::Done, None));
        assert_eq!(records.root().unwrap().work, 1);
        records.clear(None);
        records.report(status(ProgramState::Working, None));
        records.report(status(ProgramState::Blocked, None));
        assert_eq!(records.root().unwrap().work, 2);
    }

    /// Claude Code tells `working` again and again while a command it
    /// left running behind is not over.
    #[test]
    fn working_told_again_while_working_is_no_new_work() {
        let mut records = ProgramRecords::default();
        records.report(status(ProgramState::Working, None));
        records.report(status(ProgramState::Working, Some("part")));
        records.report(status(ProgramState::Working, None));
        records.clear(Some("part"));
        records.report(status(ProgramState::Working, None));
        let root = records.root().unwrap();
        assert_eq!((root.serial, root.work), (4, 1));
        // After anything else it told, and after its record went.
        records.report(status(ProgramState::Blocked, None));
        records.report(status(ProgramState::Working, None));
        assert_eq!(records.root().unwrap().work, 2);
        records.end_run();
        records.report(status(ProgramState::Working, None));
        assert_eq!(records.root().unwrap().work, 3);
        records.clear(None);
        records.report(status(ProgramState::Working, None));
        assert_eq!(records.root().unwrap().work, 4);
    }

    #[test]
    fn past_the_limit_the_record_told_least_recently_makes_room() {
        let mut records = ProgramRecords::default();
        records.report(status(ProgramState::Working, Some("first")));
        records.report(status(ProgramState::Idle, None));
        for n in 2..RECORD_LIMIT {
            records.report(status(ProgramState::Working, Some(&format!("n{n}"))));
        }
        // Told again, "first" is no longer the oldest.
        records.report(status(ProgramState::Working, Some("first")));
        assert_eq!(records.records.len(), RECORD_LIMIT);
        records.report(status(ProgramState::Working, Some("one-more")));
        assert_eq!(records.records.len(), RECORD_LIMIT);
        assert!(records.root().is_none());
        assert!(records.records.contains_key("first"));
        assert!(records.records.contains_key("one-more"));
    }

    #[test]
    fn the_end_of_a_run_drops_every_working_and_blocked_record() {
        let mut records = ProgramRecords::default();
        records.report(status(ProgramState::Done, None));
        records.report(status(ProgramState::Blocked, Some("a")));
        records.report(status(ProgramState::Working, Some("b")));
        records.report(status(ProgramState::Error, Some("c")));
        records.end_run();
        assert_eq!(ids(&records), ["", "c"]);
    }
}
