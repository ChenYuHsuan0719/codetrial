# Provider pooling

Provider pooling spreads rooms over more than one LiveKit project, so
concurrent interviews draw on several projects' quotas instead of exhausting
one. A single-project deployment needs none of this and is unaffected by it.

Before a room token is minted, the server uses cached periodic probes and probes
only providers it tries until one is available. A fresh 429 excludes exhausted
connection minutes and a fresh 401/403 excludes a refused credential. The log
names the project and status without printing credentials. Startup rotation
summaries show provider ids and endpoint authorities (host and optional port);
provider debug output shows the host. Neither prints URL userinfo, paths, query
strings or fragments. Replacing a configured
credential takes effect after restarting the server.

## Adding a project

Add one `config/codetrial.env.<id>` per extra project, each with its own
`LIVEKIT_URL`, `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET`, and optionally
`GOOGLE_API_KEY`. The credentials already in the environment or in
`config/codetrial.env.local` become the provider called `primary`.

The id is the part after `codetrial.env.`, and it must look like a GitHub
username: 1 to 39 letters, digits, or single dashes, with no leading, trailing,
or doubled dash. `primary` is reserved in any casing, so no file can shadow the
environment's credentials. `local` and `example` are the operator's own config
and the checked-in template, so neither is read as a project.

Ids are otherwise matched exactly, so on a case-insensitive filesystem
`codetrial.env.Foo` and `codetrial.env.foo` are one file, not two projects. A
file that does not parse, or that names only part of a provider, is skipped with
a warning on stderr rather than failing the load: an optional extra project must
not be able to stop the primary one from serving.

## Gemini credential failover

Set `GOOGLE_API_KEYS=first-key,second-key` to try Gemini credentials in order.
Whitespace around entries is ignored; empty entries and duplicate keys are
rejected. A list takes precedence over `GOOGLE_API_KEY` at the same level. A
provider's own credentials replace the global ones entirely: its
`GOOGLE_API_KEYS` replaces the global list, and a provider that sets only
`GOOGLE_API_KEY` uses that key and not the global list, so its rooms never bill
another project. Single-key configurations still work. A blank value of
`GOOGLE_API_KEYS` counts as unset.

Quota exhaustion and rejected or disabled keys select the next available key.
A key counts as rejected on a 401, on a 403 that is not about quota (including
one with no body, which is how a WebSocket handshake refusal arrives), and on a
400 whose reason names the key, such as `API_KEY_INVALID` or an IP, referrer or
app restriction. Quota failures put a key on a 60-second cooldown shared across
interviews in the same process, separately for Live and report requests.
Rejected keys are disabled for both for 60 seconds, except that a 403 with no
reason disables the key only for the kind of request that saw it. When every
key is out on quota, an interview waits for the first one back within its
restart budget instead of ending. A working key
stays selected for that interview; report failover leaves the Live key alone,
and moving to a backup does not wait out the failed key's backoff. Other
failures follow the existing retry rules without switching keys. Keys from the
same Google Cloud project share quota, so adding keys does not add quota to
that project.

A sole key is never taken out of rotation, because there is nothing to move to:
quota failures keep the retry budgets they always had, and a rejection is
reported as a rejection, rather than as an empty rotation, without disabling the
key for every later room.

An interview's first Live open retries within 30 seconds, the time one attempt
could always take, so an unreachable Gemini ends the room as quickly as before.
`check-gemini` selects keys the same way: it moves past rejected keys, trying no
more than an interview's first open would, and reports any other failure at
once.

Interim reviews never retry in place, and they share invalid-key failures and
quota cooldowns with the final report.

Switching keys keeps the LiveKit room and project, but starts a cold Gemini
session from the retained transcript, editor and interview state. Resumption
handles stay with their original key; seamless resumption across keys or
projects has not been verified.

## How a room finds its project

The id travels inside the room name, `interview-<id>-xxxxxxxx`. That is the
channel the web process uses to tell a separately launched
`codetrial run-livekit ROOM` which project the candidate was handed a token
for. Rooms minted by a single-provider deployment carry no id segment and
belong to `primary`.

Give both processes the same config directory: `config/` by default, or the
directory holding the `--config` file. `codetrial run-livekit` refuses a room
that names a project it cannot see, rather than joining the wrong one. An agent
that cannot read the config directory has a pool of one and an unresolvable id,
which is exactly the case that has to fail loudly.

## Choosing which project leads

`CODETRIAL_PROVIDER_ORDER` puts the projects it names at the front of the
rotation, in the order given, so an account with quota to burn is spent before
one that costs money. Anything it does not name keeps the order it already had
and follows: a pool is capacity, so an unnamed project stays in the rotation
rather than dropping out of it. A name no project answers to, or a name listed
twice, is reported on stderr and ignored.

Rooms that carry no provider segment resolve by id, so wherever a provider
called `primary` exists, leading the rotation with another project does not
re-point them. The exception is a pool built entirely from
`config/codetrial.env.<id>` files, which has no `primary` to resolve and falls
back to whichever project is first; there, and only there, this setting does
move which project answers for a segment-less room.

## Upgrade hazard: the first dashed id

Adding the first id that contains a dash is the one change worth draining old
processes for. A binary from before dashed ids were allowed splits the room name
at the first dash rather than the last, so it reads
`interview-eu-west-xxxxxxxx` as project `eu`. It then refuses the room, which is
the safe answer, unless a project really is named `eu`, in which case the
candidate and the agent land in different projects. Room names minted before the
change are unaffected: the random suffix holds no dash, so there was only ever
one dash to split on.
