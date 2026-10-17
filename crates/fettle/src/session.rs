//! One run's connection to a host: the agent started over SSH and the messages exchanged with it.

use std::fmt;
use std::num::NonZeroUsize;
use std::process::Output;
use std::sync::Arc;
use std::time::Duration;

use fettle_protocol::{AgentMessage, CliMessage, Facts};
use openssh::{ChildStdin, ChildStdout, ControlPersist, OwningCommand, SessionBuilder, Stdio};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};

/// An agent binary bundled into this CLI at build time.
struct Agent {
    bytes: &'static [u8],
    /// Lowercase hex, so it is safe to interpolate into shell scripts.
    sha256: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/agents.rs"));

const BUNDLED: [(&str, Option<Agent>); 2] = [("x86_64", X86_64), ("aarch64", AARCH64)];

/// Covers the gaps between the sequential commands of `connect` and stops the master soon after
/// a CLI that was killed outright leaves it without clients.
const CONTROL_PERSIST_SECONDS: NonZeroUsize = NonZeroUsize::new(5).unwrap();

/// Fails an unreachable host instead of waiting out the OS TCP timeout, which can take minutes;
/// the value is a guess generous enough for slow links.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// A failure on one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub destination: String,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.destination, self.message)
    }
}

impl std::error::Error for Error {}

/// The agent running on a host, reached over one SSH connection.
pub struct Session {
    destination: String,
    ssh: Arc<openssh::Session>,
    agent: openssh::Child<Arc<openssh::Session>>,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
}

impl Session {
    /// Connects to `destination`, starts the agent and returns once it has sent its facts.
    ///
    /// # Errors
    ///
    /// Fails when ssh cannot connect, the host's architecture has no bundled agent, the upload
    /// fails, or the agent's first message is not its facts.
    pub async fn connect(destination: &str) -> Result<(Session, Facts), Error> {
        let fail = |message: String| Error {
            destination: destination.to_owned(),
            message,
        };
        let ssh = SessionBuilder::default()
            .control_persist(ControlPersist::IdleFor(CONTROL_PERSIST_SECONDS))
            .connect_timeout(CONNECT_TIMEOUT)
            .connect(destination)
            .await
            .map_err(|e| fail(chain(&e)))?;
        let ssh = Arc::new(ssh);

        let machine = run(sh(&ssh, "uname -m"), None).await.map_err(fail)?;
        let agent = select(&BUNDLED, machine.trim()).map_err(fail)?;
        let path = format!("\"$HOME/.cache/fettle/agent-{}\"", agent.sha256);
        let cached = sh(&ssh, &format!("test -x {path}"))
            .status()
            .await
            .map_err(|e| fail(chain(&e)))?
            .success();
        if !cached {
            let upload = format!(
                "mkdir -p \"$HOME/.cache/fettle\" && tmp={path}.$$ && cat > \"$tmp\" && chmod 700 \"$tmp\" && mv -f \"$tmp\" {path}"
            );
            run(sh(&ssh, &upload), Some(agent.bytes))
                .await
                .map_err(|e| fail(format!("uploading the agent: {e}")))?;
        }

        let (agent, stdin, stdout) = start(&ssh, &path).await.map_err(|e| fail(chain(&e)))?;
        let mut session = Session {
            destination: destination.to_owned(),
            ssh,
            agent,
            stdin,
            stdout,
        };
        match session.recv().await {
            Ok(AgentMessage::Facts(facts)) => Ok((session, facts)),
            Ok(other) => Err(session
                .abort(format!("expected facts, got {other:?}"))
                .await),
            Err(error) => Err(session.abort(error.message).await),
        }
    }

    /// Sends one message to the agent.
    ///
    /// # Errors
    ///
    /// Fails when the agent's stdin is closed.
    pub async fn send(&mut self, message: &CliMessage) -> Result<(), Error> {
        let written = async {
            let mut line = serde_json::to_vec(message)?;
            line.push(b'\n');
            self.stdin.write_all(&line).await?;
            self.stdin.flush().await
        };
        written
            .await
            .map_err(|e| self.error(format!("sending to the agent: {e}")))
    }

    /// Reads the agent's next message.
    ///
    /// # Errors
    ///
    /// Fails when the agent's stdout ends or the line is not an agent message.
    pub async fn recv(&mut self) -> Result<AgentMessage, Error> {
        let line = self
            .stdout
            .next_line()
            .await
            .map_err(|e| self.error(format!("reading from the agent: {e}")))?
            .ok_or_else(|| self.error("the agent closed its output".to_owned()))?;
        serde_json::from_str(&line)
            .map_err(|e| self.error(format!("unreadable agent message {line:?}: {e}")))
    }

    /// Ends the session: closes the agent's stdin, waits for it to exit, then closes SSH.
    ///
    /// # Errors
    ///
    /// Fails when the agent exits non-zero, with its status and error output.
    pub async fn close(self) -> Result<(), Error> {
        let Session {
            destination,
            ssh,
            agent,
            stdin,
            stdout,
        } = self;
        drop((stdin, stdout));
        let fail = |message: String| Error {
            destination: destination.clone(),
            message,
        };
        let output = agent
            .wait_with_output()
            .await
            .map_err(|e| fail(chain(&e)))?;
        if !output.status.success() {
            return Err(fail(agent_failure(&output)));
        }
        if let Ok(ssh) = Arc::try_unwrap(ssh) {
            ssh.close().await.map_err(|e| fail(chain(&e)))?;
        }
        Ok(())
    }

    /// Waits for an agent that broke the protocol and reports what it wrote to stderr.
    async fn abort(self, problem: String) -> Error {
        let Session {
            destination,
            agent,
            stdin,
            stdout,
            ..
        } = self;
        drop((stdin, stdout));
        let message = match agent.wait_with_output().await {
            Ok(output) => format!("{problem}; {}", agent_failure(&output)),
            Err(e) => format!("{problem}; {}", chain(&e)),
        };
        Error {
            destination,
            message,
        }
    }

    fn error(&self, message: String) -> Error {
        Error {
            destination: self.destination.clone(),
            message,
        }
    }
}

fn select<'a>(bundled: &'a [(&str, Option<Agent>)], machine: &str) -> Result<&'a Agent, String> {
    match bundled.iter().find(|(name, _)| *name == machine) {
        None => Err(format!("unsupported architecture {machine}")),
        Some((_, None)) => Err(format!("this fettle build has no agent for {machine}")),
        Some((_, Some(agent))) => Ok(agent),
    }
}

async fn start(
    ssh: &Arc<openssh::Session>,
    path: &str,
) -> Result<
    (
        openssh::Child<Arc<openssh::Session>>,
        ChildStdin,
        Lines<BufReader<ChildStdout>>,
    ),
    openssh::Error,
> {
    let mut child = sh(ssh, &format!("exec {path}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .await?;
    let stdin = child.stdin().take().expect("piped stdin");
    let stdout = BufReader::new(child.stdout().take().expect("piped stdout")).lines();
    Ok((child, stdin, stdout))
}

fn sh(ssh: &Arc<openssh::Session>, script: &str) -> OwningCommand<Arc<openssh::Session>> {
    let mut command = openssh::Session::to_command(Arc::clone(ssh), "sh");
    command.arg("-c").arg(script);
    command
}

/// Runs a short command to completion, feeding it `input`, and returns its stdout.
async fn run(
    mut command: OwningCommand<Arc<openssh::Session>>,
    input: Option<&[u8]>,
) -> Result<String, String> {
    let mut child = command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .await
        .map_err(|e| chain(&e))?;
    if let Some(input) = input {
        let mut stdin = child.stdin().take().expect("piped stdin");
        stdin.write_all(input).await.map_err(|e| e.to_string())?;
        stdin.shutdown().await.map_err(|e| e.to_string())?;
    }
    let output = child.wait_with_output().await.map_err(|e| chain(&e))?;
    if !output.status.success() {
        return Err(format!(
            "{}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn agent_failure(output: &Output) -> String {
    format!(
        "agent {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

/// `openssh` errors carry the ssh client's own message in their sources.
fn chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENT: Agent = Agent {
        bytes: b"",
        sha256: "00",
    };

    #[test]
    fn selects_the_agent_for_the_machine() {
        let bundled = [("x86_64", None), ("aarch64", Some(AGENT))];
        assert_eq!(select(&bundled, "aarch64").unwrap().sha256, "00");
    }

    #[test]
    fn unknown_architecture_is_named() {
        let bundled = [("x86_64", Some(AGENT)), ("aarch64", Some(AGENT))];
        assert_eq!(
            select(&bundled, "armv7l").err().unwrap(),
            "unsupported architecture armv7l"
        );
    }

    #[test]
    fn unbundled_architecture_is_named() {
        let bundled = [("x86_64", Some(AGENT)), ("aarch64", None)];
        assert_eq!(
            select(&bundled, "aarch64").err().unwrap(),
            "this fettle build has no agent for aarch64"
        );
    }

    #[test]
    fn error_names_the_destination_and_the_agent_output() {
        use std::os::unix::process::ExitStatusExt;

        let output = Output {
            status: std::process::ExitStatus::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: b"fettle-agent: no os-release file at /etc/os-release or /usr/lib/os-release\n"
                .to_vec(),
        };
        let error = Error {
            destination: "nas".into(),
            message: agent_failure(&output),
        };
        assert_eq!(
            error.to_string(),
            "nas: agent exit status: 1: fettle-agent: no os-release file at /etc/os-release or /usr/lib/os-release"
        );
    }
}
