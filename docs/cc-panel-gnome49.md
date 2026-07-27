# The Updates panel on GNOME 49: builds, appears, doesn't load

Status of `build-aux/test-cc-panel-in-toolbox.sh` as of 2026-07-27, run in the
Fedora 43 `finupdate` toolbox against gnome-control-center `gnome-49`.

## What works

The script does everything it claims:

* installs `libfinupdate` into the toolbox's writable `/usr/local`;
* clones gnome-control-center, vendors `cc-panel/panels/updates/`, applies the
  loader + meson patches;
* **builds successfully** — `[791/793] Merging translations for
  panels/updates/gnome-updates-panel.desktop` confirms the panel compiles into
  the binary alongside the upstream panels (17 `cc_updates_panel` symbol
  references in the resulting executable);
* stages a prefix and writes a `run-cc.sh` launcher;
* regenerates `build-aux/cc-panel-patches/{cc-panel-loader,panels-meson}.patch`
  for the Dakota override element.

Launched under Broadway, GNOME Settings comes up and **"Software Updates"
appears at the top of the sidebar** with its icon and label
(`tests/gui/screenshots/light/cc-settings-sidebar.png`).

## What doesn't

Clicking the row selects it, but the content pane stays on whatever was
previously shown. The log says:

```
WARNING: The direct access to `updates` is now deprecated. Please, use `system updates` instead.
WARNING: Invalid subpage: 'updates'
```

## Root cause

`shell/cc-window.c:369` — when a panel resolves to `CC_CATEGORY_SYSTEM`, the
shell **rewrites the panel id to `"system"`** and passes the original id as a
*subpage* parameter:

```c
if (category == CC_CATEGORY_SYSTEM)
  {
    param_str = g_strdup_printf ("[<'%s'>]", start_id);
    system_param_overwrite = g_variant_new_parsed (param_str);
    g_warning ("The direct access to `%s` is now deprecated. ...", start_id, start_id);
    start_id = "system";
  }
```

`shell/cc-panel.c:123` then fails, because the System panel has no `updates`
subpage — GNOME 49 consolidated About / Date & Time / Region / Users into
System, and that is the machinery doing it.

The category is derived by `parse_categories()` from the `.desktop` file's
`Categories=` line. Ours declares:

```
Categories=GNOME;GTK;Settings;X-GNOME-Settings-Panel;X-GNOME-SystemSettings;
```

`X-GNOME-SystemSettings` is exactly what `panels/system/datetime` uses — i.e.
we are declaring ourselves a System *subpage*. A genuine top-level panel such
as `panels/display` uses `X-GNOME-DevicesSettings` instead.

So the panel is being correctly classified according to what it asks for; it
just asks for the wrong thing.

## The fix

Change `cc-panel/panels/updates/gnome-updates-panel.desktop.in` to a
non-System category — most likely `X-GNOME-DevicesSettings`, or whichever
`parse_categories()` branch places it where you want it in the sidebar
ordering.

Worth checking at the same time: `default_subpages[]` in
`shell/cc-panel-loader.c` lists the ids that *are* System subpages (`about`,
`datetime`, `region`, `users`). `updates` is deliberately not one of them, which
is consistent with wanting a top-level panel.

This was not a problem when the panel was first written — GNOME 49 reorganised
the shell. It is a one-line change, but it needs a rebuild to confirm, so it is
recorded here rather than guessed at.
