# Work that lasts several days

Start lasting work in the current `rook tui` conversation:

```text
/goal Implement the agreed plan and verify the result
```

The goal keeps this session's ID, transcript, model, effort and permissions.
It has no overall time, iteration or token ceiling. Each bounded stage continues
in the same session when its allowance ends. Ordinary chat outside an active
goal retains its normal limits.

Send corrections in the chat: `↩` confirms receipt, and `✓ taken up` confirms
inclusion in context. Another `/goal <text>` updates the goal; `/goal` displays it.
Ctrl-C pauses after the active operation; `/continue` resumes. Switch through
Ctrl-P → sessions → Enter, or `/session <id>`, without stopping the previous
session. `/new` opens another conversation. Closing the TUI leaves the daemon
working. Restarting the daemon resumes runnable goals in their original sessions;
paused goals stay paused. Model outages and unknown-effect recovery use the
same machinery described below.

This requires the shared daemon (the default `rook tui` starts it). Browser chat
and a daemon-connected `rook chat` accept `/goal` too. In local `--alone` mode the
command continues to set goal metadata only. It does not grant extra permissions;
use the existing stance selector (F2) for autonomy. Context, tool deadlines,
retry windows, stalled-work detection and completion verification remain bounded.

The standalone Tasks pane and CLI below are optional interfaces for separate
background runs, with their own default total budgets.

The **Tasks** pane in `rook tui` submits a durable goal to `rookd`. The daemon runs bounded turns,
records their results, and continues until independent verification accepts the
goal, a budget is reached, or somebody must intervene. Closing a terminal or
browser does not stop it. Restarting `rookd` resumes runnable tasks from the same
store; paused tasks stay paused.

## Optional standalone Tasks pane

Open `rook tui` and press **F4**, or choose **tasks** in **Ctrl+P**. No task IDs or
shell commands are needed:

- **n**: write a new goal; **Tab** toggles autonomous approval, **Enter** starts it.
- **↑/↓**: select a task. Its status, answer and verification update automatically.
- **Enter**: write a correction for the selected task; **Enter** sends it.
  Paste supports paragraphs; **Alt/Shift+Enter** inserts a newline.
- **p / r**: pause / resume. **x**, then **y**: cancel. **d**: forget a terminal record.
- **PgUp/PgDn**: scroll the selected task. **Esc**: leave the editor or pane.

Saved corrections appear as **Pending**, then automatically change to **In
context** when the agent takes them up. Polling preserves an unsent draft, and
connection errors preserve the text and retry ID. Closing the TUI leaves tasks
running. The pane uses configured total budgets (seven days by default).

## Optional CLI

```sh
rook -C /path/to/project task start "Implement the agreed plan and verify it" --yes --seconds 604800
rook task list
rook task show TASK_ID
rook task steer TASK_ID "Keep the public API compatible" --message-id api-compat-1
rook task pause TASK_ID
rook task resume TASK_ID
rook task cancel TASK_ID
rook task forget TASK_ID
```

`--yes` explicitly approves operations the deny list allows. Without it, the
task uses the configured policy and unattended approval requests are refused.
Only one unfinished durable task may own a workspace. Separate workspaces can
run concurrently. Existing interactive sessions are still available; avoid
editing a task's files independently while its verification is running.

`task start` also accepts `--max-iterations`, `--tokens`, and `--seconds`.
Zero removes that particular ceiling. Each individual turn still has its normal
step/token/time limits: reaching a turn limit starts another iteration instead
of ending the whole task. Total time includes pauses and downtime. Token limits
are checked at safe boundaries; a provider request already in flight may cross
one, so they are not a hard billing cap imposed on the provider.

## Corrections and acknowledgement

Every correction has a stable ID and two distinct stages:

1. **Saved / pending**: durably accepted by the daemon.
2. **In context**: inserted into the agent's conversation at a safe boundary,
   with a timestamp and session ID. This confirms delivery, not completion of
   the requested change.

An in-flight model request or tool is not interrupted to deliver a message.
The next safe boundary picks it up. Applied corrections carry forward to later
iterations and the independent goal checker. A correction arriving during
verification prevents that outdated verdict from completing the task.

The CLI waits up to 30 seconds for acknowledgement; `--wait-secs 0` returns
immediately. `task show` reports later delivery. If a connection fails, retry
with the same `--message-id` and text: delivery is idempotent. A different text
with the same ID is rejected. A receipt can be retrieved again after completion.

The web **Tasks** tab polls status and receipts, preserves the correction draft
while polling, and offers pause/resume/cancel. In the shared TUI use `/task list`,
`/task start <goal>`, `/task start-autonomous <goal>`, `/task show <id>`,
`/task steer <id> <text>`, and `/task pause|resume|cancel|forget <id>`.
`/task show` displays acknowledgement; these commands manage background work
without steering an unrelated chat turn. `rook tui --alone` does not share a
daemon and therefore cannot manage durable tasks.

## Failure and completion

Temporary model failures schedule exponential retries (30 seconds up to 30
minutes by default), preserving the current session and already billed tokens.
An outage lasting 24 hours moves the task to `blocked`. Credentials, exhausted
provider credit, unresolved questions, or repeated iterations with no tool
activity can also require intervention. Read `reason`, send guidance if needed,
then resume. Failed iterations are not counted as idle work.

The agent maintains `.rook/plan.md`; subsequent iterations receive that plan,
recent results, corrections, and the previous verification. If the project has
`.rook/evaluation.toml`, its checks must pass; an empty scorecard is rejected.
Passing existing tests alone cannot finish a task: an independent checker must
inspect evidence and return `holds` for the current goal. Model-based checking
is evidence gathering, not a mathematical guarantee of correctness.

After a crash, an operation whose effect is unknown blocks automatic replay.
Inspect the session with `rook session recovery SESSION_ID`. Only acknowledge
an operation after inspecting its effects, using the recovery command's
`--acknowledge` and `--note` options, then resume the task. This protection also
applies to interrupted evaluation commands. Cancellation does not undo effects.

`limited` means the saved total budget has been spent. Resume cannot reset it;
cancel that record and submit an explicit new goal/budget. Forget removes only
the task record of a completed or fully stopped cancelled run. Sessions remain
subject to normal retention.

## Defaults and storage bounds

These settings go in the user's `config.toml`:

```toml
[work]
max_parallel_runs = 2
max_runs = 64
max_messages = 128
max_message_bytes = 8192
max_goal_bytes = 32768
retained_iterations = 16
max_iterations = 10000
max_tokens = 0
max_seconds = 604800
retry_initial_secs = 30
retry_max_secs = 1800
retry_window_secs = 86400
idle_iterations = 8
```

Completed records count against `max_runs` until forgotten. Corrections are
never silently discarded; reaching their limit rejects new messages. Active
iteration sessions and their descendants are protected from retention until
the iteration is accounted for. Existing storage/context limits still apply.

## Keep the daemon available

The machine must remain powered and awake, with model access. The task continues
when `rookd` starts again, but Rook does not install a boot service automatically.
Use the operating system's supervisor for automatic restart after process or
machine failure. For example, this **systemd user unit** can be saved as
`~/.config/systemd/user/rookd.service`, replacing the absolute paths:

```ini
[Unit]
Description=Rook durable work
After=network-online.target

[Service]
ExecStart=/absolute/path/to/rookd --workspace /absolute/path/to/project
Environment=ROOK_HOME=/absolute/path/to/rook-home
Restart=always
RestartSec=10

[Install]
WantedBy=default.target
```

Enable it with `systemctl --user enable --now rookd`. Running a user service
after logout/at boot also requires the host's user-lingering configuration.
On macOS use a LaunchAgent with an absolute `ProgramArguments` array,
`RunAtLoad`, `KeepAlive`, and `EnvironmentVariables.ROOK_HOME`; that starts at
login. A LaunchDaemon or an always-on host is needed for unattended operation
before login. Ensure only one service owns the same `ROOK_HOME`.

## HTTP interface

`GET/POST /api/work` lists/starts tasks. Start accepts `goal`, optional
`workspace`, `autonomous`, `max_iterations`, `max_tokens`, and `max_seconds`.
`GET /api/work/{id}` returns full state and receipts;
`POST /api/work/{id}/steer` accepts `{"id":"unique-id","text":"correction"}`;
`POST /api/work/{id}/control` accepts the JSON string `"pause"`, `"resume"`, or
`"cancel"`. `DELETE /api/work/{id}` forgets a stopped terminal record.
The same authority checks as the other daemon endpoints apply.

`rook work` remains the foreground scorecard loop for existing scripts.
Use `rook task` for daemon-owned work, automatic recovery, and durable steering.
