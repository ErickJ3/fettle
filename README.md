# fettle

Declarative configuration for Linux machines: see the diff, apply only the difference, undo what you remove.

> [!WARNING]
> fettle is pre-alpha and not usable yet. The example below shows the target syntax; `reloadOn`, `template`, `local` and `docker.*` arrive in v0.2. See the [roadmap](ROADMAP.md).

## Why

You describe each machine in typed TypeScript. fettle reads its real state, shows exactly what will change and applies only that. Remove a package or file that fettle put there from the config and the next run removes it from the machine. Think of it as Terraform for the inside of the machine.

fettle is for anyone who runs long-lived Linux servers, from a company's production hosts to a single VPS or a homelab. It configures ordinary Debian, Ubuntu or Fedora machines declaratively, with fast runs and predictable results, without switching operating systems or adopting a whole platform.

## Example

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

```console
$ fettle plan
nas (192.168.1.10)
  + pkg       caddy
  ~ file      /etc/caddy/Caddyfile   (+2 −1 lines)
  ↻ service   caddy                  (reload: Caddyfile changed)
  - pkg       nginx                  (removed from config)

3 changes, 1 removal. Run `fettle apply` to apply.
```

## How it works

1. fettle evaluates `fettle.ts` on your machine to find the hosts. Your code never runs on the target.
2. It opens one SSH connection per host and starts a short-lived static agent. Nothing needs to be installed on the target besides SSH.
3. The agent reports the host's facts (distribution, architecture, hostname), and fettle runs that host's callback with them to build its catalog of desired resources.
4. The agent reads the real state and compares it with the catalog and with what fettle managed before. `fettle plan` stops here.
5. `fettle apply` applies only the difference, one resource at a time in the order of `fettle.ts`, and records what it manages in `/var/lib/fettle/state.json` on the host.

## Coming from Ansible?

v0.4 will read your existing inventories and playbooks and run them through the same engine, so you get a plan for the configuration you already have before rewriting anything. `fettle import` will convert a playbook into `fettle.ts` when you want to move. See the [roadmap](ROADMAP.md).

## Learn more

- [Roadmap](ROADMAP.md)
- [Design](docs/design.md)
- [Contributing](CONTRIBUTING.md)

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
