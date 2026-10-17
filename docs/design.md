# fettle design

## Overview

fettle is a configuration management tool that puts Linux machines in exactly the state described in code: it shows the diff first, applies only the difference, and undoes what was removed. Think of it as "Terraform for the inside of the machine", with typed code and no permanent agent.

The name comes from British English: *to fettle* is to fix up or put in order; to be *in fine fettle* is to be in great shape.

**The problem.** Tools that run a list of tasks over SSH are easy to adopt, but they keep no record of what they manage: idempotency depends on each task, and deleting a task does not undo what it did. The alternatives built on desired state (immutable images, NixOS, Kubernetes) solve this, but they require switching operating systems or mental models, and prebuilt images go stale.

**Who it is for.** Anyone who runs long-lived Linux servers, from a company's production hosts to a single VPS or a homelab, and wants to configure ordinary Debian, Ubuntu or Fedora machines quickly, declaratively and predictably, without adopting a whole platform.

## Principles

1. **Real code.** Configuration is typed TypeScript: autocomplete, functions, modules, and errors in the editor instead of on host 37 of 50.
2. **Plan before apply.** Every apply starts by inspecting the real state and showing the exact diff. Nothing changes without appearing in the plan.
3. **Removing it from the code removes it from the machine.** fettle keeps track of what it manages; deleting a package or file it created from the config undoes it on the next run. What already existed before fettle managed it is forgotten instead, never removed.
4. **Idempotency belongs to the framework.** Every resource implements the same interface (read, compare, apply). A bare command only exists if it declares what it creates.
5. **Reads in parallel, writes in order.** The agent reads every resource at once, then applies changes one at a time in the order they appear in `fettle.ts`. The order you read is the order that runs, and hosts still run in parallel.
6. **Fast by default.** One SSH connection per host, with the work done on the target by a static binary, without relying on the target's Python.
7. **Simple to start.** One file, one command. No central server, database or account to create.
8. **It manages only what it declared.** fettle does not try to control the whole machine; what is not declared, it does not touch.

## Example

A homelab with one server running Caddy and Docker services fits in a single `fettle.ts`:

```ts
import { host, pkg, file, service, user, docker, read, template, local } from "fettle";

host("nas", { ssh: "root@192.168.1.10" }, () => {
  docker.engine();
  user("marcos", { groups: ["docker"], sshKeys: [read("~/.ssh/id_ed25519.pub")] });

  pkg(["htop", "tmux", "caddy"]);
  const caddyfile = file("/etc/caddy/Caddyfile", {
    content: template("./caddy/Caddyfile", { domain: "casa.lan" }),
  });
  service("caddy", { enabled: true, reloadOn: [caddyfile] });

  docker.compose("immich", { source: local("./stacks/immich") });
});
```

Running `fettle plan` would print something like:

```
nas (192.168.1.10)
  + pkg       caddy
  ~ file      /etc/caddy/Caddyfile   (+2 −1 lines)
  ↻ service   caddy                  (reload: Caddyfile changed)
  - pkg       nginx                  (removed from config)

3 changes, 1 removal. Run `fettle apply` to apply.
```

`restartOn` and `reloadOn` play the role of Ansible handlers: the service restarts or reloads, once, when a resource declared before it changes in the same run.

The order of the file is the order of the run: `docker.engine()` comes before `user` because the user joins the `docker` group that the engine creates.

The same file can describe many hosts, declared in a loop and selected by name or `tags`, which fettle configures in parallel.

## Configuration language

`fettle.ts` is an ordinary TypeScript module. fettle evaluates it on the operator's machine into a catalog: the final desired resources of each host.

**Hosts register themselves.** Each top-level `host(name, options?, callback)` call registers one host, so a loop declares many. fettle does not read the module's exports. Two hosts with the same name fail evaluation, naming both source locations.

```ts
for (const name of ["pi1", "pi2", "pi3"]) {
  host(name, { tags: ["pis"] }, () => { pkg("htop"); });
}
```

**SSH destination defaults to the host name.** Without an `ssh` option, fettle runs the system `ssh` with the host name as the destination, so `~/.ssh/config` aliases apply. `ssh: "root@192.168.1.10"` replaces the destination.

**Targets select hosts by name or tag.** `fettle plan pis` selects every host named `pis` or whose `tags` contain `pis`. Without a target, every host is selected. A target that selects nothing fails before any connection.

**Resources belong to the host callback that declares them.** While a host callback runs, every resource function appends to that host, including calls from plain functions the callback invokes. A role is just a function:

```ts
function caddy() {
  pkg("caddy");
  service("caddy", { enabled: true });
}
host("nas", () => { caddy(); });
```

A resource called outside a host callback, or `host()` called inside one, fails evaluation with its source location.

**Host callbacks receive facts.** fettle reads the host's facts before running its callback and passes them as the first argument: distribution family, distribution id and version, architecture and hostname.

```ts
host("web", (h) => {
  pkg(h.os.family === "debian" ? "apache2" : "httpd");
});
```

**Host callbacks are synchronous.** A callback that returns a promise or any other thenable fails evaluation naming the host. Every helper is synchronous, so nothing in a callback needs `await`.

**Resource identity is kind and id.** A resource is identified by its kind and first argument, such as `pkg htop` or `file /etc/motd`, and that identity is its key in the catalog and in the host state. A list of ids declares one resource per id: `pkg(["htop", "tmux"])` is `pkg htop` and `pkg tmux`, so removing `tmux` from the list removes only that package.

**Duplicate resources merge or conflict.** Two declarations with the same identity on one host merge when their props are equal, at the position of the first. When the props differ, such as `pkg("nginx")` and `pkg("nginx", { absent: true })`, evaluation fails naming the identity and both source locations.

**Strings are values, local files go through helpers.** A string in resource props is a literal or a path on the target, never a path on the operator's machine. Local files enter only through three helpers, resolved relative to `fettle.ts` with `~` as the operator's home:

- `read(path)` returns the text of a local file.
- `template(path, vars)` returns rendered text.
- `local(path)` refers to a local file or directory that fettle uploads as is.

`sshKeys: [read("~/.ssh/id_ed25519.pub")]` puts the key text in the catalog; `sshKeys: ["~/.ssh/id_ed25519.pub"]` fails because that string is not a public key. A missing local file fails evaluation naming the resolved path and the call's source location.

**Templates render on the operator's machine.** `template(path, vars)` renders with minijinja during evaluation, so the catalog holds the final content and the target never renders anything. A template error, such as an undefined variable, fails evaluation naming the template path.

**Trigger references point backward on the same host.** `restartOn` and `reloadOn` accept only values returned by resource functions declared earlier on the same host. A reference to a later resource, or to another host's, fails evaluation pointing to the entry.

**Props are validated without TypeScript checking.** Types are stripped, not checked, so fettle validates every resource's props while it builds the catalog: `service("caddy", { enabled: "yes" })` fails naming `service caddy`, the `enabled` prop and the source location. The type declarations that `fettle init` writes for the editor are generated from the same protocol types, so they accept exactly what validation accepts.

**Absent resources.** `absent: true` declares that something must not exist: fettle removes it whoever created it, and dropping the declaration later does not bring it back.

## Execution model

Every run follows the same path, from local code to the host:

1. **Evaluate.** fettle evaluates `fettle.ts` once, locally, and the top-level `host()` calls register the hosts. Syntax and top-level errors fail here, before any connection.
2. **Connect.** It opens one SSH connection per selected host and sends `fettle-agent`, a static binary (~5 MB) for the target's architecture, cached by hash. The agent lives only for the duration of the run.
3. **Facts.** The agent reports the host's facts: distribution family, id and version, architecture and hostname.
4. **Build the catalog.** fettle runs each host's callback on the operator's machine with its facts and validates the result into that host's catalog. No user code runs on the server.
5. **Read.** The agent queries, in parallel, the real state of every resource in the catalog and of every resource the state says was managed before. Nothing is written before the plan is complete.
6. **Plan.** The diff between desired, real and recorded state becomes the plan: create, update, restart, reload, remove, forget. `fettle plan` stops here.
7. **Apply.** The agent applies changes one resource at a time in source order, comparing each against the state left by the writes before it and reporting the change it actually made. A service with `restartOn` or `reloadOn` restarts or reloads at its own position, at most once. A failed write stops that host: the remaining writes are skipped and reported, and the run exits non-zero. Other hosts carry on.
8. **Record.** The state is updated with every write that was applied, including those before a failure.

**State.** It lives on the host itself, in `/var/lib/fettle/state.json`: the list of managed resources, keyed by kind and id, and a hash of what was applied. Each entry has an `origin`: `created` when fettle found nothing there, `preexisting` when the resource already existed. A dropped `created` resource is removed; a dropped `preexisting` one is forgotten, so fettle never deletes a file it did not write first or uninstalls a package it did not install. Resources declared with `absent: true` are removed if present and are not kept in the state. There is no remote backend to configure, and each machine knows what fettle put on it. The state is used for removal and drift detection, never as the source of truth: the real state is always read again.

**Drift.** If someone edits a managed file by hand, the next `plan` shows the difference. `fettle drift` runs only the read step and can go in a cron job.

## Resources

Every resource, built-in or from the community, implements the same interface in the agent. This is what guarantees idempotency and removal for everything:

```rust
pub trait Resource {
    /// Observable state (what `read` returns and the state file stores)
    type State: Serialize + DeserializeOwned;

    /// What it looks like now (`None` = does not exist)
    fn read(&self, ctx: &Ctx) -> Result<Option<Self::State>>;
    /// What needs to change; `self` is the desired state
    fn diff(&self, have: Option<&Self::State>) -> Vec<Change>;
    /// Applies only the difference
    fn apply(&self, ctx: &Ctx, changes: &[Change]) -> Result<()>;
    /// Undoes the resource when it leaves the config
    fn remove(&self, ctx: &Ctx, last: &Self::State) -> Result<()>;
}
```

Because the agent keeps resources of different types in the same ordered list, a blanket impl turns any `Resource` into a version with its `State` serialized as `serde_json::Value`, which the list stores as `Box<dyn AnyResource>`. Authors of a new resource only implement the typed trait.

**Built-in MVP resources:** `pkg` (apt, dnf, apk), `file` (with `content` or `source`), `dir`, `symlink`, `user` and `group`, `service` (systemd), `cron`/`timer`, `docker.compose`, `firewall` (nftables) and `git` (clone a repository at a given version).

**Helpers.** `read`, `template` and `local` are not resources: they run during evaluation and turn local files into values. `read` returns text, `template` returns text rendered by minijinja, and `local` returns a reference to a file or directory whose bytes travel with the catalog. The agent never renders a template or reads the operator's files, so the plan diff is exact. Template variables come from the configuration, and later secrets from `age`/`sops` and environment variables, through a `secret()` value that the plan prints redacted.

**Standard library.** Composites that are only a bundle of primitives, such as `docker.engine()` (package, service and group), are TypeScript functions bundled with the CLI. They expand into primitive resources in the catalog, so the agent needs no code for them. A resource is written in Rust only when it needs its own read, as `docker.compose` does to query the running stack.

**Arbitrary commands.** They exist, but they must say when the work is already done:

```ts
exec("install-tailscale", {
  run: "curl -fsSL https://tailscale.com/install.sh | sh",
  creates: "/usr/bin/tailscale",
});
```

Without `creates`, `unless` or `onlyIf`, fettle rejects the resource. This keeps every resource idempotent, commands included.

**Extensions.** New resources are binaries or WASM plugins that implement the interface, distributed by URL with a hash, with no central registry at first.

## CLI

| Command | What it does |
| --- | --- |
| `fettle init` | Creates an example `fettle.ts` and the types for the editor |
| `fettle plan [target]` | Shows the diff without changing anything; a target is a host name or a tag |
| `fettle apply [target]` | Shows the plan, asks for confirmation and applies (`-y` skips the confirmation) |
| `fettle drift` | Only reads and reports what changed outside fettle; exits non-zero if there is drift |
| `fettle run <host> -- <cmd>` | Ad-hoc command on one or more hosts, like `ansible -m shell` |
| `fettle state <host>` | Lists what fettle manages on that host |
| `fettle forget <resource>` | Stops managing a resource without removing it from the machine |
| `fettle test` | Applies the config to a throwaway container or VM to validate it |

## Comparison

fettle brings the plan-and-remove model of Terraform and NixOS to ordinary distributions over SSH, the way Ansible and pyinfra work.

|  | fettle | Ansible | pyinfra | Terraform | NixOS |
| --- | --- | --- | --- | --- | --- |
| Language | TypeScript | YAML + Jinja | Python | HCL | Nix |
| Reliable plan with diff | Yes | Check mode (`--check`) | Yes | Yes | Yes (build) |
| Removes what left the code | Yes | No | No | Yes | Yes |
| State | On the host | None | None | Remote/local | System generations |
| Execution | Local binary, parallel reads, writes in source order | Task by task over SSH | Commands over SSH | Provider API | Build + switch |
| Target requirement | SSH | SSH + Python | SSH | Not applicable | Being NixOS |
| Configures inside the machine | Yes | Yes | Yes | No | Yes |

Puppet and Salt also have a catalog and a graph, but they require a permanent agent and a central server, while fettle needs only SSH on the target.

## Ansible compatibility

The biggest barrier to switching tools is rewriting everything before seeing any benefit. uv solved this by understanding the `requirements.txt` everyone already had. fettle can do the same: read the inventory and playbooks people already have and run them faster, with a real plan, in the first minute.

**How it works.** Compatibility is just another frontend. Instead of evaluating `fettle.ts`, fettle reads the Ansible YAML and produces the same catalog. From there on, the engine, agent and state are the same, and handlers become `restartOn` or `reloadOn` on a service placed after the tasks that notify it.

**Commands.**

- `fettle ansible check site.yml`: a coverage report showing how many tasks fettle understands and which modules are missing. It is the five-minute test for newcomers and the metric that decides what to implement next.
- `fettle ansible plan site.yml -i hosts.ini` and `fettle ansible apply`: run the playbook through the fettle engine.
- `fettle import site.yml`: converts the playbook into `fettle.ts`. It is for those who want to keep their configuration in TypeScript.

**Scope.** Only what covers most homelab playbooks: the most used `ansible.builtin` modules (apt, dnf, package, copy, template, file, lineinfile, user, group, service, systemd, cron, git, get_url, command and shell), INI and YAML inventories, `group_vars` and `host_vars`, local roles, `when`, `loop` and variables through minijinja, and a small set of facts (distribution, architecture, hostname). Third-party collections, network modules, Windows and Python plugins are out of scope.

**Where the Ansible model differs.**

- *Removal.* People coming from Ansible do not expect deleting a task to undo anything. In compatibility mode, whatever leaves the playbook shows up in the plan as an orphan and is only removed with `--prune`.
- *Bare commands.* `command` and `shell` without `creates` or `removes` run, because the playbook has to work, but `check` flags each one as non-idempotent and the plan shows that they always run. It shows which tasks need a guard.

**When.** It depends on the resources and triggers from v0.2. The public launch should come with this layer, because it is what turns "yet another new tool" into "my playbook, only faster and with a plan".

## Non-goals

- **Provisioning infrastructure.** Creating VMs, DNS records or buckets is the job of Terraform/OpenTofu. fettle starts when a host with SSH already exists.
- **Controlling the whole machine.** Unlike NixOS, whatever is not declared stays as it is.
- **Orchestrating containers.** fettle brings up a `compose` stack and makes sure it is running; scheduling and failover belong to Kubernetes or Nomad.
- **Central server, web UI, RBAC.** No "fettle Tower" at first. Large teams can run it in CI.
- **Windows and network devices.** Linux with systemd first; the rest only if the resource interface proves it can handle them.

## Roadmap

The MVP proves the full cycle on a single host: declare, see the plan, apply and remove. The milestones after it are in [ROADMAP.md](../ROADMAP.md).

## Design decisions

### Implementation

| Part | Choice | Why |
| --- | --- | --- |
| Evaluating `fettle.ts` | oxc to strip types + rquickjs (embedded QuickJS) | Runs TypeScript without requiring Node, Bun or Deno; everything fits in the CLI binary |
| Templates and variables | minijinja, in the CLI | Fast, no Python, and compatible with much of Jinja2, which helps the Ansible layer; rendering before the catalog keeps the plan exact |
| Concurrency | tokio | One task per host in the CLI; parallel reads in the agent |
| SSH | `openssh` crate, using the system `ssh` | Reuses `~/.ssh/config`, ssh-agent and ControlMaster; russh is still less mature |
| Agent | Static musl binary, built with cargo-zigbuild | One file per architecture (amd64, arm64), with no dependency on the target |
| CLI ↔ agent protocol | serde over the SSH session's stdin/stdout | No extra port or daemon: the catalog goes out, the plan and the result come back |
| Plugins | wasmtime | Community resources isolated, distributed by URL with a hash |

### Crate layout

```
  fettle (CLI)                       fettle-agent (target)
  evaluates fettle.ts, SSH           Resource trait + impls
        |                                   |
        +---------> fettle-protocol <-------+
             catalog, plan, CLI<->agent messages
```

- `fettle`: the CLI that runs on the operator's machine. It evaluates `fettle.ts`, opens the SSH connections and prints the plan.
- `fettle-agent`: the binary that runs on the target during a run. It holds the `Resource` trait and every resource implementation.
- `fettle-protocol`: the serializable data both sides share: the desired-state types the CLI validates and puts in the catalog, the plan, and the messages between CLI and agent.

The `Resource` trait lives in the agent, not in a crate shared with the CLI, because the CLI never needs to link apt or systemd code: the only surface the two sides share is serializable data, which is exactly the protocol. Crates are named for their responsibility; there are no generic `core`, `common` or `utils` crates.

### Other decisions

- **State lives on the host.** No remote backend to configure; each machine knows what fettle put on it.
- **No permanent agent.** The agent is uploaded, cached by hash, and exits when the run ends. fettle opens no port and runs no daemon.
- **`exec` requires a guard.** A command without `creates`, `unless` or `onlyIf` is rejected, so idempotency holds for every resource.
- **Reads in parallel, writes in source order.** Explicit edges let a forgotten one become an intermittent race, and inferred edges hide the ordering from the reader; Puppet itself made manifest order its default. Writes are few per run and apt is serialized by the dpkg lock anyway, so the time lost is small, and the reads, where the time goes, stay parallel. Trigger references only point backward, so there is no cycle to detect.
- **Callbacks run after facts.** The module is evaluated once to register hosts, and each host's callback runs once its facts are known. Declaring the OS in host options would duplicate it in every config, and the Ansible frontend needs facts anyway; values resolved on the agent would make the plan inexact.
- **Values in the catalog, local files through helpers.** `read`, `template` and `local` resolve local files during evaluation, so the agent never reads the operator's files or renders templates and the plan shows the real final content. A plain string is never a local path, so a typo cannot silently upload the wrong file.
- **Removal undoes only what fettle created.** Each state entry records whether fettle created the resource or found it there. Removing something that existed before fettle would break machines that adopt it gradually; `absent: true` covers the cases where removal is wanted.
- **Ansible support is another frontend.** It produces the same catalog as `fettle.ts`; the engine, agent and state do not change.

## Open questions

- Is running Ansible YAML supported forever, or is it only a bridge to `fettle import`? Permanent support widens the audience but ties the roadmap to Ansible's behavior.
- Is state on the host enough, or is a local copy worth keeping to know what existed on a machine that was reinstalled?
- Should a host offer opt-in parallel writes, for example a `parallel(() => { ... })` block, and for which resources?
- Which generator produces the editor's `.d.ts` from the protocol types?
- Does `docker.engine()` install the distribution's package or Docker's own repository, which would need an apt repository resource?
- Should the plan group the primitive resources of a composite under the helper that declared them?
