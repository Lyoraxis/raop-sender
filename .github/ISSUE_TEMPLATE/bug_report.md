---
name: Bug report
about: Something does not play, drops out, or sounds wrong
labels: bug
---

## What happens

<!-- Silence? Static? Dropouts? Be specific about what you hear. -->

## `--doctor` output

Run this first and paste the whole block. It names the failing precondition, and it
plays nothing:

```
raop-sender --doctor --host <receiver>:5000
```

## Receiver

| | |
|---|---|
| Make / model | |
| `am` (model string, if known) | |
| Advertises AirPlay 1? | yes / no / unknown |

If you can reach the receiver's `_raop._tcp` TXT record, paste it: `cn`, `et`,
`ek` and `features` decide what is even possible.

## Behaviour

- [ ] Silent, but every RTSP request returns `200 OK`
- [ ] Loud static instead of audio
- [ ] Audio but heavy dropouts
- [ ] Works for a while, then stops
- [ ] Never connects

## Log

Attach the tail of `daemon.log` (next to `raop-daemon.exe`) or `last-run.log` for
the GUI:

```
paste here
```

The useful lines are `capturing …`, `buffer primed with … ms`,
`audio detected (peak …)`, `pipeline delay … ms`, `peak …` (the level actually
being sent, which distinguishes silence from music), and anything with
`volume before:` — `-144.000000` means the receiver is muted or asleep.

## Environment

| | |
|---|---|
| Windows version | |
| Rust version (`rustc --version`) | |
| Virtual cable | VB-CABLE / other |
| Receiver reachable on the LAN | yes / no |
