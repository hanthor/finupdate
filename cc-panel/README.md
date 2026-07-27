# gnome-control-center "Updates" panel — integration kit

This directory contains the C source for an `updates` panel that
gnome-control-center can render alongside its built-in panels (System,
About, etc.). It's the implementation of Path 1 in
[`docs/control-center-integration.md`](../docs/control-center-integration.md).

## What it does

When integrated, GNOME Settings gains a new sidebar entry called **Software
Updates** that:

- Shows the booted image identity (title + full registry ref + short
  digest + build date) in a hero row.
- Has a "Check" button that probes the registry via the finupdate
  backend and reports "Up to date" / "Update available".
- (Future iterations) embeds the changelog page and rebase dialog from
  the standalone finupdate app.

## Layout

```
cc-panel/
├── README.md                                 ← you are here
├── libfinupdate.pc.in                        ← pkg-config for the cdylib
└── panels/
    └── updates/
        ├── cc-updates-panel.h
        ├── cc-updates-panel.c                ← CcPanel implementation
        ├── cc-updates-panel.ui               ← GtkBuilder layout
        ├── updates.gresource.xml             ← bakes the .ui into the binary
        ├── gnome-updates-panel.desktop.in    ← Settings sidebar entry
        └── meson.build                       ← drop-in for cc's panels/
```

## Integration steps (for downstream packagers — Bluefin / Dakota)

### 1. Build and install `libfinupdate`

The panel links against the Rust cdylib built from this crate's `[lib]`
target. From the repo root:

```sh
# Builds libfinupdate.so + generates finupdate.h + installs both into
# /usr/local along with libfinupdate.pc.
sudo build-aux/install-libfinupdate.sh /usr/local
```

To verify:

```sh
pkg-config --libs libfinupdate
# → -L/usr/local/lib -lfinupdate
```

### 2. Vendor `cc-panel/panels/updates/` into the gnome-control-center source tree

Inside a downstream gnome-control-center checkout (e.g. the patched
package source Bluefin/Dakota maintains):

```sh
cp -r /path/to/finupdate/cc-panel/panels/updates panels/
```

### 3. Register the panel in cc's loader

gnome-control-center keeps a static registry of panels in
`shell/cc-panel-loader.c` (the `default_panels` array). Add:

```c
extern GType cc_updates_panel_get_type (void);
…
PANEL_TYPE("updates",         cc_updates_panel_get_type),
```

near the other entries, and `panels/updates/cc-updates-panel.h` to the
includes.

### 4. Hook the meson build

In `panels/meson.build` add `subdir('updates')` alongside the other
panels. The vendored `panels/updates/meson.build` already does the right
thing — it builds a `static_library` and appends to `panels_libs`, which
is how every other panel is built.

### 5. Build gnome-control-center

```sh
PKG_CONFIG_PATH=/usr/local/lib/pkgconfig:$PKG_CONFIG_PATH \
  meson setup builddir
meson compile -C builddir
```

The resulting `gnome-control-center` binary statically links the
`updates` panel into the same blob as the upstream panels — there's no
runtime plugin discovery.

### 6. Build at the **release** profile — and check it isn't in dry-run

`cc-updates-panel.c` embeds the *entire* app through
`finupdate_panel_widget_new`, so the panel inherits finupdate's settings
defaults wholesale. That makes the meson profile load-bearing for shipping:

```sh
meson setup builddir              # -Dprofile=default → PROFILE=''  (release)
meson setup builddir -Dprofile=development   # → PROFILE='Devel'   (dry-run)
```

Only `Devel` enables dry-run by default. A build that comes up in dry-run looks
completely normal but **withholds every privileged command**, so the panel
appears to work while quietly refusing to update anything — with no error to
chase. Confirm on first launch that the panel does *not* show the
"Dry run — actions are recorded, your system is not modified" banner.

(An earlier version of `Settings::default()` keyed dry-run off
`PROFILE == "Devel" || PROFILE.is_empty()`, which caught the release build too.
That is fixed, and `only_an_explicit_devel_profile_counts_as_a_dev_build`
guards it.)

### 7. Ship the patched package

Replace the system `gnome-control-center` with the patched build. On
Bluefin/Dakota this lands as a Containerfile step in the image build:

```dockerfile
RUN dnf install -y gnome-control-center-finupdate \
    && rm -f /var/cache/dnf/*
```

(where `gnome-control-center-finupdate` is the patched RPM the Bluefin
build process produces).

## Development workflow

For iterating on the panel without rebuilding the full
gnome-control-center every cycle, you can:

1. Build cc with the panel once: produces `builddir/shell/gnome-control-center`.
2. Edit `cc-updates-panel.c` / `.ui`.
3. `meson compile -C builddir` and run `builddir/shell/gnome-control-center updates`.

If you need to change the C ABI surface (i.e. `src/ffi.rs` in the
finupdate repo), re-run `build-aux/install-libfinupdate.sh` to reinstall
the cdylib + header, then rebuild cc.

## What's implemented

Out of date below — `finupdate_panel_widget_new` now returns the whole
`UpdatesPanel`, so the panel already renders the full app: hero row, update
check, automatic-updates toggle, the Advanced subpage, and the
`AdwNavigationView` drill-downs (Image Source, Image History, What's New).
It also inherits the adaptive layout work, so it shrinks with
gnome-control-center's own breakpoints rather than forcing the Settings window
wider.

**Verified.** `examples/panel-entry-demo/` calls
`finupdate_panel_widget_new` — the exact entry point `cc-updates-panel.c` uses
— and hosts it in a bare `AdwApplicationWindow`, the closest stand-in for
gnome-control-center's content area. Captured at
`tests/gui/screenshots/light/cc-panel-entry-point.png`.

Two things that screenshot confirms:

* The panel renders **without the app's own header bar**, deferring to the host
  container — which is what a cc panel must do.
* Run with `dry_run: false` and no `--dry-run` flag, it shows **no dry-run
  banner**, i.e. a release build comes up able to act. That is the check from
  step 6 above, and the reason it exists: an earlier `Settings::default()`
  would have left every shipped panel silently inert.

(`examples/panel-demo/` remains the widget-level harness — it composes
`finupdate_changelog_widget_new` and `finupdate_rebase_widget_new` for
iterating on those individually.)

## Older notes on remaining work

- Wire the rebase dialog as a Cc subpage (adw::NavigationView).
- Embed the changelog "What's New" widget (`status_view.rs`'s
  `rebuild_changelog_page`) — needs the widget builder factored out of
  the standalone app and exposed via FFI as a returned GTK widget.
- Stream the per-module update progress (`SegmentedProgress`) into
  the panel during an apply-update flow.
- Schedule reboot integration (`pkexec shutdown -r 02:00`).
- Polkit policy for the panel's privileged actions (separate from
  finupdate's existing polkit rules).

See `docs/control-center-integration.md` § "Prep work that helps all
paths" for the design considerations the next iterations need to
address.
