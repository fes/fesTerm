# Native terminal capability observations

This opt-in POSIX collector supplements the matched-grid Copilot gate in
`docs/dev-tunnel-workflow-lab.md`. Run it in the actual existing controlled
local or Dev Tunnels foreground PTY before Copilot; keep keyboard input idle.
It requires a private existing output directory, an exclusive new output file,
and the same attached TTY for standard input/output.

```sh
python3 scripts/check_dev_tunnel_workflow.py --terminal-capability-probe --case CASE --result PRIVATE_DIRECTORY/NEW_FILE.json
```

The optional-validation aggregators also provide a dedicated probe dispatch:

```sh
FESTERM_RUN_OPTIONAL_VALIDATION=1 \
FESTERM_RUN_TERMINAL_CAPABILITY_PROBE=1 \
FESTERM_TERMINAL_CAPABILITY_CASE=CASE \
FESTERM_TERMINAL_CAPABILITY_RESULT_PATH=PRIVATE_DIRECTORY/NEW_FILE.json \
sh scripts/run-optional-validation.sh
```

For PowerShell on POSIX, set those same four environment variables and run
`./scripts/run-optional-validation.ps1`. This mode runs only the collector,
inherits terminal handles without redirection and propagates its exit status.
Both a public case identifier and a private new result path are required; no
aggregate summary file is created. Windows PowerShell dispatch exits with
status 2, keeping native Windows console collection an explicit remaining gate.
The normal optional suites remain unchanged when the probe opt-in is absent.

The flag makes no service calls and excludes `--run`. It delegates directly to
`scripts/native_terminal_capability_probe.py` with inherited TTY descriptors.
Windows exits with status 2 for the remaining native-console gate; POSIX results do not
qualify Windows or substitute for a native client/server comparison.

The fixed ten queries are DA1/DA2, DSR cursor position, OSC4 palette indices
0/7/15/255 and OSC10/11/12. No response, mode/palette change or terminal text is
injected. The probe saves terminal attributes and descriptor flags, uses raw
mode without flushing input, and restores state in finally. Handled signals
request cancellation without raising asynchronously. Cleanup blocks SIGINT,
SIGTERM and SIGHUP through all terminal/descriptor restoration and restores the
caller's handlers before its original mask. Pending cancellation is not consumed
or replaced with an ignored handler; the caller's original disposition applies
after state restoration. It refuses readable pre-existing input before changing mode. During
the probe any unrecognized input aborts; only its count is retained. Do not type
while it runs. Unknown input is never replayed.

A run is bounded to 12 seconds, 4 KiB received data and 256 bytes per frame. Only
recognized numeric/color replies are recorded, including duplicate counts.
Replies may arrive fragmented or out of order. A complete result requires one
response to every query, stable actual PTY dimensions and restored terminal
attributes. Timeouts are incomplete observations, never proof of unsupported
capability. Once all ten replies arrive, a 250 ms quiet interval ends observation;
this finite interval cannot rule out responses arriving afterward.

The exact saved terminal attributes are supplied to TCSANOW on restoration.
Verification compares all attributes except PENDIN, the OS-owned pending-input
retype status bit which macOS sets on return to canonical mode; its change is
recorded separately. Echo/canonical flags and control characters remain checked.
The documented mutable descriptor I/O flags (NONBLOCK/APPEND/ASYNC) are checked;
any difference in other kernel descriptor status bits is reported separately.
No input is read or flushed merely to clear those kernel status indicators.

The JSON includes before/after PTY dimensions and only TERM, TERM_PROGRAM,
COLORTERM, LANG, LC_ALL and LC_CTYPE. Unset differs from empty; invalid or
oversized values are omitted. Output is created exclusively with mode 0600.
Only the fixed queries are written to terminal stdout; status is the private
JSON file and exit code. Admission errors which prevent creating that file exit
with status 2 silently. No final terminal print can block after descriptor flags
return to their original blocking state. A caller's pending terminating signal
may prevent a final JSON result after restoration; an absent/incomplete result
never qualifies a case.

No raw stdin, rejected sequences, arbitrary environment, terminal history,
credentials or hashes of rejected input are recorded. SIGKILL or losing the
TTY can prevent restoration and is not a successful result.

Record the script/source and case identities independently. Compare local and
remote observations at the same cursor, viewport and palette state. The record
supplements client content-rectangle/scale/font diagnostics and paired actual
Copilot screenshots; it does not replace them or establish native PTY ordering.
The controlled PTY unit tests inject synthetic replies solely to qualify the
collector, privacy and restoration behavior. They are not native protocol proof.
