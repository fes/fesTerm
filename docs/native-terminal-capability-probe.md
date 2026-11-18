# Native terminal capability observations

These opt-in terminal collectors supplement the matched-grid Copilot gate in
`docs/dev-tunnel-workflow-lab.md`. Run it in the actual existing controlled
local or Dev Tunnels foreground PTY before Copilot; keep keyboard input idle.
It requires a private existing output directory, an exclusive new output file,
and attached terminal handles for standard input/output (the same foreground TTY on POSIX).

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
aggregate summary file is created. On Windows, explicitly select the privately
staged helper as described below; a missing helper is refused with status 2.
The normal optional suites remain unchanged when the probe opt-in is absent.

The flag makes no service calls and excludes `--run`. It delegates directly to
`scripts/native_terminal_capability_probe.py` with inherited TTY descriptors.
On Windows it delegates to the explicitly selected native helper, also without
capture pipes or shell interpolation. POSIX results do not qualify Windows or
substitute for a native client/server comparison.

The fixed ten queries are DA1/DA2, DSR cursor position, OSC4 palette indices
0/7/15/255 and OSC10/11/12. No response, mode/palette change or terminal text is
injected. On POSIX the probe saves terminal attributes and descriptor flags, uses raw
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


## Windows console collector

The existing `festerm-pty-test-child` package also provides a separate
`festerm-terminal-capability-probe` executable. Build it on the controlled Windows
builder:

```powershell
cargo build --locked -p festerm-pty-test-child --bin festerm-terminal-capability-probe
```

Independently record the source identity and executable hash before staging it privately. This is a test helper, not a new
release, tunnel command, VM adapter mode or remotely callable service.

In the actual existing Windows local or remote foreground shell, set
`FESTERM_TERMINAL_CAPABILITY_HELPER` to the absolute staged executable path. Then
use the Python command above, or set the same four opt-in variables and run
`./scripts/run-optional-validation.ps1`. PowerShell invokes the helper directly
with inherited console handles. Neither dispatch builds or downloads it. The
output parent must already have a verified current-user-only Windows DACL;
create-new output uses the existing Windows security helper, never overwrites
an existing file, and does not rely on Unix mode bits. No console status text
is printed.

The native backend admits console input/output handles and records the visible
`srWindow` grid, rather than the scrollback-buffer size. It refuses pending input
records before and after temporary console-mode changes, without reading or
flushing them. During collection only ASCII key-down reply characters are
accepted; unrelated event records abort with counts, not event codes or content.
Counts identify whether their unit is bytes or console records. Keep the terminal
idle and do not run another reader: Windows has no POSIX foreground-process-group
admission check, and an idle buffer cannot prove another process will not read it.

Both console mode DWORDs are saved and restored exactly, without codepage or
system preference changes. The collector uses one owned I/O worker with a finite
handoff channel and the same fixed query/parser budgets. At the deadline or quiet
boundary it requests synchronous-I/O cancellation, waits at most 500 ms for the
worker and verifies completion before publishing success. Cancellation request
alone is insufficient. If completion cannot be verified, it attempts mode
restoration and exits the standalone helper unsuccessfully without a completed
record, retiring its worker with the process. A forced external termination can
also prevent restoration; absent/incomplete evidence never qualifies a case.

Windows results have `platform: "Windows"`,
`reply_scope: "application_visible_console"`, `terminal_restored` and
`worker_drained`; POSIX descriptor-only fields do not apply. Complete still means
exactly one recognized response for each of the ten queries and a stable grid.
Missing responses remain incomplete observations, not invented unsupported
capabilities or proof of a client failure.

Conhost may interpret queries and generate replies before the outer frontend
sees them. Consequently this observes what the Windows application receives
through its console/ConPTY stack; it does not establish that the client directly
answered every query. Microsoft documents this behavior in its
[VT query contract](https://learn.microsoft.com/en-us/windows/console/console-virtual-terminal-sequences)
and [pseudoconsole model](https://learn.microsoft.com/en-us/windows/console/pseudoconsoles).

Portable parser/dispatch tests and Windows cross-compilation qualify only the
collector implementation. Native Windows execution remains required to qualify
actual reply-record encoding, quiet/deadline cancellation, exact mode restoration,
private output and local/remote observations at both matched grids. Paired actual
Copilot screenshots and the remaining DT-02 matrix are still required.

The nonshipping helper explicitly keeps `unsafe_code = "deny"` and Clippy's
workspace warning policy, with a scoped exception only for its cfg-Windows
native-console FFI module. Production crates and workspace policies are
unchanged. This handle/mode/worker boundary needs independent security and
reliability review before integration; cross-compilation is not that review.
