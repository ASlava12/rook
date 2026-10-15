# Domain network policy

This opt-in policy restricts network destinations of local foreground commands
and their descendants. The initial supported platform is macOS with working
Seatbelt (`sandbox-exec`). Linux Landlock currently cannot provide the required
UDP/DNS containment; Linux, Windows and FreeBSD refuse policy-enabled commands.
`auto` never falls back to unrestricted execution while this policy is enabled.

```toml
[sandbox]
isolate = "required"
network = true

[sandbox.network_policy]
enabled = true
allow = ["registry.npmjs.org:443", "192.168.1.72:8080"]
# allow_private = false
# max_connections = 8
# max_transfer_bytes = 33554432
# idle_timeout_secs = 60
```

Each rule needs an explicit port. Exact names and IP addresses are accepted;
`*.example.org:443` matches subdomains on that port, not the apex or suffix
lookalikes. There are no implicit localhost, private-network or internet grants.
Empty `allow` denies every broker destination. Explicit IP rules admit that
address; named hosts resolving to private/special addresses require
`allow_private=true`. DNS is resolved by the trusted broker, checked and pinned
for the connection, without a second resolution by the requester. All returned
addresses must pass admission. Redirects require a new admitted request.

The command can open network connections only to its ephemeral localhost broker
port. Proxy environment variables select it for conventional HTTP/HTTPS clients;
discarding them does not open direct sockets. Seatbelt also withholds broad Mach
service access and Unix-socket networking in this mode, preventing the usual
alternate local service paths. The broker forwards bounded HTTP requests and
CONNECT tunnels. It does not decrypt TLS, validate application identities on a
shared destination IP, or filter content/paths inside an admitted tunnel. An
allowed destination remains an allowed network endpoint; it is not a promise
that every application served there is trusted. Other unrestricted processes
under the same user account are outside the command boundary.

The broker handles one HTTP request per client connection, strips proxy/hop
headers, constructs the upstream Host from the admitted URL, rejects ambiguous
lengths and unsupported request transfer coding, and does not follow redirects
itself. Requests have an 8 KiB header and 1 MiB body limit. At most 256 connections
are accepted per command, with configurable concurrency and transfer ceilings.
CONNECT has independent transfer ceilings per direction and a shared inactivity
timer: continuous download is not stopped because there is nothing to upload.
Command cancellation retires the broker and its tunnels. Descendants must be
stopped before foreground execution is reported settled.

Ordinary CLI, daemon, TUI and browser turns use the same launcher. Unsupported
egress paths refuse: editor terminals, background jobs, SSH/container execution,
external ACP execution, documentation fetching, skill installation, browser/web,
MCP and other tools without an accounted containment path. Local language servers
are withheld. Configured hooks are incompatible and refuse the turn. Trusted
operator connections (model providers and startup discovery of configured
services) remain outside the task-command policy; this is not a firewall for the
whole Rook process, other applications or the machine.

Each command has a unique broker ID. Bounded allow/deny/stop decisions contain
host, port and reason, without request paths, headers, bodies or credentials.
The common engine saves them as `rook:network-policy:v1` notes with the exact
tool-call ID. `rook session history SESSION --json` retains them across frontend
reconnect/reopen. The audit ring keeps 32 decisions and reports omissions.
Commands may explain a concrete denied destination; they must not silently
relax policy or repeatedly ask a person who has declined to provide input.

Actual tests cover the authorized LAN model endpoint and public npm registry,
forbidden domains, direct TCP/UDP and `--noproxy` bypass, private DNS admission,
header/body/connection bounds, tunnel ownership/cancellation, one-way activity,
and CLI/daemon durable decisions. Live external checks are opt-in test fixtures;
no service or host configuration is changed.

The OS/proxy split was compared with the primary
[Anthropic sandbox runtime implementation](https://github.com/anthropic-experimental/sandbox-runtime/blob/main/src/sandbox/macos-sandbox-utils.ts).
Rook's independent admission, protocol limits and fail-closed behavior are
verified by its native tests.
