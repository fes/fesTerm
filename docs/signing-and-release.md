# Signing and release operations

fesTerm releases use three independent trust layers:

1. Apple Developer ID signs and notarizes the macOS application and DMG.
2. Microsoft Artifact Signing applies public-trust Authenticode signatures to
   fesTerm's Windows executable and NSIS installer. The hash-pinned ConPTY
   sidecars retain and validate their upstream Microsoft signatures.
3. cargo-packager's updater key signs the exact self-update payload for every
   supported platform.

The private credentials are scoped to the GitHub `release` environment. They
must never be committed, logged, attached to a workflow artifact, or supplied
to pull-request jobs.

## Microsoft Artifact Signing

The Basic Artifact Signing account currently costs $9.99 USD per month in the
Azure portal. Public Individual identity validation is limited to eligible
individual developers in the United States and Canada. The Azure billing
account type and legal identity must match the validation type.

Repository configuration:

- account: `fesLabs-CodeSign`
- region endpoint: East US
- certificate profile: `fesTermPublicTrust`
- authentication: GitHub OIDC for the `release` environment
- RBAC: `Artifact Signing Certificate Profile Signer`, scoped to the
  certificate profile rather than the subscription or signing account

The workflow keeps Azure tenant, subscription, client, endpoint, account, and
profile identifiers as environment variables because they are identifiers,
not credentials. No Entra client secret exists. The federated credential is
restricted to `repo:fes/fesTerm:environment:release`.

## Apple Developer ID and notarization

The Apple Developer Program membership supplies:

- a `Developer ID Application` certificate and private key exported as a
  password-protected PKCS#12 identity;
- an App Store Connect team API key with Developer access for `notarytool`;
- the team ID, API key ID, and API issuer ID.

GitHub stores the PKCS#12 data, its generated export password, and the `.p8`
API key as environment secrets. Identifiers and the signing identity name are
environment variables. The local recovery copies are protected by the macOS
login keychain under fesTerm-specific service names.

The release job signs with hardened runtime and a secure timestamp, notarizes
and staples the application, builds and signs the DMG, then separately
notarizes and staples the final DMG.

## Updater signing

`packaging/updater.pub` is the only updater-key material committed to the
repository. The encrypted private key and its generated password are GitHub
environment secrets, with recovery copies in the macOS login keychain.

Updater signatures cover the exact bytes installed by
`cargo-packager-updater`:

| Target | Signed updater payload |
|---|---|
| macOS | notarized `fesTerm.app` tarred as `.app.tar.gz` |
| Windows | Authenticode-signed NSIS `.exe` |
| Linux | raw executable `.AppImage` |

The Debian package is package-manager-owned and is never an in-place updater
payload.

## Release controls

Production publication is triggered only by a `vMAJOR.MINOR.PATCH` tag whose
version matches the workspace. Manual dispatch builds the same signed
artifacts but does not publish.

The workflow:

- pins third-party actions to full commit hashes and cargo-packager to 0.11.8;
- runs the release validation suite before any packaging job;
- fails before packaging if protected credentials are missing or the updater
  public key differs from `packaging/updater.pub`;
- signs and verifies native packages on native runners;
- uploads only immutable, versioned artifact URLs into update metadata;
- creates the GitHub Release as a draft, uploads the signed artifacts, uploads
  `festerm-update.json` last, and only then publishes the release.

Rotate a compromised credential immediately:

- revoke and replace the Apple certificate or App Store Connect API key;
- replace the Azure federated credential or certificate profile assignment;
- replace the updater keypair and ship the new public key in a trusted
  platform-signed fesTerm release before signing future updates with it.
