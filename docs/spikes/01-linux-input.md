# Spike 1: Linux input under Proton

## Question

> Prove the overlay can capture mouse/keyboard in edit mode and pass it back to Deadlock
> afterwards, on KDE and GNOME Wayland sessions (the game itself runs through XWayland).
> (Session matrix per plan Amendment B.)

## Verdict

In progress.

## Evidence

Pass criteria (Task 7):
- P1: enter within 100 ms.
- P2: in edit mode the game sees nothing.
- P3: clicks (10/10) and typing reach widgets.
- P4: drag works.
- P5: clean exit, 20 cycles, no stuck keys.
- P6: the game doesn't minimise, pause or lose focus or mouse capture.

### H1 (XInput2 raw Ctrl+Tab, read-only, in the layer) + C1 (transparent always-on-top capture window from the host)

| Cell | P1 | P2 | P3 | P4 | P5 | P6 | Notes |
|---|---|---|---|---|---|---|---|
| KDE Wayland | ✅ instant | ✅* | ✅ | ✅ | ✅ (62 cycles) | ❌ | *The game ignores input only because it lost focus. The overlay window becomes the active window and Deadlock a background window (owner: "the overlay is not attached to the game"). |
| GNOME Wayland | not run | | | | | | C1 already fails P6 |

- H1 works: Ctrl+Tab is seen through XInput2 raw events from inside the game process while the
  game (an XWayland client) has focus. It also works while the XWayland capture window has focus.
- C1 verdict: **fails P6**. Dropped in favour of C2.

## Mechanism chosen

## Risks

## Not tested and why

## Decisions needed from owner
