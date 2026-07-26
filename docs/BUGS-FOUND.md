# Bugs found while standing up the screenshot/validation harness

All found by actually running the app under Broadway on `himachal` and reading
what it did, rather than by inspection. Ordered by user impact.

---

## 1. Every GUI update was silently simulated ✅ fixed

**Symptom:** the app ran in Developer Mode permanently, so no update, rebase, or
reboot the GUI performed ever touched the system.

**Cause:** two compounding defects.

* `main.rs` applied `--dev-mode` by mutating `settings.json` and calling
  `save()`. Running `finupdate --dev-mode` **once** left developer mode on
  forever. `himachal:~/.config/finupdate/settings.json` was found with
  `"dev_mode": true` written into it.
* `Settings::default()` set `dev_mode: is_dev_build`, and `is_dev_build` is true
  whenever `config::PROFILE` is empty — which is the case for *any* plain
  `cargo build`. So a locally built binary could never exercise the real
  orchestrator, registry, or rebase paths at all.

**Fix:**
* CLI flags now layer through `settings::RuntimeOverrides`, held in memory and
  never written back (`settings.rs`). Invoking the app with a test flag can no
  longer change stored configuration.
* The dev-build default is now `dry_run: true`, not `dev_mode: true`. Real code
  paths run; the `privileged()` chokepoint withholds the destructive command at
  the point of execution.
* Added `--no-dev-mode` so an already-polluted `settings.json` can be escaped.

**Note for you:** your real config on himachal still has `dev_mode: true`. The
test harness now uses an isolated `XDG_CONFIG_HOME`, so it is untouched — but
your interactive runs will keep simulating until that value is cleared.

---

## 2. Startup storm: 1213 changelog fetches and 1216 SBOM diffs per launch ✅ fixed

**Symptom:** the window frequently never painted at all. The process sat at
100% CPU with ~1261 threads, and GHCR/GitHub calls timed out — which then made
every *subsequent* run worse, because the API rate limits were exhausted.

**Cause:** `AvailableTagsLoaded` repopulated the tag `StringList` with
`remove(0)` in a loop followed by `append` per item. Each mutation moves the
combo row's selection, firing `connect_selected_notify` — roughly 2N times for
N tags. Every one of those carried a *different* raw tag, so the existing
idempotency guard in `SelectTag` (which only compares against the current tag)
let all of them through, and each spawned a full changelog fetch + SBOM diff.

`ghcr.io/ublue-os/bluefin` publishes 612 tags, giving ~1213 fetches on a single
launch.

**Fix** (`status_view.rs`): block the `selected_notify` handler across the
repopulation, replace the remove/append loop with a single `splice()`, then
restore the selection and unblock. Handler id is stored as `tag_row_handler`.

**Measured, same launch, before → after:**

| | before | after |
|---|---|---|
| changelog fetches | 1213 | 1 |
| SBOM diffs | 1216 | 2 |
| log lines | 14 293 | 26 |
| threads | 1261 | 11 |
| main thread state | `R` (spinning) | `S` (idle) |

---

## 3. Ten ad-hoc tokio runtimes → thread exhaustion ✅ fixed

**Symptom:** `OS can't spawn worker thread: Resource temporarily unavailable`,
surfacing as a panic deep inside hyper's DNS resolver — far from the cause.

**Cause:** the GLib↔tokio bridge was open-coded at ten-plus call sites
(`app.rs` ×3, `rebase_dialog.rs` ×3, `status_view.rs` ×3, `rebase_widget.rs`,
`changelog_widget.rs`), each building a *fresh* runtime with its own worker and
blocking pools. Some sat in per-row rendering code, so the counts multiplied.

**Fix:** new `src/runtime.rs` — one shared multi-threaded runtime with bounded
pools (4 workers, 32 blocking threads), and a `block_on` that picks the right
strategy for the calling context. Ad-hoc runtimes removed.

`ffi.rs` was already correct (one runtime per `Handle`) and was left alone.

---

## 4. Panic: "Cannot start a runtime from within a runtime" ✅ fixed

**Symptom:** intermittent crash on launch — only when a background fetch
happened to race UI construction.

**Cause:** `detect_bootc_image_info` built a runtime and called `block_on`. Its
doc comment asserted "every caller here runs on the GTK thread", but the
changelog path reaches it via `read_selected_tag()` from *inside* the runtime,
where `block_on` panics.

**Fix:** route through `runtime::block_on`, which uses `block_in_place` when
already inside the runtime. Also memoised the whole function
(`BOOTC_IMAGE_INFO_CACHE`) — it was re-running the full detection chain,
including a `bootc status` subprocess, once per rendered version row.

---

## 5. GApplication rejected the app's own CLI flags ✅ fixed

**Symptom:** `finupdate --dry-run` exited with `Unknown option --dry-run`
*after* logging that it had accepted the flag.

**Cause:** flags were parsed by hand, then the full `argv` was handed to
`RelmApp`, and GApplication parses argv itself and aborts on anything it does
not recognise.

**Fix:** pass only `argv[0]` to `RelmApp::with_args` — every flag has already
been consumed into `RuntimeOverrides` by that point.

---

## 6. Window cannot reach the HIG minimum width ⚠️ open

`AdwApplicationWindow`'s `width-request` is now 360 and a breakpoint is
installed, but setting `FINUPDATE_WINDOW_SIZE=360x640` still yields a window
noticeably wider than 360px: **some child's minimum width dominates**. Likely
candidates are the non-wrapping action-row subtitles (e.g. "System image,
Flatpak, Homebrew, and Distrobox").

Needs per-row `set_title_lines` / `set_subtitle_lines` (or ellipsizing) plus an
audit of fixed-width children before the app is honestly adaptive. Tracked as
finding #1 in `GNOME-HIG-AUDIT.md`.

---

## 7. Late async result panics after component teardown ⚠️ open

```
The runtime of the component was shutdown. Maybe you accidentally dropped a
controller?: AvailableTagsLoaded([...])
```

A registry fetch that completes after its relm4 component is dropped sends into
a closed channel and panics the worker thread. Much less frequent since #2
reduced the number of in-flight fetches from ~1200 to 1, but the race is still
there. Fix is to use `sender.send(...)` fallibly (ignoring the error) rather
than the panicking variant, or to hold a cancellation token per component.

---

## 8. Harness hazard: stale instances accumulate (not an app bug)

`pkill -x finupdate` does not match instances launched via `toolbox run`, so
repeated test launches left up to four processes alive. A leftover instance
keeps the D-Bus name and the Broadway surface, which presents as a **blank
screenshot** rather than an error — a trap worth knowing about when reading
failures. The launcher now matches on the full command line and warns if
anything survives.
