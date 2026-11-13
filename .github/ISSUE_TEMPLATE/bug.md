---
name: Bug report
about: NaN does something wrong, or its specification says something wrong
labels: bug
---

<!--
Title: the defect in one sentence naming the command, diagnostic or item, such as "nan fmt drops the labels of a layout write, which nan check then rejects with expr.argument-label".
Labels: bug, each label under areas in .github/labels.yml for the parts of NaN the issue concerns, and each label under impacts that applies. Through the API, an issue gets no label from this template: set them all when filing it.
-->

Toolchain vX.Y.Z, target x86_64-linux.
<!-- The release, or the commit of a toolchain built from source, and the target when the defect depends on it. -->

## What happens

<!-- What goes wrong, in a few sentences. -->

## Reproduction

<!-- The smallest workspace that shows it, as the specification writes examples: one nan block in which each file follows a `// file: <path>` line, then the commands run from the workspace root, each after `$ `, with their exact output. When it does not reduce, say what it depends on, what was ruled out and where a workspace showing it lives. -->

```nan
// file: repro/package.nan
```

```text
$ nan check repro
```

## Expected

<!-- What should happen instead, citing the specification by chapter and section, such as 22 §Test items. -->

<!-- Optionally, a Where section with the code at fault, a Workaround section, and the related issues as #N. -->
