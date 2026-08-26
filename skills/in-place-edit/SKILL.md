---
name: in-place-edit
description: Edit files in place from the shell across platforms — sed, in-place flags, and the traps that differ between GNU, BSD and Windows.
version: 1.0.0
license: MIT
keywords: [sed, shell, cross-platform, text]
variants:
  - when: { userland: [bsd] }
    body: variants/bsd.md
    note: BSD sed requires an explicit backup suffix argument.
  - when: { os: [windows] }
    body: variants/windows.md
---

# In-place editing (GNU userland)

Prefer the `edit_file` tool. Reach for the shell only when the change is
mechanical across many files.

## GNU sed

```sh
sed -i 's/old/new/g' file.txt
sed -i.bak 's/old/new/g' file.txt      # keep a backup
```

`-i` takes the suffix attached, with no space. `sed -i '' …` is a **BSD** form and
fails here — GNU reads `''` as the script.

## Across many files

```sh
rg -l 'old-symbol' --glob '*.rs' | xargs sed -i 's/old-symbol/new-symbol/g'
```

Always run `rg -l` alone first and read the list. `xargs` with an empty input runs
the command with no arguments on some systems.

## After any batch edit

Re-read one changed file and run the project's build or test command. A regex that
matched more than intended is not visible in the exit status.
