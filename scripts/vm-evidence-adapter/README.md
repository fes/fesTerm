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

Each mode invokes the existing repository-owned runner and writes its
content-free status file beneath the artifact directory supplied by the shared
relay. It never accepts commands, arguments, paths, environment variables, or
output destinations from a job.

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
