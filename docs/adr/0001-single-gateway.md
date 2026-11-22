# ADR 0001: One gateway connection in the edge

Status: accepted

## Context

Discord allows multiple gateway sessions for one application, but each session
receives the full event stream for every guild. Running a gateway per module
multiplies bandwidth, identify pressure, and event deserialization, and it
makes duplicate command registration and capability conflicts invisible until
Discord rejects them.

## Decision

`rsbot-edge` owns the only gateway connection. Modules own `twilight-http`
clients with the same application token, independent connection pools, and
independent rate-limit buckets. The edge converts interactions into a compact
internal payload and the supervisor routes them to the owning module.

"Each module has its own login" therefore means an independently authenticated
REST client, not a separate Discord application or bot identity. Separate
identities would require separate applications and are out of scope.

## Consequences

- Command registration, guild allowlisting, and incompatibility detection have
  exactly one authority.
- The edge is a single point of failure; the supervisor restarts it and keeps
  modules running. Gateway resume is delegated to twilight.
- Modules cannot receive gateway events they did not declare; only the events
  the supervisor forwards.
- A future egress broker can remove the token from modules without changing
  this topology.