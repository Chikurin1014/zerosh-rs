use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::mpsc,
};

use anyhow::Result;
use nix::{
    libc,
    sys::{
        signal::{self, Signal},
        wait::{self, wait, WaitPidFlag},
    },
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
        self.insert_job(job_id, pg_id, pids, line);
        unistd::tcsetpgrp(libc::STDIN_FILENO, pg_id)?;
        Ok(())
    }

    fn wait_child(&mut self, shell_tx: &mpsc::SyncSender<ShellMsg>) {
        // WUNTRACED: Return if a child has stopped
        // WNOHANG: Return immediately if no child has exited
        // WCONTINUED: Return if a stopped child has been continued
        let flag = Some(WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG | WaitPidFlag::WCONTINUED);

        loop {
            match syscall(|| wait::waitpid(Pid::from_raw(-1), flag)) {
                Ok(wait::WaitStatus::Exited(pid, status)) => {
                    self.exit_code = status;
                    self.process_term(pid, shell_tx);
                }
                Ok(wait::WaitStatus::Signaled(pid, signal, core)) => {
                    eprintln!("ZeroSh: Process {} terminated by signal {}", pid, signal);
                    self.exit_code = 128 + signal as i32;
                    self.process_term(pid, shell_tx);
                }
                Ok(wait::WaitStatus::Stopped(pid, _signal)) => self.process_stop(pid, shell_tx),
                Ok(wait::WaitStatus::Continued(pid)) => self.process_continue(pid),
                Ok(wait::WaitStatus::StillAlive) => return,
                Err(nix::Error::ECHILD) => return,
                Err(e) => {
                    eprintln!("ZeroSh: waitpid: {}", e);
                    std::process::exit(1);
                }
                #[cfg(any(target_os = "linux", target_os = "android"))]
                Ok(wait::WaitStatus::PtraceEvent(pid, _, _)) => self.process_stop(pid, shell_tx),
                #[cfg(any(target_os = "linux", target_os = "android"))]
                Ok(wait::WaitStatus::PtraceSyscall(pid)) => self.process_continue(pid),
            }
        }
    }

    fn process_term(&mut self, pid: Pid, shell_tx: &mpsc::SyncSender<ShellMsg>) {
        if let Some((job_id, pg_id)) = self.remove_pid(pid) {
            self.manage_job(job_id, pg_id, shell_tx);
        }
    }

    fn process_stop(&mut self, pid: Pid, shell_tx: &mpsc::SyncSender<ShellMsg>) {
        self.set_pid_state(pid, ProcState::Stop);
        let pg_id = self.pid_to_info.get(&pid).unwrap().pg_id;
        let job_id = self.pg_id_to_pids.get(&pg_id).unwrap().0;
        self.manage_job(job_id, pg_id, shell_tx);
    }

    fn process_continue(&mut self, pid: Pid) {
        self.set_pid_state(pid, ProcState::Run);
    }

    fn set_pid_state(&mut self, pid: Pid, state: ProcState) -> Option<ProcState> {
        let info = self.pid_to_info.get_mut(&pid)?;
        let old_state = std::mem::replace(&mut info.state, state);
        Some(old_state)
    }

    fn remove_pid(&mut self, pid: Pid) -> Option<(usize, Pid)> {
        let pg_id = self.pid_to_info.get(&pid)?.pg_id;
        let it = self.pg_id_to_pids.get_mut(&pg_id)?;
        it.1.remove(&pid);
        let job_id = it.0;
        Some((job_id, pg_id))
    }

    fn get_next_job_id(&self) -> Option<usize> {
        (1..).find(|n| !self.jobs.contains_key(&n))
    }

    fn manage_job(&mut self, job_id: usize, pg_id: Pid, shell_tx: &mpsc::SyncSender<ShellMsg>) {
        let is_fg = self.fg.map_or(false, |fg| fg == pg_id);
        let line = &self.jobs.get(&job_id).unwrap().1;
        if is_fg {
            if self.is_group_empty(pg_id) {
                eprintln!("ZeroSh: [{}] Done {}", job_id, line);
                self.remove_job(job_id);
                self.set_shell_fg(shell_tx);
            } else if self.is_group_stop(pg_id).unwrap() {
                eprintln!("ZeroSh: [{}] Stopped {}", job_id, line);
                self.set_shell_fg(shell_tx);
            }
        } else if self.is_group_empty(pg_id) {
            eprintln!("ZeroSh: [{}] Done {}", job_id, line);
            self.remove_job(job_id);
        }
    }

    fn is_group_empty(&self, pg_id: Pid) -> bool {
        self.pg_id_to_pids.get(&pg_id).unwrap().1.is_empty()
    }

    fn is_group_stop(&self, pg_id: Pid) -> Option<bool> {
        Some(
            self.pg_id_to_pids
                .get(&pg_id)?
                .1
                .iter()
                .all(|pid| self.pid_to_info.get(pid).unwrap().state != ProcState::Run),
        )
    }

    fn insert_job(&mut self, job_id: usize, pg_id: Pid, pids: HashMap<Pid, ProcInfo>, line: &str) {
        assert!(!self.jobs.contains_key(&job_id));
        self.jobs.insert(job_id, (pg_id, line.to_string()));
        let mut procs = HashSet::new();
        for (pid, info) in pids {
            procs.insert(pid);
            assert!(!self.pid_to_info.contains_key(&pid));
            self.pid_to_info.insert(pid, info);
        }
        assert!(!self.pg_id_to_pids.contains_key(&pg_id));
        self.pg_id_to_pids.insert(pg_id, (job_id, procs));
    }

    fn remove_job(&mut self, job_id: usize) {
        if let Some((pg_id, _)) = self.jobs.remove(&job_id) {
            if let Some((_, pids)) = self.pg_id_to_pids.remove(&pg_id) {
                assert!(pids.is_empty());
            }
        }
    }

    fn set_shell_fg(&mut self, shell_tx: &mpsc::SyncSender<ShellMsg>) {
        self.fg = None;
        unistd::tcsetpgrp(libc::STDIN_FILENO, self.shell_pg_id).unwrap();
        shell_tx.send(ShellMsg::Continue(self.exit_code)).unwrap();
    }
}

fn fork_exec(
    pg_id: Pid,
    filename: &str,
    args: &[&str],
    input: Option<i32>,
    output: Option<i32>,
) -> Result<Pid> {
    let filename = std::ffi::CString::new(filename)?;
    let args = args
        .iter()
        .map(|s| std::ffi::CString::new(*s).unwrap())
        .collect::<Vec<_>>();

    match syscall(|| unsafe { unistd::fork() })? {
        unistd::ForkResult::Parent { child } => {
            unistd::setpgid(child, pg_id)?;
            Ok(child)
        }
        unistd::ForkResult::Child => {
            unistd::setpgid(Pid::from_raw(0), pg_id)?;

            if let Some(stdin_fd) = input {
                syscall(|| unistd::dup2(stdin_fd, libc::STDIN_FILENO))?;
            }
            if let Some(stdout_fd) = output {
                syscall(|| unistd::dup2(stdout_fd, libc::STDOUT_FILENO))?;
            }
            for i in 3..=6 {
                let _ = syscall(|| unistd::close(i));
            }
            match unistd::execvp(&filename, &args) {
                Err(_) => {
                    unistd::write(libc::STDERR_FILENO, b"ZeroSh: Unknown command\n")?;
                    std::process::exit(1);
                }
                Ok(_) => unreachable!(),
            }
        }
    }
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
