use nix::unistd;
use std::collections::HashSet;

use super::job::JobId;

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ProcessState {
    Run,
    Stop,
}

#[derive(Debug, Clone)]
pub struct Process {
    pub pgid: unistd::Pid,
    pub state: ProcessState,
}

#[derive(Debug, Clone)]
pub struct ProcessGroup {
    pub pids: HashSet<unistd::Pid>,
    pub job_id: JobId,
}
