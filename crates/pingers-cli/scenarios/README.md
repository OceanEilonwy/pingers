# Scenario files

A scenario is a TOML file describing a Party, its devices and a timeline of what happens to
each device. `pingers-cli scenario run FILE` replays it on pingers-testkit's fake platforms and
a manual clock: nothing waits in real time, and the same file gives the same run every time.

The files here are the design's sequences G.3 to G.7, bundled into the binary
(`pingers-cli e2e g3`, `e2e all`).

## Top level

```toml
name = "g3-enter-spot-alone"   # required; shown in reports
description = "..."            # optional
start = 1763000100             # optional; Unix seconds at offset 0 (default: testkit TEST_START,
                               # a bucket boundary)
seed = 3                       # optional; seeds the Party's ids (default 0). The config file's
                               # `seed` overrides it.
```

## Party and devices

```toml
[party]
members = ["alice", "bob"]     # required; every member is a device, leaves in this order

[[party.spots]]                # any number; circles only for now
name = "anchor"
latitude = 51.5007
longitude = -0.1246
radius_m = 150

[devices.bob]                  # optional, per member
focus = "off"                  # "off" (default), "on", "unknown"
push = "fcm"                   # "fcm" (default), "unified_push", "none"
```

## Steps

`[[steps]]` entries run in file order. Each has `at` (seconds after `start`, never less than the
previous step's), `device` (a member), and **exactly one** action:

| Action | Meaning |
|---|---|
| `fix = { spot = "anchor", position = "inside" }` | Queue a fix as the answer to the device's next fix request. `position` is `inside` (the Spot's centre) or `outside` (beyond the exit buffer). |
| `fix = { latitude = 51.5, longitude = -0.12, accuracy_m = 20 }` | The same with an explicit position (`accuracy_m` defaults to 10). |
| `location = { ... }` | An unsolicited location update, delivered now. Same forms as `fix`. |
| `geofence = { region = "anchor", transition = "enter" }` | The OS reports a geofence transition (`enter` or `exit`) for a Spot. |
| `focus = "on"` | The interruption filter changes (`on`, `off`, `unknown`); the core is told to re-read it. |
| `wake = "push"` | A push wakes the device. |
| `expect = { ... }` | A check; see below. Does not deliver anything. |

Fixes carry the step's time as `taken_at`.

### What the runner does around each step

1. Advance the clock to the step's time. If a device's core scheduled a background wake that falls
   due on the way, stop there and deliver it (reported as `scheduled wake fell due`).
2. Apply the step and deliver its input to the device's core, then let all clients settle.
3. If the core asked for fixes and the script had answers, deliver them and settle again, until
   no answered fixes are left (at most 16 rounds per step).

### Expectations

| `expect =` | Holds when |
|---|---|
| `{ location_mode = "dwell" }` | The core's last location mode is this (`idle`, `dwell`, `meetup_background`, `meetup_foreground`). |
| `{ wake_pending = true }` | A background wake is (or is not) scheduled. |
| `{ wake_within_s = 900 }` | A wake is scheduled and due within this many seconds. |
| `{ geofences = 2 }` | This many OS geofences are registered. |
| `{ fix_requests = 1 }` | The core has asked for this many fixes so far. |
| `{ event = "meetup_started" }` | At least one such event so far. |
| `{ no_event = "meetup_ended" }` | No such event so far. |
| `{ notice = "unanswered_knock" }` | At least one such notice so far. |

Event names: `parties_changed`, `join_requested`, `meetup_started`, `meetup_updated`,
`meetup_message`, `peer_location`, `location_request`, `location_access_changed`, `meetup_ended`,
`notice`, `privacy_status_changed`. Notice names: `unanswered_knock`, `board_equivocation`,
`clock_skew`, `dnd_unavailable`, `relay_key_mismatch`, `board_too_large`, `push_downgraded`,
`fallback_active`.

A failed expectation is reported and makes the run exit non-zero; the remaining steps still run.
`--dry-run` replays the timeline without clients and reports every expectation as skipped.

Unknown keys anywhere are errors, so a typo cannot silently drop a step.
