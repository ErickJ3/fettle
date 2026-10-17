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

- [ ] Evaluate `fettle.ts` locally into a catalog
- [ ] Static agent for Linux amd64 and arm64, shipped over SSH
- [ ] Resources: `pkg` (apt), `file`, `service` (systemd) and `user`
- [ ] `fettle plan` and `fettle apply`, with state kept on the host
- [ ] Resources removed from the config are removed from the machine

## v0.2 - Multi-host orchestration

Goal: configure several hosts in parallel, with resources ordered by a dependency graph.

- [ ] Parallel runs across hosts, and host groups
- [ ] Parallel execution along the dependency graph
- [ ] Restarts triggered by changes (`restartOn`)
- [ ] Resources: `template`, `exec` with guards, `docker.engine` and `docker.compose`
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
