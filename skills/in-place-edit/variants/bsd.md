# In-place editing (BSD userland — macOS, FreeBSD)

Prefer the `edit_file` tool. Reach for the shell only when the change is
mechanical across many files.

## BSD sed

```sh
sed -i '' 's/old/new/g' file.txt       # note the mandatory empty suffix
sed -i '.bak' 's/old/new/g' file.txt   # keep a backup
```

The empty `''` is **required**. `sed -i 's/old/new/g' file.txt` — the GNU form —
consumes the script as the backup suffix and fails with an unhelpful error.

Portable alternative when a script has to run on both:

```sh
perl -pi -e 's/old/new/g' file.txt
```

## Across many files

```sh
rg -l 'old-symbol' --glob '*.rs' | xargs sed -i '' 's/old-symbol/new-symbol/g'
```

Run `rg -l` alone first and read the list.

## Other BSD/GNU differences worth remembering

| task | BSD | GNU |
|---|---|---|
| extended regex | `sed -E` | `sed -E` or `sed -r` |
| non-recursive `readlink -f` | not available; use `realpath` | `readlink -f` |
| `date` arithmetic | `date -v-1d` | `date -d '1 day ago'` |

## After any batch edit

Re-read one changed file and run the project's build or test command.
