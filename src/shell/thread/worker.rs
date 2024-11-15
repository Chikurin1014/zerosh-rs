use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::mpsc,
};

use anyhow::Result;
use nix::{
    libc,
    sys::signal::{self, Signal},
    unistd::{self, Pid},
};

use crate::{
    helper::Defer,
    shell::{
        syscall,
        thread::{ProcInfo, ProcState},
        ShellMsg,
    },
};

pub enum WorkerMsg {
    SignalMsg(i32), // Signal input
    Cmd(String),    // Command input
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
                    WorkerMsg::Cmd(line) => match parse_cmd(&line) {
                        Ok(cmd) => {
                            if self.built_in_cmd(&cmd, &shell_tx) {
                                continue;
                            }
                            if self.spawn_child(&line, &cmd).is_err() {
                                shell_tx.send(ShellMsg::Continue(self.exit_code)).unwrap();
                            }
                        }
                        Err(e) => {
                            eprintln!("ZeroSh: {}", e);
                            shell_tx.send(ShellMsg::Continue(self.exit_code)).unwrap();
                        }
                    },
                    WorkerMsg::SignalMsg(sig) => {
                        self.wait_child(&shell_tx);
                    }
                }
            }
        });
        Ok(())
    }

    fn run_exit(&mut self, arg: &Vec<&str>, shell_tx: &mpsc::SyncSender<ShellMsg>) -> bool {
        if !self.jobs.is_empty() {
            eprintln!("ZeroSh: There are stopped jobs.");
            self.exit_code = 1;
            shell_tx.send(ShellMsg::Continue(self.exit_code)).unwrap();
            return true;
        }

        let exit_code = if let Some(s) = arg.get(1) {
            if let Ok(n) = (*s).parse::<i32>() {
                n
            } else {
                eprintln!("ZeroSh: Invalid argument: {}", s);
                self.exit_code = 1;
                shell_tx.send(ShellMsg::Continue(self.exit_code)).unwrap();
                return true;
            }
        } else {
            self.exit_code
        };

        shell_tx.send(ShellMsg::Quit(exit_code)).unwrap();
        true
    }

    fn run_cd(&mut self, arg: &Vec<&str>, shell_tx: &mpsc::SyncSender<ShellMsg>) -> bool {
        if let Some(dir) = arg.get(1) {
            std::env::set_current_dir(dir).unwrap();
        }
        shell_tx.send(ShellMsg::Continue(self.exit_code)).unwrap();
        true
    }

    fn run_jobs(&mut self, shell_tx: &mpsc::SyncSender<ShellMsg>) -> bool {
        for (n, (pg_id, cmd)) in &self.jobs {
            println!("[{}] {} {}", n, pg_id, cmd);
        }
        shell_tx.send(ShellMsg::Continue(self.exit_code)).unwrap();
        true
    }

    fn run_fg(&mut self, arg: &Vec<&str>, shell_tx: &mpsc::SyncSender<ShellMsg>) -> bool {
        self.exit_code = 1;
        if arg.len() < 2 {
            eprintln!("ZeroSh: fg: Missing argument");
            shell_tx.send(ShellMsg::Continue(self.exit_code)).unwrap();
            return true;
        }
        if let Ok(n) = arg[1].parse::<usize>() {
            if let Some((pg_id, cmd)) = self.jobs.get(&n) {
                eprintln!("ZeroSh: [{}] continued {}", n, cmd);
                self.fg = Some(*pg_id);
                unistd::tcsetpgrp(libc::STDIN_FILENO, *pg_id).unwrap();
                signal::killpg(*pg_id, Signal::SIGCONT).unwrap();
                return true;
            }
        }
        eprintln!("ZeroSh: fg: Job not found: {}", arg[1]);
        shell_tx.send(ShellMsg::Continue(self.exit_code)).unwrap();
        true
    }

    fn built_in_cmd(&mut self, cmd: &Cmd, shell_tx: &mpsc::SyncSender<ShellMsg>) -> bool {
        if cmd.len() != 1 {
            return false;
        }
        match cmd[0].0 {
            "exit" => self.run_exit(&cmd[0].1, shell_tx),
            "cd" => self.run_cd(&cmd[0].1, shell_tx),
            "jobs" => self.run_jobs(shell_tx),
            "fg" => self.run_fg(&cmd[0].1, shell_tx),
            _ => false,
        }
    }

    fn spawn_child(&mut self, line: &str, cmd: &Cmd) -> Result<()> {
        assert_ne!(cmd.len(), 0);
        let job_id = if let Some(id) = self.get_next_job_id() {
            id
        } else {
            return Err(anyhow::anyhow!("ZeroSh: Too many jobs"));
        };
        if cmd.len() > 2 {
            return Err(anyhow::anyhow!(
                "ZeroSh: Pipeline doesn't support more than 2 commands"
            ));
        }
        let (input, output) = if cmd.len() == 2 {
            let p = unistd::pipe()?;
            (Some(p.0), Some(p.1))
        } else {
            (None, None)
        };

        let clean_up_pipe = Defer {
            f: || {
                if let Some(fd) = input {
                    syscall(|| unistd::close(fd)).unwrap();
                }
                if let Some(fd) = output {
                    syscall(|| unistd::close(fd)).unwrap();
                }
            },
        };

        let pg_id = match fork_exec(Pid::from_raw(0), cmd[0].0, &cmd[0].1, None, output) {
            Ok(child) => child,
            Err(e) => {
                return Err(e);
            }
        };

        let info = ProcInfo {
            state: ProcState::Run,
            pg_id,
        };
        let mut pids = HashMap::new();
        pids.insert(pg_id, info.clone());
        if cmd.len() == 2 {
            match fork_exec(pg_id, cmd[1].0, &cmd[1].1, input, None) {
                Ok(child) => {
                    pids.insert(child, info);
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }
        std::mem::drop(clean_up_pipe);
        self.fg = Some(pg_id);
        self.insert_job(job_id, pg_id, line.to_string());
        unistd::tcsetpgrp(libc::STDIN_FILENO, pg_id)?;
        Ok(())
    }

    fn wait_child(&mut self, shell_tx: &mpsc::SyncSender<ShellMsg>) {
        todo!()
    }

    fn get_next_job_id(&self) -> Option<usize> {
        (1..).find(|n| !self.jobs.contains_key(&n))
    }

    fn insert_job(&mut self, job_id: usize, pg_id: Pid, cmd: String) {
        self.jobs.insert(job_id, (pg_id, cmd));
        self.pg_id_to_pids.insert(pg_id, (job_id, HashSet::new()));
    }
}

fn fork_exec(
    pg_id: Pid,
    cmd: &str,
    args: &[&str],
    input: Option<i32>,
    output: Option<i32>,
) -> Result<Pid> {
    todo!()
}

type Cmd<'a> = Vec<(&'a str, Vec<&'a str>)>;

fn parse_cmd(line: &str) -> Result<Cmd> {
    let mut cmds = Vec::new();
    for part in line.split('|') {
        let mut cmd = part.trim().split_whitespace().collect::<Vec<_>>();
        if !cmd.is_empty() {
            let cmd_name = cmd.remove(0);
            cmds.push((cmd_name, cmd));
        }
    }
    Ok(cmds)
}
