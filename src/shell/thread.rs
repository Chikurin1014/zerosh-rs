use anyhow::Result;
use nix::{
    libc::{self, c_int},
    sys::signal::Signal,
    unistd::{self, Pid},
};
use signal_hook::iterator::Signals;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::mpsc,
};

pub enum WorkerMsg {
    SignalMsg(i32), // Signal input
    Cmd(String),    // Command input
}

pub enum ShellMsg {
    Continue(i32), // Continue shell interaction. (i32) is the exit code
    Quit(i32),     // Quit shell. (i32) is the exit code
}

pub fn spawn_sig_handler(tx: mpsc::SyncSender<WorkerMsg>) -> Result<()> {
    let mut signals = Signals::new(&[
        Signal::SIGINT as c_int,
        Signal::SIGTSTP as c_int,
        Signal::SIGCHLD as c_int,
    ])?;
    std::thread::spawn(move || {
        for sig in signals.forever() {
            tx.send(WorkerMsg::SignalMsg(sig)).unwrap();
        }
    });
    Ok(())
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ProcState {
    Run,
    Stop,
}

#[derive(Debug)]
struct ProcInfo {
    state: ProcState,
    pg_id: Pid,
}

#[derive(Debug)]
pub struct Worker {
    exit_code: i32,                                     // Exit code
    fg: Option<Pid>,                                    // Foreground process
    jobs: BTreeMap<usize, (Pid, String)>, // Map of job id to (process group id, command)
    pg_id_to_pids: HashMap<Pid, (usize, HashSet<Pid>)>, // Map of process group id to (job id, set of process ids)
    pid_to_info: HashMap<Pid, ProcInfo>,                // Map of process id to process info
    shell_pg_id: Pid,                                   // Shell process group id
}

impl Worker {
    pub fn new() -> Self {
        Worker {
            exit_code: 0,
            fg: None,
            jobs: BTreeMap::new(),
            pg_id_to_pids: HashMap::new(),
            pid_to_info: HashMap::new(),
            shell_pg_id: unistd::tcgetpgrp(libc::STDIN_FILENO).unwrap(),
        }
    }
    pub fn spawn(
        mut self,
        worker_rx: mpsc::Receiver<WorkerMsg>,
        shell_tx: mpsc::SyncSender<ShellMsg>,
    ) -> Result<()> {
        std::thread::spawn(move || {
            for msg in worker_rx.iter() {
                match msg {
                    WorkerMsg::Cmd(line) => {
                        todo!()
                    }
                    WorkerMsg::SignalMsg(sig) => {
                        todo!()
                    }
                }
            }
        });
        Ok(())
    }
}
