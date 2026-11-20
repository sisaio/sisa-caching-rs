# Security

## Reporting

Report a vulnerability privately through
[GitHub security advisories](https://github.com/sisaio/sisa-caching-rs/security/advisories/new),
not in a public issue.

## Supported versions

| Version | Security fixes |
| --- | --- |
| The latest 0.x minor release | Yes, as a patch release of that minor |
| Older releases | No: upgrade to the latest minor |

## After you report

- You get a first answer within 7 days.
- We confirm the problem with you, agree on a fix, and keep the advisory private until the fixed
  release is on crates.io.
- The fix ships as a patch release, and the advisory is published with it, crediting you unless
  you'd rather not be named.

## What counts

A cached value, a key, a connection URL or a password reaching a warning, an error message or a
`Debug` output is a vulnerability: info spans and warnings carry the store and the invalidation
kind, and only debug spans and events carry keys. The library encrypts nothing: values that sit
unencrypted in Redis or in a process's memory are the application's choice, not a vulnerability.

## Verifying a release

Since 0.4.0-rc.1, each GitHub release carries the crates' `.crate` files as crates.io serves them,
checked against a packaging of the tagged source, and a Sigstore bundle with their SLSA build
provenance, made by the release workflow. Check a downloaded crate with the GitHub CLI:

```sh
gh attestation verify sisa-caching-<version>.crate --repo sisaio/sisa-caching-rs \
    --signer-workflow sisaio/sisa-caching-rs/.github/workflows/release.yml
```
