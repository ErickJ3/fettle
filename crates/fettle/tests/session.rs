//! End to end against a real `sshd`; runs only when `FETTLE_TEST_SSH` names a disposable host
//! whose `~/.cache/fettle` this test may delete.

use std::process::Command;

use fettle::session::Session;

fn ssh(destination: &str, script: &str) -> String {
    let output = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=accept-new",
        ])
        .args([destination, script])
        .output()
        .expect("run ssh");
    assert!(
        output.status.success(),
        "{script}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn cached_agent(destination: &str) -> String {
    ssh(
        destination,
        "stat -c '%n %.9Y %i' \"$HOME\"/.cache/fettle/agent-* 2>/dev/null || true",
    )
}

fn running_agents(destination: &str) -> String {
    ssh(
        destination,
        "for exe in /proc/[0-9]*/exe; do readlink \"$exe\"; done 2>/dev/null | grep /.cache/fettle/ || true",
    )
}

#[tokio::test]
async fn session_against_a_real_host() {
    let Ok(destination) = std::env::var("FETTLE_TEST_SSH") else {
        return;
    };
    ssh(&destination, "rm -rf \"$HOME/.cache/fettle\"");

    let (session, facts) = Session::connect(&destination).await.unwrap();
    assert_eq!(facts.os.id, "debian");
    assert_eq!(facts.os.family, "debian");
    assert_eq!(facts.os.version.as_deref(), Some("13"));
    assert_eq!(facts.arch, ssh(&destination, "uname -m").trim());
    let uploaded = cached_agent(&destination);
    assert_eq!(uploaded.lines().count(), 1, "{uploaded}");
    session.close().await.unwrap();

    let (session, _) = Session::connect(&destination).await.unwrap();
    assert_eq!(cached_agent(&destination), uploaded);
    session.close().await.unwrap();

    assert_eq!(running_agents(&destination), "");
}
