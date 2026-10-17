use std::path::PathBuf;

use fettle::config::{Config, Error, ErrorKind, Host};
use fettle_protocol::{Catalog, Desired, Facts, Os, Pkg, Resource, Service, User};
use tempfile::TempDir;

struct Loaded {
    dir: TempDir,
    result: Result<Config, Error>,
}

/// Writes `source` to `fettle.ts` in a fresh directory and loads it.
fn load(source: &str) -> Loaded {
    load_with(&[], source)
}

/// Like [`load`], with `files` written next to `fettle.ts` first.
fn load_with(files: &[(&str, &str)], source: &str) -> Loaded {
    let dir = TempDir::new().unwrap();
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).unwrap();
    }
    let path = dir.path().join("fettle.ts");
    std::fs::write(&path, source).unwrap();
    let result = Config::load(&path);
    Loaded { dir, result }
}

fn config(source: &str) -> (TempDir, Config) {
    let loaded = load(source);
    let config = loaded.result.unwrap_or_else(|e| panic!("load failed: {e}"));
    (loaded.dir, config)
}

fn load_error(source: &str) -> Error {
    match load(source).result {
        Ok(_) => panic!("load should fail"),
        Err(error) => error,
    }
}

fn catalog_of(source: &str, host: &str) -> Result<Catalog, Error> {
    catalog_with(source, host, &debian())
}

fn catalog_with(source: &str, host: &str, facts: &Facts) -> Result<Catalog, Error> {
    let (_dir, config) = config(source);
    let host = config.hosts().iter().find(|h| h.name() == host).unwrap();
    config.catalog(host, facts)
}

fn catalog_error(source: &str, host: &str) -> Error {
    catalog_of(source, host).expect_err("catalog should fail")
}

fn debian() -> Facts {
    Facts {
        os: Os {
            family: "debian".to_owned(),
            id: "debian".to_owned(),
            version: Some("12".to_owned()),
        },
        arch: "x86_64".to_owned(),
        hostname: "nas".to_owned(),
    }
}

fn host_names(config: &Config) -> Vec<&str> {
    config.hosts().iter().map(Host::name).collect()
}

fn pkg(id: &str) -> Resource {
    Resource {
        id: id.to_owned(),
        desired: Desired::Pkg(Pkg { absent: false }),
    }
}

#[track_caller]
fn assert_at(error: &Error, line: u32) {
    let location = error
        .location()
        .unwrap_or_else(|| panic!("no location: {error}"));
    assert_eq!(location.file.file_name().unwrap(), "fettle.ts", "{error}");
    assert_eq!(location.line, line, "{error}");
}

#[track_caller]
fn assert_mentions(error: &Error, needles: &[&str]) {
    let text = error.to_string();
    for needle in needles {
        assert!(text.contains(needle), "{text:?} should mention {needle:?}");
    }
}

#[test]
fn typed_configuration_on_a_machine_without_node() {
    let (_dir, config) = config(
        r#"import { host, pkg } from "fettle";
interface Role {
  packages: string[];
}
type Name = "nas" | "pi";
const web: Role = { packages: ["caddy"] };
const name = "nas" as Name;
host(name, (h: { os: { family: string } }): void => {
  pkg(web.packages as string[]);
});
"#,
    );
    assert_eq!(host_names(&config), ["nas"]);
    let catalog = config.catalog(&config.hosts()[0], &debian()).unwrap();
    assert_eq!(catalog, [pkg("caddy")]);
}

#[test]
fn syntax_error() {
    let error = load_error(
        r#"import { host, pkg } from "fettle";

host("nas", () => {
  pkg("htop"
});
"#,
    );
    assert_at(&error, 4);
}

#[test]
fn exception_at_the_top_level() {
    let error = load_error(
        r#"import { host } from "fettle";

function check(value: string) {
  return value;
}

throw new Error("no hosts today");
"#,
    );
    assert_at(&error, 7);
    assert_mentions(&error, &["no hosts today"]);
}

#[test]
fn unsettled_top_level_await_fails() {
    let error = load_error("await new Promise(() => {});\n");
    assert_mentions(&error, &["never settles"]);
}

#[test]
fn import_other_than_fettle_fails() {
    let error = load_error("import { host } from \"fettle\";\nimport roles from \"./roles\";\n");
    assert_at(&error, 2);
    assert_mentions(&error, &["./roles"]);
}

#[test]
fn hosts_declared_in_a_loop() {
    let (_dir, config) = config(
        r#"import { host, pkg } from "fettle";
for (const name of ["pi1", "pi2", "pi3"]) {
  host(name, () => pkg("htop"));
}
"#,
    );
    assert_eq!(host_names(&config), ["pi1", "pi2", "pi3"]);
}

#[test]
fn default_export_is_ignored() {
    let (_dir, config) = config(
        r#"import { host } from "fettle";
export default { hosts: ["pi"] };
host("nas", () => {});
"#,
    );
    assert_eq!(host_names(&config), ["nas"]);
}

#[test]
fn duplicate_host_name() {
    let error = load_error(
        r#"import { host } from "fettle";
host("nas", () => {});

host("nas", () => {});
"#,
    );
    assert_at(&error, 4);
    assert_mentions(&error, &["\"nas\"", "fettle.ts:2:"]);
}

#[test]
fn host_without_ssh_option() {
    let (_dir, config) = config("import { host } from \"fettle\";\nhost(\"nas\", () => {});\n");
    assert_eq!(config.hosts()[0].ssh(), "nas");
    assert_eq!(config.hosts()[0].tags(), Vec::<String>::new());
}

#[test]
fn explicit_ssh_option() {
    let (_dir, config) = config(
        "import { host } from \"fettle\";\nhost(\"nas\", { ssh: \"root@192.168.1.10\", tags: [\"home\"] }, () => {});\n",
    );
    assert_eq!(config.hosts()[0].ssh(), "root@192.168.1.10");
    assert_eq!(config.hosts()[0].tags(), ["home"]);
}

#[test]
fn unknown_host_option() {
    let error = load_error(
        "import { host } from \"fettle\";\n\nhost(\"nas\", { user: \"root\" }, () => {});\n",
    );
    assert_at(&error, 3);
    assert_mentions(&error, &["\"nas\"", "user"]);
}

#[test]
fn host_option_of_the_wrong_type() {
    let error = load_error(
        "import { host } from \"fettle\";\nhost(\"nas\", { tags: \"home\" }, () => {});\n",
    );
    assert_at(&error, 2);
    assert_mentions(&error, &["\"nas\"", "tags"]);
}

#[test]
fn host_option_that_json_cannot_hold() {
    let error = load_error(
        "import { host } from \"fettle\";\nhost(\"nas\", { ssh: 1n } as any, () => {});\n",
    );
    assert_at(&error, 2);
    assert_mentions(&error, &["\"nas\"", "BigInt"]);
}

#[test]
fn props_that_json_cannot_hold() {
    let error = catalog_error(
        "import { host, pkg } from \"fettle\";\nhost(\"nas\", () => {\n  pkg(\"htop\", { absent: 1n } as any);\n});\n",
        "nas",
    );
    assert_at(&error, 3);
    assert_mentions(&error, &["pkg", "BigInt"]);
}

#[test]
fn host_without_a_callback() {
    let error =
        load_error("import { host } from \"fettle\";\n\nhost(\"nas\", { ssh: \"root@nas\" });\n");
    assert_at(&error, 3);
    assert_mentions(&error, &["\"nas\"", "callback"]);
}

#[test]
fn branch_on_distribution_family() {
    let catalog = catalog_of(
        r#"import { host, pkg } from "fettle";
host("nas", (h) => {
  pkg(h.os.family === "debian" ? "apache2" : "httpd");
});
"#,
        "nas",
    )
    .unwrap();
    assert_eq!(catalog, [pkg("apache2")]);
}

#[test]
fn distribution_without_a_version() {
    let mut facts = debian();
    facts.os.version = None;
    let catalog = catalog_with(
        r#"import { host, pkg } from "fettle";
host("nas", (h) => {
  if (h.os.version === null) pkg("testing-only");
});
"#,
        "nas",
        &facts,
    )
    .unwrap();
    assert_eq!(catalog, [pkg("testing-only")]);
}

#[test]
fn async_callback() {
    let error = catalog_error(
        "import { host, pkg } from \"fettle\";\nhost(\"nas\", async () => { pkg(\"htop\"); });\n",
        "nas",
    );
    assert_mentions(&error, &["\"nas\""]);
}

#[test]
fn exception_in_a_host_callback() {
    let error = catalog_error(
        r#"import { host, pkg } from "fettle";

function fail(): never {
  throw new Error("disk not found");
}

host("pi", () => {});

host("nas", () => {
  pkg("htop");

  fail();
});
"#,
        "nas",
    );
    assert_at(&error, 4);
    assert_mentions(&error, &["\"nas\"", "disk not found"]);

    let error = catalog_error(
        r#"import { host, pkg } from "fettle";









host("nas", () => {
  throw new Error("boom");
});
"#,
        "nas",
    );
    assert_at(&error, 12);
    assert_mentions(&error, &["\"nas\"", "boom"]);
}

#[test]
fn thrown_string_in_a_host_callback_points_at_the_host() {
    let error = catalog_error(
        "import { host } from \"fettle\";\n\nhost(\"nas\", () => {\n  throw \"boom\";\n});\n",
        "nas",
    );
    assert_at(&error, 3);
    assert_mentions(&error, &["\"nas\"", "\"boom\""]);
}

#[test]
fn exception_in_a_function_whose_name_has_parentheses() {
    let error = catalog_error(
        r#"import { host, pkg } from "fettle";
const o = { ["role (web)"]() {
  pkg(42 as any);
} };
host("nas", () => {

  o["role (web)"]();
});
"#,
        "nas",
    );
    assert_at(&error, 3);
}

#[test]
fn runtime_error_column_counts_utf16_units() {
    let error = catalog_error(
        "import { host, service } from \"fettle\";\nhost(\"nas\", () => {\n  const s = \"😀😀\"; service(\"a\", { enabled: 1 } as any);\n});\n",
        "nas",
    );
    assert_eq!(error.location().unwrap().column, 21, "{error}");
}

#[test]
fn resource_at_top_level() {
    let error = load_error("import { pkg } from \"fettle\";\n\npkg(\"htop\");\n");
    assert_at(&error, 3);
    assert_mentions(&error, &["pkg()"]);
}

#[test]
fn host_inside_a_callback() {
    let error = catalog_error(
        "import { host } from \"fettle\";\nhost(\"nas\", () => {\n  host(\"pi\", () => {});\n});\n",
        "nas",
    );
    assert_at(&error, 3);
    assert_mentions(&error, &["host()"]);
}

#[test]
fn callback_state_resets_after_a_failure() {
    let (_dir, config) = config(
        "import { host, pkg } from \"fettle\";\nhost(\"bad\", () => { pkg(\"x\"); throw new Error(\"no\"); });\nhost(\"good\", () => pkg(\"htop\"));\n",
    );
    let [bad, good] = config.hosts() else {
        panic!()
    };
    assert!(config.catalog(bad, &debian()).is_err());
    assert_eq!(config.catalog(good, &debian()).unwrap(), [pkg("htop")]);
}

#[test]
fn host_from_another_config_is_rejected() {
    let (_one_dir, one) = config("import { host } from \"fettle\";\nhost(\"nas\", () => {});\n");
    let (_two_dir, two) = config(
        "import { host } from \"fettle\";\nhost(\"nas\", () => {});\nhost(\"pi\", () => {});\n",
    );
    let error = one.catalog(&two.hosts()[1], &debian()).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UnknownHost);
    assert_mentions(&error, &["\"pi\"", "not registered"]);
}

#[test]
fn host_of_the_same_name_from_another_config_is_rejected() {
    let (_one_dir, one) = config(
        "import { host, pkg } from \"fettle\";\nhost(\"nas\", { ssh: \"root@a\" }, () => pkg(\"a\"));\n",
    );
    let (_two_dir, two) = config(
        "import { host, pkg } from \"fettle\";\nhost(\"nas\", { ssh: \"root@b\" }, () => pkg(\"b\"));\n",
    );
    let error = one.catalog(&two.hosts()[0], &debian()).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UnknownHost);
}

#[test]
fn missing_file_keeps_the_io_error() {
    let dir = TempDir::new().unwrap();
    let Err(error) = Config::load(dir.path().join("fettle.ts")) else {
        panic!("load should fail");
    };
    assert_eq!(error.kind(), ErrorKind::Io);
    let io = std::error::Error::source(&error)
        .and_then(|source| source.downcast_ref::<std::io::Error>())
        .expect("an io::Error source");
    assert_eq!(io.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn role_as_a_plain_function() {
    let catalog = catalog_of(
        r#"import { host, pkg } from "fettle";
function caddy() {
  pkg("caddy");
}
host("nas", () => caddy());
"#,
        "nas",
    )
    .unwrap();
    assert_eq!(catalog, [pkg("caddy")]);
}

#[test]
fn package_list_expands() {
    let catalog = catalog_of(
        "import { host, pkg } from \"fettle\";\nhost(\"nas\", () => pkg([\"htop\", \"tmux\"]));\n",
        "nas",
    )
    .unwrap();
    assert_eq!(catalog, [pkg("htop"), pkg("tmux")]);
}

#[test]
fn id_that_is_not_a_string() {
    for id in ["42", "[\"htop\", \"\"]"] {
        let error = catalog_error(
            &format!(
                "import {{ host, pkg }} from \"fettle\";\nhost(\"nas\", () => {{\n  pkg({id});\n}});\n"
            ),
            "nas",
        );
        assert_at(&error, 3);
        assert_mentions(&error, &["pkg"]);
    }
}

#[test]
fn two_roles_need_the_same_package() {
    let catalog = catalog_of(
        r#"import { host, pkg } from "fettle";
function web() {
  pkg("git");
  pkg("caddy");
}
function dev() {
  pkg("git", { absent: false });
}
host("nas", () => {
  web();
  dev();
});
"#,
        "nas",
    )
    .unwrap();
    assert_eq!(catalog, [pkg("git"), pkg("caddy")]);
}

#[test]
fn conflicting_declarations() {
    let error = catalog_error(
        r#"import { host, pkg } from "fettle";
host("nas", () => {
  pkg("nginx");

  pkg("nginx", { absent: true });
});
"#,
        "nas",
    );
    assert_at(&error, 5);
    assert_mentions(&error, &["pkg nginx", "fettle.ts:3:"]);
}

#[test]
fn wrong_prop_type_with_types_stripped() {
    let error = catalog_error(
        r#"import { host, service } from "fettle";
host("nas", () => {
  service("caddy", { enabled: "yes" } as any);
});
"#,
        "nas",
    );
    assert_at(&error, 3);
    assert_mentions(&error, &["service caddy", "enabled"]);
}

#[test]
fn unknown_prop() {
    let error = catalog_error(
        "import { host, service } from \"fettle\";\nhost(\"nas\", () => service(\"caddy\", { enable: true }));\n",
        "nas",
    );
    assert_at(&error, 2);
    assert_mentions(&error, &["service caddy", "enable"]);
}

#[test]
fn location_survives_type_stripping() {
    let error = catalog_error(
        r#"import { host, service } from "fettle";

interface ServiceProps {
  enabled?: boolean;
}

type Distro =
  | "debian"
  | "fedora";

interface Facts {
  os: {
    family: Distro;
    id: string;
  };
}

host("nas", (h: Facts): void => {
  const props = { enabled: 1 } as unknown as ServiceProps;
  service("caddy", props);
});
"#,
        "nas",
    );
    assert_at(&error, 20);
}

#[test]
fn ssh_key_from_a_local_file() {
    let loaded = load_with(
        &[("marcos.pub", "ssh-ed25519 AAAA marcos\n")],
        r#"import { host, read, user } from "fettle";
host("nas", () => {
  user("marcos", { sshKeys: [read("./marcos.pub")] });
});
"#,
    );
    let config = loaded.result.unwrap();
    let catalog = config.catalog(&config.hosts()[0], &debian()).unwrap();
    assert_eq!(
        catalog,
        [Resource {
            id: "marcos".to_owned(),
            desired: Desired::User(User {
                groups: None,
                ssh_keys: Some(vec!["ssh-ed25519 AAAA marcos\n".to_owned()]),
            }),
        }]
    );
}

#[test]
fn read_resolves_home() {
    let home = std::env::home_dir().unwrap();
    let key = tempfile::NamedTempFile::new_in(&home).unwrap();
    std::fs::write(key.path(), "from home").unwrap();
    let name = key.path().file_name().unwrap().to_str().unwrap();
    let (_dir, config) = config(&format!(
        "import {{ host, file, read }} from \"fettle\";\nconst text = read(\"~/{name}\");\nhost(\"nas\", () => file(\"/etc/motd\", {{ content: text }}));\n"
    ));
    let catalog = config.catalog(&config.hosts()[0], &debian()).unwrap();
    assert_eq!(
        serde_json::to_value(&catalog).unwrap()[0]["props"]["content"],
        "from home"
    );
}

#[test]
fn read_works_at_the_top_level() {
    let loaded = load_with(
        &[("hosts.txt", "pi1\npi2\n")],
        "import { host, read } from \"fettle\";\nconst names = read(\"hosts.txt\").trim().split(\"\\n\");\nfor (const name of names) host(name, () => {});\n",
    );
    assert_eq!(host_names(&loaded.result.unwrap()), ["pi1", "pi2"]);
}

#[test]
fn missing_local_file() {
    let loaded = load(
        r#"import { host, file, read } from "fettle";
host("nas", () => {

  file("/etc/motd", { content: read("./missing.txt") });
});
"#,
    );
    let config = loaded.result.unwrap();
    let error = config.catalog(&config.hosts()[0], &debian()).unwrap_err();
    assert_at(&error, 4);
    let resolved: PathBuf = loaded.dir.path().join("./missing.txt");
    assert_mentions(&error, &[&resolved.display().to_string()]);
}

#[test]
fn service_props_round_trip() {
    let catalog = catalog_of(
        "import { host, service } from \"fettle\";\nhost(\"nas\", () => service(\"caddy\", { enabled: true }));\n",
        "nas",
    )
    .unwrap();
    assert_eq!(
        catalog[0].desired,
        Desired::Service(Service {
            enabled: Some(true)
        })
    );
}
