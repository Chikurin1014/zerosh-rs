use std::{
    collections::{HashMap, HashSet},
    sync::mpsc,
};

use anyhow::{Context as _, Result};
use nix::{
    libc::{self},
    sys::{
        signal::{self, Signal},
        wait::{self, WaitPidFlag},
    },
    unistd::{self, Pid},
};

use crate::shell::{
    parser::{parse_cmd, Cmd},
    syscall,
    thread::{
        job::{JobId, JobInfo},
        process::{ProcessGroup, ProcessManager},
    },
    ShellMsg,
};

pub enum WorkerMsg {
    SignalMsg(i32), // Signal input
    Cmd(String),    // Command input
}

#[derive(Debug)]
pub struct Worker {
    exit_code: i32,                  // Exit code
    process_manager: ProcessManager, // Process manager
    jobs: HashMap<JobId, JobInfo>,   // Map of job id to job info
    _shell_pgid: Pid,                // Shell process group id
}

impl Worker {
    pub fn new() -> Self {
        Worker {
            exit_code: 0,
            process_manager: ProcessManager::new(),
            jobs: HashMap::new(),
            _shell_pgid: unistd::tcgetpgrp(libc::STDIN_FILENO).unwrap(),
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
                    WorkerMsg::SignalMsg(_sig) => {
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
        for (n, job) in &self.jobs {
            println!("[{}] {} {}", n, job.pgid, job.command);
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
        if let Ok(n) = arg[1].parse::<JobId>() {
            if let Some(job) = self.jobs.get(&n) {
                eprintln!("ZeroSh: [{}] continued {}", n, job.command);
                self.process_manager.set_fg(job.pgid).unwrap();
                signal::killpg(job.pgid, Signal::SIGCONT).unwrap();
                return true;
            }
        }
        eprintln!("ZeroSh: fg: Job not found: {}", arg[1]);
        shell_tx.send(ShellMsg::Continue(self.exit_code)).unwrap();
        true
    }

    fn built_in_cmd(&mut self, cmd: &[Cmd], shell_tx: &mpsc::SyncSender<ShellMsg>) -> bool {
        if cmd.len() != 1 {
            return false;
        }
        match cmd[0].name {
            "exit" => self.run_exit(&cmd[0].args, shell_tx),
            "cd" => self.run_cd(&cmd[0].args, shell_tx),
            "jobs" => self.run_jobs(shell_tx),
            "fg" => self.run_fg(&cmd[0].args, shell_tx),
            _ => false,
        }
    }

    fn spawn_child(&mut self, line: &str, cmd: &[Cmd]) -> Result<()> {
        assert!(!cmd.is_empty());

        let job_id = self.get_next_job_id().context("Too many jobs")?;
        if cmd.len() > 2 {
            Err(anyhow::anyhow!(
                "Pipe line doesn't support more than 2 commands"
            ))?;
        }
        // Create a pipe
        let (input, output) = match cmd.len() {
            1 => (None, None),
            2 => {
                let (i, o) = unistd::pipe()?;
                (Some(i), Some(o))
            }
            _ => unreachable!(),
        };
        let pgid = Pid::from_raw(0);
        let mut pg = ProcessGroup {
            pids: HashSet::new(),
        };
        let pid1 = fork_exec(pgid, cmd[0].name, &cmd[0].args, None, output)?;
        pg.pids.insert(pid1);
        if cmd.len() == 2 {
            let pid2 = fork_exec(pgid, cmd[1].name, &cmd[1].args, input, None)?;
            pg.pids.insert(pid2);
        }
        self.process_manager.add_group(pgid, pg);
        self.jobs.insert(
            job_id,
            JobInfo {
                pgid,
                command: line.to_string(),
            },
        );

        // Close pipe
        if let Some(fd) = input {
            syscall(|| unistd::close(fd))?;
        }
        if let Some(fd) = output {
            syscall(|| unistd::close(fd))?;
        }

        // Set the shell process to the foreground
        self.process_manager.set_fg(pgid)?;
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
                    self.process_manager.terminate(pid, shell_tx).unwrap();
                }
                Ok(wait::WaitStatus::Signaled(pid, signal, _core)) => {
                    eprintln!("ZeroSh: Process {} terminated by signal {}", pid, signal);
                    self.exit_code = 128 + signal as i32;
                    self.process_manager.terminate(pid, shell_tx).unwrap();
                }
                Ok(wait::WaitStatus::Stopped(pid, _signal)) => {
                    self.process_manager.stop(pid, shell_tx).unwrap()
                }
                Ok(wait::WaitStatus::Continued(pid)) => self.process_manager.resume(pid).unwrap(),
                Ok(wait::WaitStatus::StillAlive) => return,
                Err(nix::Error::ECHILD) => return,
                Err(e) => {
                    eprintln!("ZeroSh: waitpid: {}", e);
                    std::process::exit(1);
                }
                #[cfg(any(target_os = "linux", target_os = "android"))]
                Ok(wait::WaitStatus::PtraceEvent(pid, _, _)) => {
                    self.process_manager.resume(pid).unwrap()
                }
                #[cfg(any(target_os = "linux", target_os = "android"))]
                Ok(wait::WaitStatus::PtraceSyscall(pid)) => {
                    self.process_manager.resume(pid).unwrap()
                }
            }
        }
    }

    fn get_next_job_id(&self) -> Option<JobId> {
        (1..)
            .map(|n| n.into())
            .find(|n| !self.jobs.contains_key(&n))
    }
}

fn fork_exec(
    pgid: Pid,
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
            unistd::setpgid(child, pgid)?;
            Ok(child)
        }
        unistd::ForkResult::Child => {
            unistd::setpgid(Pid::from_raw(0), pgid)?;

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
