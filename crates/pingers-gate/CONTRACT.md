# pingers-gate contract

Implemented in full, with a test per path. The only place that decides whether to share.

## Fixed

- `decide(&GateInputs) -> GateDecision`.
- `GateInputs { location_sharing, pause, focus, quiet_hours, local_time, zones }`, with
  `LocationSharing`, `PauseState`, `FocusState` (`On`, `Off`, `Unknown`), `QuietHoursSetting`,
  `QuietHours` (start inclusive, end exclusive, may wrap midnight), `MinuteOfDay`, and
  `pingers_geo::ZoneStatus` for zones.
- `GateDecision::{Share, Suppress(SuppressReason)}`; `SuppressReason` in priority order:
  `LocationSharingOff`, `Paused`, `Focus`, `QuietHours`, `ExclusionZone`.
- `GateError`.

## Implementer decides

- Nothing structural. Add inputs only by adding fields to `GateInputs` and variants to
  `SuppressReason` (both changes are breaking for callers, on purpose).

## Decisions

- **`FocusState::Unknown` does not suppress.** Suppressing would make the app useless where the
  platform will not report DND; the design instead says the app tells the user and offers the
  manual pause (B.10), and the core raises `DndUnavailable`.
- **Quiet hours come from local time supplied by the caller**, as a minute of the day. Time
  zones and DST are the shell's concern. Quiet hours are in the architecture but not the design.
- **One reason is reported** when several apply, in the order above.
- pingers-presence and pingers-meetup cannot depend on this crate (same layer), so they take
  their own two-valued `Sharing` input; pingers-wake maps `GateDecision` onto it.
