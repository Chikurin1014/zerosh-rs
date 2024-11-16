mod job;
mod process;
mod worker;

use anyhow::Result;
use nix::{libc::c_int, sys::signal::Signal};
use signal_hook::iterator::Signals;
use std::sync::mpsc;

pub use worker::{Worker, WorkerMsg};

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
