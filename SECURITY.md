# Security Policy

Security fixes land on `main` and ship with the next release. Please run the
latest version before reporting.

**Do not open a public issue for a security problem.** Use GitHub's private
vulnerability reporting:

  https://github.com/sumofbytes/po/security/advisories/new

(Repository → Security → Report a vulnerability.) Include the affected
version, a minimal reproduction, and your assessment of impact. Expect an
initial response within a week.

## Scope notes

`po` launches the agent commands you configure (`agents.json` /
`config.json`) and relays their requests; approving a tool call or
permission prompt hands that agent the capability. That is the product working
as intended, not a vulnerability.
