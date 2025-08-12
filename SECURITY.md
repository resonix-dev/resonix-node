# Security Policy

Reporting a Vulnerability
- Please report security issues privately via GitHub Security Advisories or email the maintainers rather than opening a public issue.
- Include details to reproduce the issue, affected versions, environment, and potential impact.
- We will acknowledge receipt within 72 hours and aim to provide an initial assessment within 7 days.

Supported Versions
- This project is pre-1.0; only the latest release/commit on `main` is supported.

Handling and Disclosure
- We will work with you to validate and remediate issues.
- Once a fix is available, we will publish a release and coordinated disclosure.

Operational Guidance
- If you enable authentication, keep your `server.password` secret and rotate it if leaked.
- Avoid exposing the node directly to the public internet; prefer a private network or authentication proxy.
- Keep dependencies and `yt-dlp` up to date if you use the resolver.
