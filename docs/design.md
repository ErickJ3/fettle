# fettle design

## Overview

fettle is a configuration management tool that puts Linux machines in exactly the state described in code: it shows the diff first, applies only the difference, and undoes what was removed. Think of it as "Terraform for the inside of the machine", with typed code and no permanent agent.

The name comes from British English: *to fettle* is to fix up or put in order; to be *in fine fettle* is to be in great shape.

**The problem.** Tools that run a list of tasks over SSH are easy to adopt, but they keep no record of what they manage: idempotency depends on each task, and deleting a task does not undo what it did. The alternatives built on desired state (immutable images, NixOS, Kubernetes) solve this, but they require switching operating systems or mental models, and prebuilt images go stale.

**Who it is for.** Anyone who runs long-lived Linux servers, from a company's production hosts to a single VPS or a homelab, and wants to configure ordinary Debian, Ubuntu or Fedora machines quickly, declaratively and predictably, without adopting a whole platform.

## Principles

1. **Real code.** Configuration is typed TypeScript: autocomplete, functions, modules, and errors in the editor instead of on host 37 of 50.
2. **Plan before apply.** Every apply starts by inspecting the real state and showing the exact diff. Nothing changes without appearing in the plan.
3. **Removing it from the code removes it from the machine.** fettle keeps track of what it manages; deleting a package or file from the config undoes it on the next run.
4. **Idempotency belongs to the framework.** Every resource implements the same interface (read, compare, apply). A bare command only exists if it declares what it creates.
5. **A graph, not a list.** Explicit dependencies form a graph; independent work runs in parallel, and changes propagate restarts on their own.
6. **Fast by default.** One SSH connection per host, with the work done on the target by a static binary, without relying on the target's Python.
7. **Simple to start.** One file, one command. No central server, database or account to create.
8. **It manages only what it declared.** fettle does not try to control the whole machine; what it did not create, it does not touch.

## Example

A homelab with one server running Caddy and Docker services fits in a single `fettle.ts`:

```ts
import { host, pkg, file, service, user, docker } from "fettle";

export default host("nas", { ssh: "root@192.168.1.10" }, () => {
  user("marcos", { groups: ["docker"], sshKeys: ["~/.ssh/id_ed25519.pub"] });

  pkg(["htop", "tmux", "caddy"]);
  docker.engine();

  const caddyfile = file("/etc/caddy/Caddyfile", {
    template: "./caddy/Caddyfile",
    vars: { domain: "casa.lan" },
  });

  service("caddy", { enabled: true, restartOn: [caddyfile] });

  docker.compose("immich", { source: "./stacks/immich" });
});
```

Running `fettle plan` would print something like:

```
nas (192.168.1.10)
  + pkg       caddy
  ~ file      /etc/caddy/Caddyfile   (+2 −1 lines)
  ↻ service   caddy                  (restart: Caddyfile changed)
  - pkg       nginx                  (removed from config)

3 changes, 1 removal. Run `fettle apply` to apply.
```

`restartOn` plays the role of Ansible handlers as an edge in the graph: the service restarts when a resource it depends on changes.

The same file can describe several hosts and host groups (`host("nas")`, `group("pis")`), which fettle configures in parallel.

## Execution model

Every run follows the same path, from local code to the host:

1. **Evaluate.** fettle runs `fettle.ts` locally and produces a catalog: the list of desired resources per host, with their dependencies. No user code runs on the server.
2. **Connect.** It opens one SSH connection per host and sends `fettle-agent`, a static binary (~5 MB) for the target's architecture, cached by hash. The agent lives only for the duration of the run.
3. **Read.** The agent queries the real state of every resource in the catalog and of every resource the state says was managed before.
4. **Plan.** The diff between desired, real and recorded state becomes the plan: create, update, restart, remove. `fettle plan` stops here.
5. **Apply.** The agent walks the graph in topological order, in parallel where there are no dependencies, and stops a failing branch without bringing down the rest.
6. **Record.** The state is updated with what was applied.

**State.** It lives on the host itself, in `/var/lib/fettle/state.json`: the list of managed resources and a hash of what was applied. There is no remote backend to configure, and each machine knows what fettle put on it. The state is used for removal and drift detection, never as the source of truth: the real state is always read again.

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

Because the engine keeps resources of different types in the same graph, a blanket impl turns any `Resource` into a version with its `State` serialized as `serde_json::Value`, which the graph stores as `Box<dyn AnyResource>`. Authors of a new resource only implement the typed trait.

**Built-in MVP resources:** `pkg` (apt, dnf, apk), `file` and `template`, `dir`, `symlink`, `user` and `group`, `service` (systemd), `cron`/`timer`, `docker.engine`, `docker.compose`, `firewall` (nftables) and `git` (clone a repository at a given version).

Templates take variables from the configuration, and later secrets from `age`/`sops` and environment variables.

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
| `fettle plan [host]` | Shows the diff without changing anything |
| `fettle apply [host]` | Shows the plan, asks for confirmation and applies (`-y` skips the confirmation) |
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
| Execution | Local binary, parallel by graph | Task by task over SSH | Commands over SSH | Provider API | Build + switch |
| Target requirement | SSH | SSH + Python | SSH | Not applicable | Being NixOS |
| Configures inside the machine | Yes | Yes | Yes | No | Yes |

Puppet and Salt also have a catalog and a graph, but they require a permanent agent and a central server, while fettle needs only SSH on the target.

## Ansible compatibility

The biggest barrier to switching tools is rewriting everything before seeing any benefit. uv solved this by understanding the `requirements.txt` everyone already had. fettle can do the same: read the inventory and playbooks people already have and run them faster, with a real plan, in the first minute.

**How it works.** Compatibility is just another frontend. Instead of evaluating `fettle.ts`, fettle reads the Ansible YAML and produces the same catalog. From there on, the engine, agent, graph and state are the same, and handlers become `restartOn`.

**Commands.**

- `fettle ansible check site.yml`: a coverage report showing how many tasks fettle understands and which modules are missing. It is the five-minute test for newcomers and the metric that decides what to implement next.
- `fettle ansible plan site.yml -i hosts.ini` and `fettle ansible apply`: run the playbook through the fettle engine.
- `fettle import site.yml`: converts the playbook into `fettle.ts`. It is for those who want to keep their configuration in TypeScript.

**Scope.** Only what covers most homelab playbooks: the most used `ansible.builtin` modules (apt, dnf, package, copy, template, file, lineinfile, user, group, service, systemd, cron, git, get_url, command and shell), INI and YAML inventories, `group_vars` and `host_vars`, local roles, `when`, `loop` and variables through minijinja, and a small set of facts (distribution, architecture, hostname). Third-party collections, network modules, Windows and Python plugins are out of scope.

**Where the Ansible model differs.**

- *Removal.* People coming from Ansible do not expect deleting a task to undo anything. In compatibility mode, whatever leaves the playbook shows up in the plan as an orphan and is only removed with `--prune`.
- *Bare commands.* `command` and `shell` without `creates` or `removes` run, because the playbook has to work, but `check` flags each one as non-idempotent and the plan shows that they always run. It shows which tasks need a guard.

**When.** It depends on the resources and the graph from v0.2. The public launch should come with this layer, because it is what turns "yet another new tool" into "my playbook, only faster and with a plan".

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
| Templates and variables | minijinja | Fast, no Python, and compatible with much of Jinja2, which helps the Ansible layer |
| Graph | petgraph | Topological ordering and cycle detection out of the box |
| Concurrency | tokio | One task per host in the CLI; independent graph branches in parallel in the agent |
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
- **Ansible support is another frontend.** It produces the same catalog as `fettle.ts`; the engine, agent, graph and state do not change.

## Open questions

- Is running Ansible YAML supported forever, or is it only a bridge to `fettle import`? Permanent support widens the audience but ties the roadmap to Ansible's behavior.
- Is state on the host enough, or is a local copy worth keeping to know what existed on a machine that was reinstalled?
- How should existing things be imported: a `fettle adopt` that starts managing files and packages already present without recreating them?
