use anyhow::Result;
use nix::sys::signal::{signal, SigHandler, Signal};
use std::sync::mpsc;
use thread::{spawn_sig_handler, ShellMsg, Worker, WorkerMsg};

mod parser;
mod thread;

fn syscall<F, T>(f: F) -> Result<T, nix::Error>
where
    F: Fn() -> Result<T, nix::Error>,
{
    loop {
        match f() {
            Err(nix::errno::Errno::EINTR) => continue,
            other => return other,
        }
    }
}

#[derive(Debug)]
pub struct Shell {
    logfile: String, // Log file
}

impl Shell {
    pub fn new(logfile: &str) -> Self {
        Self {
            logfile: logfile.to_string(),
        }
    }

    /// Main thread
    pub fn run(&self) -> Result<()> {
        // Ignore SIGTTOU (signal when background process writes to stdout)
        unsafe { signal(Signal::SIGTTOU, SigHandler::SigIgn) }?;

        let mut rl = rustyline::Editor::<()>::new()?;
        if let Err(e) = rl.load_history(&self.logfile) {
            eprintln!("ZeroSh: Unable to load history: {}", e);
        }

        let (worker_tx, worker_rx) = mpsc::sync_channel(64);
        let (shell_tx, shell_rx) = mpsc::sync_channel(0);
        spawn_sig_handler(worker_tx.clone())?;
        Worker::new().spawn(worker_rx, shell_tx)?;

        let exit_val;
        let mut prev = 0;
        loop {
            let symbol = if prev == 0 { '$' } else { '!' };
            match rl.readline(&format!("ZeroSh {} ", symbol)) {
                Ok(line) => {
                    let line_trimmed = line.trim();
                    if line_trimmed.is_empty() {
                        continue;
                    }
                    rl.add_history_entry(line_trimmed);

                    worker_tx.send(WorkerMsg::Cmd(line))?;
                    match shell_rx.recv()? {
                        ShellMsg::Continue(n) => prev = n,
                        ShellMsg::Quit(n) => {
                            exit_val = n;
                            break;
                        }
                    }
                }
                Err(rustyline::error::ReadlineError::Interrupted) => {
                    eprintln!("ZeroSh: Interrupted")
                }
                Err(rustyline::error::ReadlineError::Eof) => {
                    worker_tx.send(WorkerMsg::Cmd("exit".to_string()))?;
                    match shell_rx.recv()? {
                        ShellMsg::Quit(n) => {
                            exit_val = n;
                            break;
                        }
                        _ => unreachable!(),
                    }
                }
                Err(e) => {
                    eprintln!("ZeroSh: Error: {}", e);
                    exit_val = 1;
                    break;
                }
            }
        }
        if let Err(e) = rl.save_history(&self.logfile) {
            eprintln!("ZeroSh: Unable to save history: {}", e);
        }
        std::process::exit(exit_val);
    }
}
