# Compiler-checked maintained examples

Run `cargo xtask check-docs` to compile the Rust fences listed in
[checks.json](checks.json). Use `cargo xtask check-docs --run` to execute their
normal programs and test harnesses. `--only ownership,serde-json` selects article
IDs. `--output path.json` writes a new JSON receipt; an existing file is refused.

This command checks explicitly maintained repository inputs. It never scans or
executes the downloaded documentation cache. The idea of compiling a curated
knowledge library comes from [Haico's library checker at its reviewed
commit](https://github.com/Revertron/Haico/blob/5b352f3c7e3b608e68ec49af65a66feb098e07d4/scripts/check_library.py).
Rook's parser, Cargo protocol checks and process handling are independently
implemented.

## Adding an article

Add an index entry with an ID, exact repository-relative article, manifest and
lockfile paths, and the expected number of Rust fences. The count includes
explicit ignores. Each manifest describes an independent package at version
0.0.0 with an empty `[workspace]` and disabled build scripts, libraries, bins,
tests and benches. Its examples are generated from the admitted fences. Only
exact `=x.y.z` registry dependencies and a committed version-4 Cargo.lock are
accepted. `path`, `git`, patches, custom registries and external targets are
refused. Include the required crate versions in the development Cargo cache:
the checker uses `--locked --offline` and does not download missing dependencies.
The two current articles use versions already in Rook's root lockfile.

Every checked fence is a standalone Rust program; statements are never guessed
into a `main`. Rustdoc hidden lines (`# ` and `#`) and escaped hashes (`##`) are
supported, preserving diagnostic line numbers. Modifiers are comma separated:

| Fence | Check |
| --- | --- |
| `rust` | Build a normal executable and a test harness; `--run` executes both. |
| `rust,items` | Append an empty `main` for definitions and tests. |
| `rust,no_run` | Build both targets, including test bodies; never execute them. |
| `rust,compile_fail,E0308` | Require a real selected-source error with every annotated typed code. |
| `rust,compile_fail` | Require a real selected-source compiler error, even without a code. |
| `rust,ignore` | Count and report the block without generating a program. |
| `rust,edition2024` | Require this edition to match the article manifest. |

Unknown, repeated and conflicting modifiers are errors. At least one block per
selected article must be checked. A failed Cargo command, missing executable,
partial verdict, dependency error, timeout or capture overflow cannot satisfy
`compile_fail`. Quoted diagnostic codes do not count. Runtime and harness exits
must both succeed for an executed block.

## Receipts and limits

The JSON receipt records input and implementation hashes, compiler/Cargo
versions, original article lines, compiler diagnostics, executable hashes,
command exits, output sizes and cleanup observations. Artifact paths refer to
the temporary workspace, which is removed after the check. A failed command
produces a failed receipt and a nonzero xtask exit; compiled or ignored blocks
never turn an incomplete article into a success. Generated inputs and admitted
executables are hashed after commands; original indexed files are rechecked
before success.

Defaults are 120 seconds per compiler command, 5 seconds per runtime command,
512 KiB combined output and a 600-second admission budget. Flags
`--compile-seconds`, `--runtime-seconds`, `--capture-bytes` and `--total-seconds`
allow bounded overrides. Before each launch the command patience is limited to
the remaining admission budget. Synchronous filesystem work, process creation
and platform cleanup are outside a hard real-time guarantee. `--work-root`
chooses the parent for a fresh workspace without reusing existing data.

Inputs are limited to 16 indexed articles, 64 selected Rust blocks, 256 KiB of
selected Rust code and 4 MiB of unique source inputs. Individual limits are
64 KiB for the index/manifest, 128 KiB per article, 64 KiB per block and 512 KiB
per lockfile. Paths cannot traverse directories or include symlink/reparse
components. These checks admit trusted maintained examples; they do not sandbox
Rust programs, registry build scripts, ambient Cargo configuration or hostile
concurrent filesystem changes. Execution inherits ordinary local permissions.
Process cleanup observations describe the owned process group/job and do not
certify the absence of detached processes.

CI compiles and executes these articles on Linux, macOS and Windows after the
workspace gate. The separate phase has a 12-minute outer ceiling and keeps the
workspace gate's 45-minute ceiling.
