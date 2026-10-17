use std::process::{Command, Output, Stdio};

use crate::Result;

/// Runs `command` and returns its stdout.
///
/// Stdin is `/dev/null` and stdout and stderr are captured, because the agent's own stdin and
/// stdout carry the protocol: a child inheriting them would swallow or corrupt messages.
pub(crate) fn run(command: &mut Command) -> Result<String> {
    let output = output(command)?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    Err(failure(command, &output).into())
}

/// Runs `command` with the same stdio as [`run`] and returns its output whatever the exit status.
pub(crate) fn output(command: &mut Command) -> Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("{}: {error}", line(command)).into())
}

/// Names the command and what it wrote; stdout stands in when stderr is empty, since apt writes
/// some errors there.
pub(crate) fn failure(command: &Command, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let message = match stderr.trim() {
        "" => stdout.trim(),
        stderr => stderr,
    };
    format!("{} failed ({}): {message}", line(command), output.status)
}

fn line(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn success_returns_stdout() {
        assert_eq!(run(&mut sh("echo hi; echo noise >&2")).unwrap(), "hi\n");
    }

    #[test]
    fn failure_carries_the_command_and_stderr() {
        let error = run(&mut sh("echo out; echo 'permission denied' >&2; exit 100"))
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("sh -c echo out;"), "{error}");
        assert!(error.ends_with(": permission denied"), "{error}");
        assert!(!error.contains("out\n"), "{error}");
    }

    #[test]
    fn failure_without_stderr_carries_stdout() {
        let error = run(&mut sh("echo 'E: Unable to locate package'; exit 100"))
            .unwrap_err()
            .to_string();
        assert!(error.ends_with(": E: Unable to locate package"), "{error}");
    }

    #[test]
    fn stdin_is_at_eof() {
        assert_eq!(
            run(&mut sh("if read line; then echo got; else echo eof; fi")).unwrap(),
            "eof\n"
        );
    }

    #[test]
    fn missing_program_names_it() {
        let error = run(&mut Command::new("fettle-no-such-program"))
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("fettle-no-such-program: "), "{error}");
    }
}
