# oh-my-setting (OMS)

**oh-my-setting (`oms`)** is a native Rust multi-agent harness. It lets
**Codex**, **Claude Code** and **Antigravity** share project rules, task plans,
isolated worker worktrees, review gates and verified landings.

Source of truth: this wiki summarizes the [repository docs](https://github.com/eightmm/oh-my-setting/blob/main/docs/README.md), and the docs win if they differ.

## Hosts

- **Linux** (glibc, Ubuntu 20.04+): Bash 5.0+ and `curl`.
- **macOS**: Bash 5.0+ (`brew install bash`), `curl` and the Command Line Tools.
- **Windows**: use WSL2. Native Windows is retired and refused.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh | bash
```

After you install:

- `oms` opens the terminal panel.
- `oms list` shows the everyday commands.
- `oms doctor` checks the wiring.
- `oms update` updates the installation.
- `oms uninstall` restores the configuration files that were backed up.

## Documentation

- [README](https://github.com/eightmm/oh-my-setting/blob/main/README.md)
- [Documentation map](https://github.com/eightmm/oh-my-setting/blob/main/docs/README.md)
- [Install, update and uninstall](https://github.com/eightmm/oh-my-setting/blob/main/docs/INSTALL.md)
- [Command catalog](https://github.com/eightmm/oh-my-setting/blob/main/docs/COMPONENTS.md)
- [Terminal panel](https://github.com/eightmm/oh-my-setting/blob/main/docs/TERMINAL-PANEL.md)
- [Verification and release](https://github.com/eightmm/oh-my-setting/blob/main/docs/COMPONENTS.md#verification-and-release)
- [Contributing](https://github.com/eightmm/oh-my-setting/blob/main/CONTRIBUTING.md)
- [GitHub workflow](https://github.com/eightmm/oh-my-setting/wiki/GitHub-Workflow)
