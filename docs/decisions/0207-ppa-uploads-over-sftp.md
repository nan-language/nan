# 0207. PPA uploads over SFTP

Status: accepted

## Context

The release workflow uploaded each series' source package to the PPA ([0070](0070-the-ppa-package.md)) over Launchpad's anonymous FTP. Launchpad's FTP server now and then waits in vain for a session's passive data connection and answers `550 Requested action not taken: internal server error`, at times for minutes, so retries did not save every release; and a series that failed stopped the upload of the series after it. Launchpad also accepts uploads over SFTP, which carries the whole session over one SSH connection.

## Decision

- **Uploads go over SFTP**, through dput's `ssh-ppa` target, as the Launchpad user `catwallon`, owner of the `nanlang` team, with the private key of the `PPA_SSH_PRIVATE_KEY` secret and Launchpad's host key pinned in the workflow.
- **Every series is attempted**, each up to three times; the job fails after the last series if any failed.

## Consequences

- The public half of `PPA_SSH_PRIVATE_KEY` is registered to `catwallon` on Launchpad; the key has no passphrase.
- A change of Launchpad's host key fails the upload until the workflow pins the new one.
