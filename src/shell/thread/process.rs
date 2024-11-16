use anyhow::{Context as _, Result};
use nix::{libc, unistd};
use std::{
    collections::{HashMap, HashSet},
    sync::mpsc,
};

use crate::shell::ShellMsg;

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ProcessState {
    Run,
    Stop,
}

#[derive(Debug, Clone)]
pub struct Process {
    pub pgid: unistd::Pid, // TODO: Remove this field
    pub state: ProcessState,
}

#[derive(Debug, Clone)]
pub struct ProcessGroup {
    pub pids: HashSet<unistd::Pid>, // Map of process id to process info
}

#[derive(Debug)]
struct Procs {
    processes: HashMap<unistd::Pid, Process>, // Map of process id to process info
    groups: HashMap<unistd::Pid, ProcessGroup>, // Map of job id to job info
}

impl Procs {
    fn new() -> Self {
        Self {
            processes: HashMap::new(),
            groups: HashMap::new(),
        }
    }

    fn manage_pg(&mut self, pgid: unistd::Pid, is_foreground: bool) -> Result<()> {
        let grp = self
            .groups
            .get_mut(&pgid)
            .context("Process group not found")?;
        let is_grp_empty = grp.pids.is_empty();
        let is_grp_stop = self
            .processes
            .values()
            .filter(|p| p.pgid == pgid)
            .all(|p| p.state == ProcessState::Stop);

        if is_grp_empty {
            self.remove_pg(pgid)?;
            return Ok(());
        }
        if is_foreground && is_grp_stop {
            return Ok(());
        }

        Ok(())
    }

    fn add_pg(&mut self, pgid: unistd::Pid, group: ProcessGroup) {
        self.groups.insert(pgid, group.clone());
        for pid in group.pids {
            self.processes.insert(
                pid,
                Process {
                    pgid,
                    state: ProcessState::Run,
                },
            );
        }
    }

    fn remove_pg(&mut self, pgid: unistd::Pid) -> Result<()> {
        let grp = self
            .groups
            .remove(&pgid)
            .with_context(|| format!("Process group {} not found", pgid))?;
        for pid in grp.pids {
            self.processes
                .remove(&pid)
                .with_context(|| format!("Process {} not found", pid))?;
        }
        Ok(())
    }

    fn remove_process(&mut self, pid: unistd::Pid) -> Result<Process> {
        let process = self.processes.remove(&pid).context("Process not found")?;
        Ok(process)
    }

    fn set_process_state(&mut self, pid: unistd::Pid, state: ProcessState) -> Result<&Process> {
        let process = self.processes.get_mut(&pid).context("Process not found")?;
        process.state = state;
        Ok(process)
    }
}

#[derive(Debug)]
pub struct ProcessManager {
    procs: Procs,
    exit_code: i32,
    fg_pid: Option<unistd::Pid>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            procs: Procs::new(),
            exit_code: 0,
            fg_pid: None,
        }
    }

    pub fn terminate(
        &mut self,
        pid: unistd::Pid,
        shell_tx: &mpsc::SyncSender<ShellMsg>,
    ) -> Result<()> {
        let pgid = self.procs.remove_process(pid)?.pgid;
        let is_fg = self.fg_pid == Some(pgid);
        self.procs.manage_pg(pgid, is_fg)?;
        if is_fg {
            self.set_shell_fg(shell_tx)?;
        }
        Ok(())
    }

    pub fn resume(&mut self, pid: unistd::Pid) -> Result<()> {
        self.procs
            .set_process_state(pid, ProcessState::Run)
            .and(Ok(()))
    }

    pub fn stop(&mut self, pid: unistd::Pid, shell_tx: &mpsc::SyncSender<ShellMsg>) -> Result<()> {
        let pgid = self.procs.set_process_state(pid, ProcessState::Stop)?.pgid;
        let is_fg = self.fg_pid == Some(pgid);
        self.procs.manage_pg(pgid, is_fg)?;
        if is_fg {
            self.set_shell_fg(shell_tx)?;
        }
        Ok(())
    }

    pub fn add_group(&mut self, pgid: unistd::Pid, group: ProcessGroup) {
        self.procs.add_pg(pgid, group);
    }

    pub fn set_fg(&mut self, pgid: unistd::Pid) -> Result<()> {
        self.fg_pid = Some(pgid);
        unistd::tcsetpgrp(libc::STDIN_FILENO, pgid)?;
        Ok(())
    }

    fn set_shell_fg(&mut self, shell_tx: &mpsc::SyncSender<ShellMsg>) -> Result<()> {
        self.fg_pid = None;
        unistd::tcsetpgrp(libc::STDIN_FILENO, unistd::getpgrp())?;
        shell_tx.send(ShellMsg::Continue(self.exit_code))?;
        Ok(())
    }
}
