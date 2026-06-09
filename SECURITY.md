# Security Policy

rekody handles sensitive inputs — microphone audio, API keys stored in the macOS Keychain, and Accessibility-privileged text injection. Security reports are taken seriously.

## Supported Versions

Only the latest minor release receives security fixes.

| Version | Supported |
| ------- | --------- |
| 0.5.x   | ✅        |
| < 0.5   | ❌        |

## Reporting a Vulnerability

**Please report vulnerabilities privately — do not open a public issue.**

Open a [private security advisory](https://github.com/rekody/rekody/security/advisories/new) on GitHub. This keeps the report confidential until a fix is published.

Please include:

- A description of the issue and its impact
- Steps to reproduce (or a proof of concept)
- The rekody version (`rekody --version`) and your macOS version
- Any suggested mitigation, if you have one

## What to Expect

- **Acknowledgement** within 3 business days
- **Initial assessment** within 7 business days
- **Fix or mitigation plan** within 14 business days for confirmed, high-severity issues
- **Public disclosure** coordinated with the reporter once a fix is released

## Scope

In scope:

- Credential handling (Keychain access, API key logging, environment exposure)
- Audio or transcript leakage to unintended providers
- Injection of untrusted content into the user's foreground app
- Supply-chain issues in the release binary (tampered tarballs, checksums, Homebrew formula)
- Update mechanism (`rekody update`) integrity

Out of scope:

- Issues requiring prior physical or root access to the user's machine
- Vulnerabilities in third-party STT/LLM providers (report to them directly)
- Denial-of-service via local resource exhaustion (large audio buffers, etc.)
- Missing hardening unrelated to a concrete vulnerability

## Disclosure

rekody follows coordinated disclosure. A CVE is requested for confirmed, high-severity issues. Reporters are credited in the release notes unless they request anonymity.
