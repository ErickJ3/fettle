use std::fs;
use std::io::ErrorKind;

use fettle_protocol::{Facts, Os};

use crate::Result;

const OS_RELEASE_PATHS: [&str; 2] = ["/etc/os-release", "/usr/lib/os-release"];

/// Reads the facts of the host this agent runs on.
///
/// # Errors
///
/// Fails when no os-release file exists, it has no `ID`, or the hostname cannot be read.
pub fn read() -> Result<Facts> {
    Ok(Facts {
        os: parse_os_release(&read_os_release(&OS_RELEASE_PATHS)?)?,
        arch: std::env::consts::ARCH.into(),
        hostname: fs::read_to_string("/proc/sys/kernel/hostname")?
            .trim()
            .into(),
    })
}

fn read_os_release(paths: &[&str]) -> Result<String> {
    for path in paths {
        match fs::read_to_string(path) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{path}: {error}").into()),
            Ok(text) => return Ok(text),
        }
    }
    Err(format!("no os-release file at {}", paths.join(" or ")).into())
}

/// Parses an os-release file; the family is the last word of `ID_LIKE`, since derivatives
/// list their ancestry nearest first.
fn parse_os_release(text: &str) -> Result<Os> {
    let mut id = None;
    let mut id_like = None;
    let mut version = None;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = unquote(value).to_owned();
        match key {
            "ID" => id = Some(value),
            "ID_LIKE" => id_like = Some(value),
            "VERSION_ID" => version = Some(value),
            _ => {}
        }
    }
    let id = id.ok_or("os-release has no ID")?;
    let family = id_like
        .as_deref()
        .and_then(|like| like.split_whitespace().last())
        .unwrap_or(&id)
        .to_owned();
    Ok(Os {
        family,
        id,
        version,
    })
}

fn unquote(value: &str) -> &str {
    ['"', '\'']
        .into_iter()
        .find_map(|q| value.strip_prefix(q)?.strip_suffix(q))
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(text: &str) -> Os {
        parse_os_release(text).unwrap()
    }

    #[test]
    fn ubuntu() {
        let text =
            "# comment\n\nNAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=debian\nVERSION_ID=\"24.04\"\n";
        assert_eq!(
            os(text),
            Os {
                family: "debian".into(),
                id: "ubuntu".into(),
                version: Some("24.04".into()),
            }
        );
    }

    #[test]
    fn derivative_of_a_derivative() {
        let os = os("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\nVERSION_ID='22'\n");
        assert_eq!(os.family, "debian");
        assert_eq!(os.version.as_deref(), Some("22"));
    }

    #[test]
    fn no_id_like() {
        assert_eq!(os("ID=debian\nVERSION_ID=\"13\"\n").family, "debian");
    }

    #[test]
    fn no_version() {
        assert_eq!(os("ID=debian\n").version, None);
    }

    #[test]
    fn no_os_release_names_both_paths() {
        let error = read_os_release(&[
            "/nonexistent/etc/os-release",
            "/nonexistent/usr/lib/os-release",
        ])
        .unwrap_err()
        .to_string();
        assert_eq!(
            error,
            "no os-release file at /nonexistent/etc/os-release or /nonexistent/usr/lib/os-release"
        );
    }
}
