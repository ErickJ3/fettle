use std::fmt::Debug;

use fettle_protocol::{
    Action, AgentMessage, ApplyReport, CliMessage, Desired, Facts, Failure, File, Kind, Os, Pkg,
    Resource, ResourceChange, ResourceKey, User,
};
use serde::{Serialize, de::DeserializeOwned};

fn round_trip<T: Serialize + DeserializeOwned>(value: &T) -> T {
    let json = serde_json::to_string(value).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn assert_round_trips<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T) {
    assert_eq!(&round_trip(value), value);
}

fn desired(kind: &str, props: &str) -> Result<Desired, serde_json::Error> {
    serde_json::from_str(&format!(r#"{{"kind":"{kind}","props":{props}}}"#))
}

fn rejection(kind: &str, props: &str) -> String {
    desired(kind, props).unwrap_err().to_string()
}

fn key(kind: Kind, id: &str) -> ResourceKey {
    ResourceKey {
        kind,
        id: id.into(),
    }
}

fn change(kind: Kind, id: &str, action: Action, detail: Option<&str>) -> ResourceChange {
    ResourceChange {
        key: key(kind, id),
        action,
        detail: detail.map(Into::into),
    }
}

fn failed_install() -> ApplyReport {
    ApplyReport {
        applied: vec![change(Kind::Pkg, "htop", Action::Create, None)],
        failed: Some(Failure {
            key: key(Kind::Pkg, "does-not-exist"),
            error: "E: Unable to locate package does-not-exist".into(),
        }),
        skipped: vec![key(Kind::File, "/etc/motd")],
    }
}

fn ubuntu() -> Facts {
    Facts {
        os: Os {
            family: "debian".into(),
            id: "ubuntu".into(),
            version: Some("24.04".into()),
        },
        arch: "x86_64".into(),
        hostname: "nas".into(),
    }
}

#[test]
fn facts_ubuntu_server() {
    assert_eq!(
        serde_json::to_string(&ubuntu()).unwrap(),
        r#"{"os":{"family":"debian","id":"ubuntu","version":"24.04"},"arch":"x86_64","hostname":"nas"}"#
    );
}

#[test]
fn facts_without_version() {
    let mut facts = ubuntu();
    facts.os.version = None;

    let read = round_trip(&facts);

    assert_eq!(read.os.version, None);
    assert_eq!(read, facts);
}

#[test]
fn catalog_keeps_order_and_identity() {
    let catalog: Vec<Resource> = serde_json::from_str(
        r#"[
            {"id": "htop", "kind": "pkg", "props": {}},
            {"id": "/etc/motd", "kind": "file", "props": {"content": "hi\n"}},
            {"id": "tmux", "kind": "pkg", "props": {}}
        ]"#,
    )
    .unwrap();
    let installed = Desired::Pkg(Pkg { absent: false });
    let motd = Desired::File(File::Present {
        content: "hi\n".into(),
    });

    assert_eq!(
        catalog,
        [
            Resource {
                id: "htop".into(),
                desired: installed.clone(),
            },
            Resource {
                id: "/etc/motd".into(),
                desired: motd,
            },
            Resource {
                id: "tmux".into(),
                desired: installed,
            },
        ]
    );
    assert_round_trips(&catalog);
    assert_eq!(
        catalog.iter().map(Resource::key).collect::<Vec<_>>(),
        [
            key(Kind::Pkg, "htop"),
            key(Kind::File, "/etc/motd"),
            key(Kind::Pkg, "tmux"),
        ]
    );
}

#[test]
fn catalog_rejects_unknown_kind() {
    let error = serde_json::from_str::<Resource>(r#"{"id": "x", "kind": "widget", "props": {}}"#)
        .unwrap_err();

    assert!(error.to_string().contains("widget"), "{error}");
}

#[test]
fn props_pkg_without_props_is_installed() {
    assert_eq!(
        desired("pkg", "{}").unwrap(),
        Desired::Pkg(Pkg { absent: false })
    );
}

#[test]
fn props_wrong_type_is_rejected() {
    assert!(desired("service", r#"{"enabled": "yes"}"#).is_err());
}

#[test]
fn props_wrong_case_is_rejected() {
    let error = rejection("user", r#"{"sshkeys": []}"#);

    assert!(error.contains("`sshkeys`"), "{error}");
}

#[test]
fn props_groups_omitted_versus_empty() {
    let groups = |props| match round_trip(&desired("user", props).unwrap()) {
        Desired::User(User { groups, .. }) => groups,
        other => panic!("not a user: {other:?}"),
    };

    assert_eq!(groups("{}"), None);
    assert_eq!(groups(r#"{"groups": []}"#), Some(vec![]));
}

#[test]
fn file_without_content_is_rejected() {
    let error = rejection("file", "{}");

    assert!(error.contains("`content`"), "{error}");
}

#[test]
fn file_unknown_prop_is_rejected() {
    let error = rejection("file", r#"{"content": "hi", "mode": "0644"}"#);

    assert!(error.contains("`mode`"), "{error}");
}

#[test]
fn file_absent_with_content_is_rejected() {
    let error = rejection("file", r#"{"absent": true, "content": "hi"}"#);

    assert!(error.contains("`absent"), "{error}");
    assert!(error.contains("`content`"), "{error}");
}

#[test]
fn file_absent() {
    assert_eq!(
        desired("file", r#"{"absent": true}"#).unwrap(),
        Desired::File(File::Absent)
    );
}

#[test]
fn file_explicit_absent_false() {
    assert_eq!(
        desired("file", r#"{"absent": false, "content": "hi"}"#).unwrap(),
        desired("file", r#"{"content": "hi"}"#).unwrap()
    );
}

#[test]
fn file_round_trips_both_variants() {
    assert_round_trips(&Desired::File(File::Present {
        content: "hi\n".into(),
    }));
    assert_round_trips(&Desired::File(File::Absent));
}

#[test]
fn plan_file_update_with_line_counts() {
    let plan = vec![
        change(
            Kind::File,
            "/etc/caddy/Caddyfile",
            Action::Update,
            Some("+2 −1 lines"),
        ),
        change(Kind::Pkg, "nginx", Action::Remove, None),
    ];

    assert_round_trips(&plan);
}

#[test]
fn report_package_install_fails_midway() {
    assert_round_trips(&failed_install());
}

#[test]
fn report_clean_run() {
    let report = round_trip(&ApplyReport {
        applied: vec![change(Kind::Pkg, "htop", Action::Create, None)],
        failed: None,
        skipped: vec![],
    });

    assert_eq!(report.failed, None);
    assert_eq!(report.skipped, []);
}

#[test]
fn message_each_round_trips() {
    let catalog = vec![Resource {
        id: "htop".into(),
        desired: Desired::Pkg(Pkg { absent: false }),
    }];
    let plan = vec![change(Kind::Pkg, "htop", Action::Create, None)];

    assert_round_trips(&CliMessage::Catalog(catalog));
    assert_round_trips(&CliMessage::Apply);
    assert_round_trips(&AgentMessage::Facts(ubuntu()));
    assert_round_trips(&AgentMessage::Plan(plan));
    assert_round_trips(&AgentMessage::Report(failed_install()));
    assert_eq!(
        serde_json::to_string(&CliMessage::Apply).unwrap(),
        r#""apply""#
    );
}

#[test]
fn message_unknown_is_rejected() {
    let error = serde_json::from_str::<CliMessage>(r#""shutdown""#).unwrap_err();

    assert!(error.to_string().contains("shutdown"), "{error}");
}
