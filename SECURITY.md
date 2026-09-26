# Security Policy

## Reporting a vulnerability

Please do not report security vulnerabilities through public GitHub issues.

Report them privately instead: open the **Security and quality** tab of this
repository and choose **Report a vulnerability**, or go directly to
https://github.com/bulentgercek/bg-bucket-browser/security/advisories/new.
Only you and the maintainer can see the report. A GitHub account is required.

If you cannot use GitHub, email bulentgercek@gmail.com.

A useful report includes the application version (Settings → About), your
operating system, and the steps to reproduce the issue.

## Supported versions

Only the latest release receives security fixes.

## What to expect

BG Bucket Browser is maintained by one person. Reports are handled on a
best-effort basis: there is no guaranteed response time and no bug bounty.

## Scope

- **Credentials.** S3 access keys and the optional RunPod API key are kept in
  the operating system's credential store (Secret Service on Linux, Credential
  Manager on Windows, Keychain on macOS). They are not written to the
  application's settings file or to its logs.
- **Network.** The application connects to three places:
  - the S3 endpoint you configure for each connection;
  - `https://api.runpod.io/graphql`, only when a RunPod API key is set, to read
    the volume's total capacity;
  - `https://bulentgercek.com/feedback`, only when you press **Send** in
    Settings → Feedback.
- **Feedback reports.** What a report contains and how long it is kept is
  described in the [Debug logging](README.md#debug-logging) section of the
  README.
