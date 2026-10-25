# Security Policy

## Supported versions

Kiln is pre-1.0. Security fixes land on `main` and in the latest release
tag only. Older tags are not patched.

## Reporting a vulnerability

Please **do not** open a public GitHub issue for security reports.

Email **cody@will.dev** (or the address listed on the GitHub profile of
the repository owner) with:

- a short description of the issue
- steps to reproduce or a proof of concept
- the affected version or commit, if known

You should get an acknowledgement within a few days. We will coordinate a
fix and a public disclosure date with you. Please give us reasonable
time to ship a fix before sharing the report widely.

## Scope

In scope: the `kiln` / `kilnc` compilers, the standard library runtime,
install scripts, and release artifacts published from this repository.

Out of scope: vulnerabilities only in dependencies (report upstream),
issues that require unrealistic local privileges, and denial of service
from deliberately pathological input to a development build.
