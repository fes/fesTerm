# ADR 0006: Native SSH Backend with OpenSSH Interoperability

- **Status:** Accepted
- **Date:** 2026-08-05

## Context

SSH is intended to be a first-class fesTerm session type. Requiring users to install and invoke an external `ssh` executable would weaken cross-platform packaging, reconnect control, connection-state reporting, and profile integration. Reimplementing SSH and cryptography would add substantial security risk and little useful learning value.

Existing users may already have OpenSSH configuration and keys that should be reusable where practical.

## Decision

fesTerm will integrate an established Rust SSH implementation in-process rather than implementing SSH cryptography or depending on an external SSH executable at runtime.

fesTerm will interoperate with OpenSSH configuration and common key material, but OpenSSH configuration will be mapped into fesTerm's own internal profile model. OpenSSH compatibility is an input and migration surface, not the application's authoritative data model.

SSH verification will use layered tests: fake or in-process tests for state logic, library client/server tests where supported, and a small containerized OpenSSH `sshd` interoperability suite owned entirely by the test environment.

## Consequences

- SSH library selection requires explicit security, maintenance, algorithm, authentication, and async-integration evaluation.
- Users can create SSH tabs without first opening a local shell.
- Automatic reconnect can be implemented as application behavior.
- Unsupported OpenSSH directives must be reported clearly.
- Tests must not depend on machine-wide SSH configuration or user credentials.
