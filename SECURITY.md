# Security policy

## Supported versions

fettle has no release yet. Only the `main` branch is supported until the first release.

## Reporting a vulnerability

Please do not report vulnerabilities in public issues. Use GitHub private vulnerability reporting: open the repository's [Security tab](https://github.com/ErickJ3/fettle/security) and choose "Report a vulnerability". You can expect a first response within 7 days.

## Trust model

- fettle trusts the operator's machine and its `ssh` client. It connects with the system `ssh`, using `~/.ssh/config` and ssh-agent as they are.
- fettle opens no port and runs no daemon. The CLI and the agent talk over the stdin/stdout of the SSH session.
- On each run, fettle uploads `fettle-agent`, a static binary cached on the target by hash, which runs with the privileges of the SSH user (usually root) and exits when the run ends.
- The agent writes its state to `/var/lib/fettle/state.json` on the target.
- `fettle.ts` is evaluated on the operator's machine; user code never runs on the target.

## In scope

- Integrity of the agent upload and of the hash-based cache on the target.
- The CLI-agent protocol over stdin/stdout.
- The `exec` resource.
- Later: templates, secrets, and resource plugins distributed by URL with a hash.
