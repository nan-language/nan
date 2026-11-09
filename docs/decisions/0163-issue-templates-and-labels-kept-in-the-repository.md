# 0163. Issue templates and labels kept in the repository

Status: accepted

## Context

Agents file the issues, through the GitHub API, which applies no template and no label: an issue gets a label only when its author names it, and the GitHub tools of agents' sessions read a label by its name but cannot list them. Issues came with the two default labels agents guessed, `bug` and `enhancement`, or with none, and no label told which part of NaN an issue concerns. The seven other default labels were on no issue.

## Decision

- **Markdown issue templates**, a bug report and a feature request, give an issue its title, first line and sections, and name its labels: its kind, the areas of NaN it concerns and the impacts that apply. An agent writes the body itself, so a template is the outline it follows rather than a form.
- **The labels are those of `.github/labels.yml`.** On each change of the file on `main`, the labels workflow creates and updates its labels and deletes every other, with a few lines of `gh` as the release workflow publishes releases.
- **The `file-issue` skill** sends agents working in this repository to the templates.

Rejected alternatives:

- **A third-party action syncing the labels:** it would hold a token that writes issues, for a few lines of shell.
- **Issue types for the kind:** a second field to set beside the labels, which the areas need anyway.

## Consequences

- A label is made or changed in `labels.yml`; one made on GitHub's pages lasts until the file next changes on `main`.
- Agents working in another repository, such as NaN OS's, follow the templates only when their own instructions point to them.
