# Security

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

## Verifying a release

From the next release on, each GitHub release carries the crates' `.crate` files as crates.io
serves them, checked against a packaging of the tagged source, and a Sigstore bundle with their
SLSA build provenance, made by the release workflow.
Check a downloaded crate with the GitHub CLI:

```sh
gh attestation verify sisa-caching-<version>.crate --repo sisaio/sisa-caching-rs
```
