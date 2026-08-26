# In-place editing (Windows)

Prefer the `edit_file` tool. `sed` is not present by default.

## PowerShell

```powershell
(Get-Content file.txt) -replace 'old','new' | Set-Content file.txt
```

Read fully before writing — piping `Get-Content` straight into `Set-Content` on the
same path truncates the file first.

Preserve encoding explicitly when it matters:

```powershell
$c = Get-Content -Raw file.txt
$c -replace 'old','new' | Set-Content -NoNewline -Encoding utf8 file.txt
```

## Across many files

```powershell
Get-ChildItem -Recurse -Filter *.rs |
  ForEach-Object {
    (Get-Content $_.FullName) -replace 'old-symbol','new-symbol' |
      Set-Content $_.FullName
  }
```

List the matches first with `Select-String -Pattern 'old-symbol' -List`.

## Line endings

Windows tools write CRLF. In a repository with an `.gitattributes` enforcing LF,
check `git diff --stat` after a batch edit — a whole-file line-ending change is
easy to commit by accident.
