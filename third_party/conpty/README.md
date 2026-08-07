# Pinned Windows ConPTY Runtime

`manifest.json` pins the reviewed Microsoft package archive and SHA-512 hashes
for every deployed native file. It is not a user-selectable runtime.

## Install layout

An installer may opt into the pinned runtime by placing exactly the
architecture-matched pair beside the installed executable:

```text
fesTerm.exe
runtime/
  conpty/
    win-x64/
      conpty.dll
      x64/
        OpenConsole.exe
```

Use `win-x86`/`x86` or `win-arm64`/`arm64` for those executable targets. The
installer must own this directory and preserve the relative layout; no
configuration, current directory, `PATH`, or environment variable can select a
runtime.

At the first Windows local-PTY start, fesTerm verifies SHA-512 for both files,
sets the process DLL search to System32, and loads the verified `conpty.dll`
through its absolute path before `portable-pty` opens ConPTY. If either file is
missing or has a mismatched hash, it safely uses inbox Kernel32 ConPTY instead.
An already loaded unverified `conpty.dll` is a startup error, not a fallback.

## Updating

The archive hash is verified before CI stages files. Any package update must
update the archive and file hashes in `manifest.json`, review the license, and
run the inbox fallback plus pinned native resize smoke flows.

## Smoke staging

On Windows, use the checked-in staging command rather than copying package
files manually:

```powershell
pwsh -NoProfile -File scripts\stage-conpty.ps1
pwsh -NoProfile -File scripts\stage-conpty.ps1 -RunSmoke
```

The script reads this manifest, caches the exact package under the current
user's local application-data directory (never in the repository), verifies
the archive and extracted x64 files, builds the workspace, and stages the
documented layout below both `target\debug` and `target\debug\deps`.
`-RunSmoke` additionally runs the pinned content-continuity ConPTY retention
smoke. If Windows Application Control blocks the staged native runtime, record
the policy failure; do not bypass the policy or substitute an unverified DLL.
