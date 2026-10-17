use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use fettle_agent::{Result, facts};
use fettle_protocol::{AgentMessage, CliMessage};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fettle-agent: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &AgentMessage::Facts(facts::read()?))?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    for line in io::stdin().lock().lines() {
        // Catalog and Apply are handled once the agent can plan and apply; until then a valid
        // message is read and dropped.
        let _: CliMessage = serde_json::from_str(&line?)?;
    }
    Ok(())
}
