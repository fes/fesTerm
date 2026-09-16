# fesTerm VM Evidence Adapter

This directory is the product-owned half of fesTerm's VM evidence harness. It
is installed by a pinned `vm-evidence-lab` controller and runs only after that
controller has restored the VM, staged an exact fesTerm source bundle, and
validated this adapter's pinned commit.

The adapter accepts exactly one source named `festerm`, an empty payload, and
one of these modes:

- `native-smoke`
- `os-input-smoke`
- `optional-validation`
- `running-session-stress`
- `keyboard-routing-check`
- `keyboard-routing-native`

Each mode invokes the existing repository-owned runner and writes its
content-free status file beneath the artifact directory supplied by the shared
relay. It never accepts commands, arguments, paths, environment variables, or
output destinations from a job.

`running-session-stress` runs the same isolated Running Sessions harness as
optional validation with fixed `--batch 8 --cycles 3` limits. It exercises native
sessiond on every platform and installed tmux/screen providers on Unix, without
opening a GUI or requiring a working GPU. Provider skips remain explicit in the
guest log; a backend pass is not native-window or OS-input qualification. The
private host configuration must explicitly allow this mode and pin a reviewed
adapter commit that supports it. Use a short guest checkout path for Unix
domain socket limits.

`keyboard-routing-check` invokes `scripts/check_keyboard_routing.py` without
arguments. It runs synthesized production routing/configuration/controller
checks on the guest platform; it does not open a window or establish native
input delivery. `keyboard-routing-native` enables the repository-owned
keyboard scenario in the existing OS-input driver and keeps its content-free
result in the job artifact directory. These are separate modes so a working
backend cannot mask missing desktop, graphics, or Accessibility prerequisites.
The native scenario checks no-selection Copy, palette capture, an identified
palette Paste request, and controlled terminal bytes. It overwrites the test
desktop clipboard with a fixed fixture without reading or restoring previous
contents: use a dedicated desktop with guest/host clipboard sharing disabled.
It is not selected-URL clipboard, AltGr, or IME qualification.
Both modes require an empty payload and a reviewed adapter pin.

`vm-evidence-lab` owns the host controller, Parallels provider, guest relay,
exact-source bundles, locks, result records, and manifests.

From a fesTerm checkout, bootstrap its reviewed shared-lab dependency with:

```sh
./scripts/bootstrap-vm-evidence-lab.sh
```

Run the host-independent Unix adapter contract check with:

```sh
./scripts/vm-evidence-adapter/tests/run.sh
```
