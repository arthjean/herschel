# Schema history

Three numbers in `crates/core` gate compatibility, and each one is bumped for a
different audience: the socket protocol between the two processes, the shape of
the capability record on disk, and the shape of the configuration file. The
constants live next to the code they govern; the reasoning for every bump lives
here, so a constant stays a constant instead of growing a changelog above it.

Each entry says what changed and, more importantly, what would go wrong if the
two sides were left to disagree quietly. A version is bumped when a mismatch
would produce a wrong answer rather than a parse failure, because a parse
failure is already loud.

## Protocol version

`kori_core::ipc::PROTOCOL_VERSION`. Incremented on any breaking change to
`Request` or `Response`. Negotiated by `Request::Hello` before anything else is
sent, so a mismatch is refused at the handshake and names both versions.

**2.** Added the lighting command and the per-channel lighting state.

**3.** Added the panel preset and the state the daemon reports for it.

**4.** Changed the shape of that preset in both directions: a reading slot
gained the color its band fades to, and the wordmark color it no longer draws
stopped being written. A preset refuses unknown fields, so a client of this
version and a daemon of the last cannot exchange one either way. The bump is
what turns that into a refusal at the handshake, naming both versions, instead
of a parse failure the first time a frame is applied.

**5.** Narrowed what `DisplayOutcome::deduplicated` claims. It used to mean the
whole command was a no-op; it now means only that no *frame* was sent, because
the brightness travels over its own report and is sent even when the picture is
unchanged. A version 4 client would read the new answer without error and report
"nothing was sent" about a panel that was just dimmed, so the two are separated
at the handshake rather than left to disagree quietly.

**6.** Added `DisplayState::faulted`. A version 5 daemon does not send it and a
version 6 client requires it, so the two cannot exchange a status at all; more
importantly, a client that could not see a stopped stream would leave the
operator no way to restart it now that the panel row writes on its own. Refusing
at the handshake says that, where a defaulted field would have reported a
healthy stream that had in fact stopped.

## Capability schema version

`kori_core::capability::CAPABILITY_SCHEMA_VERSION`. Bumped whenever the shape of
`CapabilityRecord` changes. A record carrying an unknown version is rejected
instead of guessed, because a field this build cannot see is a capability it
would silently report as absent.

**2.** Added the USB endpoint list on every interface and the RGB controller's
`RgbTopology`, both required to gate a lighting write.

**3.** Added the Kraken's `LcdTopology`, which gates a panel write.

## Config schema version

`kori_core::profile::CONFIG_SCHEMA_VERSION`. Bumped whenever the on-disk
configuration shape changes.

**2.** Added the per-channel lighting a profile carries.

**3.** Added the panel preset. Both fields are optional, so a file at an earlier
version parses exactly as it stands and the next save rewrites it.

**4.** Added the session table, which is what the daemon last committed rather
than what the operator saved under a name. The bump matters in the other
direction: an earlier build refuses an unknown key outright and would preserve
the file as unreadable, while the version makes it say which build wrote it.

## Deprecated fields awaiting removal

A field kept only so an older file still loads is listed here with the condition
that retires it. Without a stated condition a compatibility shim is permanent by
default.

- `DisplayPreset::logo`, accepted and never drawn since protocol version 4. It
  is `skip_serializing`, so no file written by version 4 or later carries it.
  It can be deleted once no configuration file at schema version 3 or earlier
  can remain, which is the same moment `CONFIG_SCHEMA_VERSION` stops accepting
  a version 3 document.
