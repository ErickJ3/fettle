use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

/// Bundles the agent binaries named by these variables; an unset one leaves that architecture
/// unreachable, so builds without zig keep working.
const AGENTS: [(&str, &str); 2] = [
    ("X86_64", "FETTLE_AGENT_X86_64"),
    ("AARCH64", "FETTLE_AGENT_AARCH64"),
];

fn main() {
    let out_dir = env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    let mut generated = String::new();
    for (name, variable) in AGENTS {
        println!("cargo:rerun-if-env-changed={variable}");
        let agent = match env::var_os(variable) {
            None => "None".to_owned(),
            Some(source) => {
                println!("cargo:rerun-if-changed={}", source.display());
                let copy = Path::new(&out_dir).join(format!("agent-{name}"));
                fs::copy(&source, &copy)
                    .unwrap_or_else(|e| panic!("{variable}={}: {e}", source.display()));
                let sha256 = hex(&Sha256::digest(fs::read(&copy).expect("copied agent")));
                format!(
                    "Some(Agent {{ bytes: include_bytes!({:?}), sha256: {sha256:?} }})",
                    copy.display()
                )
            }
        };
        writeln!(generated, "const {name}: Option<Agent> = {agent};").unwrap();
    }
    fs::write(Path::new(&out_dir).join("agents.rs"), generated).expect("write agents.rs");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        write!(s, "{b:02x}").unwrap();
        s
    })
}
