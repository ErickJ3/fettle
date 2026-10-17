# Roadmap

This roadmap describes the direction of fettle, not commitments: scope and order can change as the design meets real machines. Work is tracked in [GitHub milestones](https://github.com/ErickJ3/fettle/milestones), and the reasoning behind each item is in the [design document](docs/design.md).

## Status

| Milestone | Focus | Status |
| --- | --- | --- |
| [v0.1](https://github.com/ErickJ3/fettle/milestone/1) | [Core engine](#v01---core-engine) | Planned |
| [v0.2](https://github.com/ErickJ3/fettle/milestone/2) | [Multi-host orchestration](#v02---multi-host-orchestration) | Planned |
| [v0.3](https://github.com/ErickJ3/fettle/milestone/3) | [Testing, secrets and extensibility](#v03---testing-secrets-and-extensibility) | Planned |
| [v0.4](https://github.com/ErickJ3/fettle/milestone/4) | [Ansible compatibility](#v04---ansible-compatibility) | Planned |

## v0.1 - Core engine

Goal: the full cycle on a single host: declare, plan, apply and remove.

- [ ] Evaluate `fettle.ts` locally: host registration, `ssh` defaulting to the host name, and host callbacks run with the host's facts
- [ ] Validated catalog: prop validation with source locations, identity by kind and id, duplicate merging, and `read()`
- [ ] Static agent for Linux amd64 and arm64, shipped over SSH, reporting facts
- [ ] Resources: `pkg` (apt), `file`, `service` (systemd) and `user`
- [ ] `fettle plan` and `fettle apply`, with writes in source order, a failed write stopping the host, and state kept on the host
- [ ] Resources fettle created are removed when dropped from the config, pre-existing ones are forgotten, and `absent: true` removes either

## v0.2 - Multi-host orchestration

Goal: configure several hosts in parallel, with parallel reads and service triggers.

- [ ] Parallel runs across hosts, selected by name or `tags`
- [ ] Parallel reads in the agent
- [ ] Restarts and reloads triggered by changes (`restartOn`, `reloadOn`)
- [ ] Helpers: `template()` and `local()`
- [ ] Resources: `exec` with guards and `docker.compose`
- [ ] Standard library: `docker.engine()`
- [ ] Drift detection (`fettle drift`)

## v0.3 - Testing, secrets and extensibility

Goal: validate a configuration before it reaches a real host, and extend fettle beyond the built-in resources.

- [ ] `fettle test` against a throwaway container
- [ ] Secrets from `age`/`sops` and environment variables
- [ ] `pkg` support for dnf and apk
- [ ] Resources provided by plugins

## v0.4 - Ansible compatibility

Goal: adopt fettle without rewriting existing Ansible configuration first. This milestone is the first public release.

- [ ] Read Ansible inventories (INI and YAML), `group_vars` and `host_vars`
- [ ] Map the most used `ansible.builtin` modules to fettle resources
- [ ] Coverage report for existing playbooks (`fettle ansible check`)
- [ ] Plan and apply playbooks through the fettle engine, removing orphans only on request
- [ ] Convert playbooks into `fettle.ts` (`fettle import`)

## Out of scope

Provisioning infrastructure, controlling the whole machine, container scheduling, a central server or web UI, and Windows or network devices are not planned. The [non-goals](docs/design.md#non-goals) explain why.

## Proposing changes

Open a [feature request](https://github.com/ErickJ3/fettle/issues/new?template=feature_request.yml) describing the problem it solves. Changes to the roadmap are discussed there before they land in a milestone.
