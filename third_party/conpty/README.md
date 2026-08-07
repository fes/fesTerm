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
