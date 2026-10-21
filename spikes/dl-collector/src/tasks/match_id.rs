use super::{at, rules_object};
use crate::keys::MATCH_INFO;
use crate::mem::MemRead;
use crate::runner::{Pass, Task, TaskError};

pub struct MatchId {
    rules_ptr: u64,
    field: u32,
}

impl MatchId {
    pub fn new(rules_ptr: u64, field: u32) -> MatchId {
        MatchId { rules_ptr, field }
    }
}

impl Task for MatchId {
    fn name(&self) -> &'static str {
        "match_id"
    }

    fn run(&mut self, m: &dyn MemRead, out: &mut Pass) -> Result<(), TaskError> {
        let id = match rules_object(m, self.rules_ptr)? {
            None => None,
            Some(rules) => match m.read_u32(at(rules, self.field.into())?)? {
                0 => None,
                id => Some(id.to_string()),
            },
        };
        out.set(MATCH_INFO, "match_id", id);
        Ok(())
    }
}
