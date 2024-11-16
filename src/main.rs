use zerosh;

const HISTORY_FILE: &str = ".zerosh_history";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut logfile = HISTORY_FILE;
    let mut home = dirs::home_dir();
    if let Some(h) = &mut home {
        h.push(HISTORY_FILE);
        logfile = h.to_str().unwrap_or(HISTORY_FILE);
    }
    zerosh::Shell::new(logfile).run()?;
    Ok(())
}
