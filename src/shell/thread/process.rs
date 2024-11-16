use anyhow::Result;
use nix::unistd;
use std::collections::HashSet;

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
    pub pgid: unistd::Pid,
    pub pids: HashSet<unistd::Pid>,
    pub job_id: usize,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub job_id: usize,
    pub pgid: unistd::Pid,
    pub command: String,
}

impl Job {
    fn get_process_group(&self) -> ProcessGroup {
        ProcessGroup {
            pgid: self.pgid,
            pids: HashSet::new(),
            job_id: self.job_id,
        }
    }
}
