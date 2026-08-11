# Graphical-session evidence relays

The host writes a JSON job containing only a full exact lowercase Git object ID,
generated run ID, and one allowlisted mode:

- `native-smoke`
- `optional-validation`

Each relay validates that schema, checks out the requested commit, and invokes
only the repository-owned entry point for that mode. Jobs cannot supply shell
commands, environment variables, paths, or test arguments.

Install the relay once in the dedicated lab account:

```sh
# Linux Xorg guest
scripts/vm-evidence/relay/install-linux.sh \
  "$HOME/vm-evidence-spool" "$HOME/src/fesTerm-evidence" \
  "https://github.com/fes/fesTerm.git"

# macOS console guest
scripts/vm-evidence/relay/install-macos.sh \
  "$HOME/vm-evidence-spool" "$HOME/src/fesTerm-evidence" \
  "https://github.com/fes/fesTerm.git"

# Windows graphical guest, in PowerShell 7
scripts/vm-evidence/relay/install-windows.ps1 `
  -Spool "$HOME\vm-evidence-spool" `
  -Repository "$HOME\src\fesTerm-evidence" `
  -RepositoryUrl "https://github.com/fes/fesTerm.git"
```

The Linux relay is intentionally Xorg-only for qualifying runs. Wayland must
remain a separately identified target. The Linux path unit and macOS
`WatchPaths` rerun their relay when a job arrives. The Windows host controller
starts the interactive scheduled task after copying each job. The Windows relay
can automate session and CPU-rendered diagnostics, but its Parallels
Windows-on-ARM result remains `diagnostic`, not accelerated-WGPU acceptance
evidence.
