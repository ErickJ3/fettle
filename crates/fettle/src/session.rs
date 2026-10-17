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

/// Probes an idle connection so a link that dropped silently fails the waits on the agent
/// instead of hanging them; ssh gives up after three unanswered probes.
const SERVER_ALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// A failure on one host.
///
/// `message` leaves out the [`source`](std::error::Error::source), so report the whole chain:
/// for ssh failures it holds the ssh client's own message.
#[derive(Debug, Clone)]
pub struct Error {
    destination: String,
    kind: ErrorKind,
    message: String,
    source: Option<Arc<dyn std::error::Error + Send + Sync>>,
}

/// What failed, for callers that handle failures differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// ssh could not connect, run a command or close.
    Ssh,
    /// This build has no agent for the host's architecture.
    UnsupportedArch,
    /// Uploading the agent failed.
    Upload,
    /// The agent's pipes broke or it sent something other than the expected message.
    Protocol,
    /// The agent exited non-zero.
    Agent,
}

impl Error {
    #[must_use]
    pub fn destination(&self) -> &str {
        &self.destination
    }

    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn new(destination: &str, kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            destination: destination.to_owned(),
            kind,
            message: message.into(),
            source: None,
        }
    }

    fn caused_by(self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            source: Some(Arc::new(source)),
            ..self
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.destination, self.message)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

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
    pub async fn connect(destination: impl Into<String>) -> Result<(Session, Facts), Error> {
        let destination = destination.into();
        let fail = |kind, message: &str| Error::new(&destination, kind, message);
        let ssh = SessionBuilder::default()
            .control_persist(ControlPersist::IdleFor(CONTROL_PERSIST_SECONDS))
            .connect_timeout(CONNECT_TIMEOUT)
            .server_alive_interval(SERVER_ALIVE_INTERVAL)
            .connect(&destination)
            .await
            .map_err(|e| fail(ErrorKind::Ssh, "cannot connect").caused_by(e))?;
        let ssh = Arc::new(ssh);

        let machine = run(sh(&ssh, "uname -m"), None, |message| {
            fail(ErrorKind::Ssh, message)
        })
        .await?;
        let agent = select(&BUNDLED, machine.trim())
            .map_err(|message| fail(ErrorKind::UnsupportedArch, &message))?;
        let path = format!("\"$HOME/.cache/fettle/agent-{}\"", agent.sha256);
        let cached = sh(&ssh, &format!("test -x {path}"))
            .status()
            .await
            .map_err(|e| fail(ErrorKind::Ssh, "cannot look for a cached agent").caused_by(e))?
            .success();
        if !cached {
            // An interrupted upload still ends `cat` with EOF and status 0, so the size check is
            // what keeps a truncated agent from being published under its final name.
            let size = agent.bytes.len();
            let upload = format!(
                "mkdir -p \"$HOME/.cache/fettle\" && tmp={path}.$$ && cat > \"$tmp\" \
                 && received=$(wc -c < \"$tmp\") \
                 && if [ $received -ne {size} ]; then rm -f \"$tmp\"; echo \"received $received of {size} bytes\" >&2; exit 1; fi \
                 && chmod 700 \"$tmp\" && mv -f \"$tmp\" {path}"
            );
            run(sh(&ssh, &upload), Some(agent.bytes), |message| {
                fail(
                    ErrorKind::Upload,
                    &format!("uploading the agent: {message}"),
                )
            })
            .await?;
        }

        let (agent, stdin, stdout) = start(&ssh, &path)
            .await
            .map_err(|e| fail(ErrorKind::Ssh, "cannot start the agent").caused_by(e))?;
        let mut session = Session {
            destination,
            ssh,
            agent,
            stdin,
            stdout,
        };
        match session.recv().await {
            Ok(AgentMessage::Facts(facts)) => Ok((session, facts)),
            Ok(other) => {
                let problem = session.error(
                    ErrorKind::Protocol,
                    format!("expected facts, got {other:?}"),
                );
                Err(session.abort(problem).await)
            }
            Err(problem) => Err(session.abort(problem).await),
        }
    }

    /// Sends one message to the agent.
    ///
    /// # Errors
    ///
    /// Fails when the agent's stdin is closed.
    ///
    /// # Cancel safety
    ///
    /// Not cancel safe: a dropped future may leave part of a line on the agent's stdin, after
    /// which the session can only be closed.
    pub async fn send(&mut self, message: &CliMessage) -> Result<(), Error> {
        let written = async {
            let mut line = serde_json::to_vec(message)?;
            line.push(b'\n');
            self.stdin.write_all(&line).await?;
            self.stdin.flush().await
        };
        written.await.map_err(|e| {
            self.error(ErrorKind::Protocol, "cannot send to the agent")
                .caused_by(e)
        })
    }

    /// Reads the agent's next message.
    ///
    /// # Errors
    ///
    /// Fails when the agent's stdout ends or the line is not an agent message.
    ///
    /// # Cancel safety
    ///
    /// Cancel safe: a dropped future has received no message, so none is lost.
    pub async fn recv(&mut self) -> Result<AgentMessage, Error> {
        let line = self
            .stdout
            .next_line()
            .await
            .map_err(|e| {
                self.error(ErrorKind::Protocol, "cannot read from the agent")
                    .caused_by(e)
            })?
            .ok_or_else(|| self.error(ErrorKind::Protocol, "the agent closed its output"))?;
        serde_json::from_str(&line).map_err(|e| {
            self.error(
                ErrorKind::Protocol,
                format!("unreadable agent message {line:?}"),
            )
            .caused_by(e)
        })
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
        let fail = |kind, message: &str| Error::new(&destination, kind, message);
        let output = agent
            .wait_with_output()
            .await
            .map_err(|e| fail(ErrorKind::Ssh, "cannot wait for the agent").caused_by(e))?;
        if !output.status.success() {
            return Err(fail(ErrorKind::Agent, &agent_failure(&output)));
        }
        if let Ok(ssh) = Arc::try_unwrap(ssh) {
            ssh.close()
                .await
                .map_err(|e| fail(ErrorKind::Ssh, "cannot close the connection").caused_by(e))?;
        }
        Ok(())
    }

    /// Waits for an agent that broke the protocol and adds what it wrote to stderr to `problem`.
    async fn abort(self, problem: Error) -> Error {
        let Session {
            agent,
            stdin,
            stdout,
            ..
        } = self;
        drop((stdin, stdout));
        match agent.wait_with_output().await {
            Ok(output) => Error {
                kind: if output.status.success() {
                    problem.kind
                } else {
                    ErrorKind::Agent
                },
                message: format!("{}; {}", problem.message, agent_failure(&output)),
                ..problem
            },
            // `problem` came first; a failed wait on top of it says nothing about the agent.
            Err(_) => problem,
        }
    }

    fn error(&self, kind: ErrorKind, message: impl Into<String>) -> Error {
        Error::new(&self.destination, kind, message)
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

/// Runs a short command to completion, feeding it `input`, and returns its stdout; `fail` turns
/// a message into the error the caller reports for this command.
async fn run(
    mut command: OwningCommand<Arc<openssh::Session>>,
    input: Option<&[u8]>,
    fail: impl Fn(&str) -> Error,
) -> Result<String, Error> {
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
        .map_err(|e| fail("cannot run the command").caused_by(e))?;
    let written = match input {
        Some(input) => {
            let mut stdin = child.stdin().take().expect("piped stdin");
            async {
                stdin.write_all(input).await?;
                stdin.shutdown().await
            }
            .await
        }
        None => Ok(()),
    };
    // A command that fails early breaks the pipe; its status and stderr say why, the write
    // error does not.
    let output = child
        .wait_with_output()
        .await
        .map_err(|e| fail("cannot wait for the command").caused_by(e))?;
    if !output.status.success() {
        return Err(fail(&format!(
            "{}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    written.map_err(|e| fail("cannot write the command's input").caused_by(e))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn agent_failure(output: &Output) -> String {
    format!(
        "agent {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )
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
        let error = Error::new("nas", ErrorKind::Agent, agent_failure(&output));
        assert_eq!(
            error.to_string(),
            "nas: agent exit status: 1: fettle-agent: no os-release file at /etc/os-release or /usr/lib/os-release"
        );
    }
}
