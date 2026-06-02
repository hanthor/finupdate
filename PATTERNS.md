# Bluefin Utility App Patterns

> Architectural patterns extracted from **Finupdate** — the first app in the Bluefin utility suite.
> Every future Bluefin app should start from these patterns for consistency.

---

## 1. Project Scaffold

### Cargo.toml Template

```toml
[package]
name = "your-app-name"
version = "0.1.0"
edition = "2024"
rust-version = "1.85"

[dependencies]
# GTK4 bindings — always use the `gtk4` package aliased as `gtk`
gtk = { version = "0.11", package = "gtk4", features = ["v4_16"] }

# libadwaita — enable version feature for the minimum libadwaita you target
adw = { version = "0.9", package = "libadwaita", features = ["v1_7"] }

# Relm4 with libadwaita feature — this replaces gtk::Application with adw::Application
relm4 = { version = "0.11", features = ["libadwaita"] }

# Tokio — only include features you actually need
tokio = { version = "1", features = ["rt-multi-thread", "process", "io-util", "sync", "macros"] }

# Structured logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

### Why these choices:
- **`gtk4` aliased as `gtk`**: matches upstream convention and all documentation
- **`libadwaita` aliased as `adw`**: matches the C library namespace
- **`v4_16` / `v1_7` features**: gates APIs to the latest stable GNOME 47+ release
- **Edition 2024**: latest Rust edition for modern syntax (e.g., `use` in closures)
- **relm4 `"libadwaita"` feature**: ensures `adw::init()` is called before any widget creation
- **Tokio**: needed for async subprocess/network I/O — GTK's own async (gio) is harder to use from Rust

### Crate Structure

```
src/
├── main.rs              # Entry point: logging + relm4 app launch
├── config.rs            # Build-time constants (APP_ID, VERSION, PROFILE)
├── config.rs.in         # Meson template that generates config.rs
├── app.rs               # Top-level component (owns the window)
├── ui/
│   ├── mod.rs           # Re-exports child components
│   ├── status_view.rs   # State-driven content switcher
│   └── log_view.rs      # Scrollable text output
└── <domain>_worker.rs   # Async logic (subprocess, network, etc.)
data/
├── org.projectbluefin.<AppName>.desktop.in   # Template with @APP_ID@
├── org.projectbluefin.<AppName>.metainfo.xml.in
└── icons/
    ├── org.projectbluefin.<AppName>.svg
    └── org.projectbluefin.<AppName>-symbolic.svg
```

### Build System (Meson + Flatpak)

Follow the standard GNOME Flatpak convention:

```
meson.build                   — top-level project definition + deps
meson_options.txt             — build profile option
src/meson.build               — cargo build integration via custom_target
data/meson.build              — install desktop, metainfo, icons
build-aux/
├── org.projectbluefin.<App>.json       — release Flatpak manifest
├── org.projectbluefin.<App>.Devel.json — dev Flatpak manifest
└── dist-vendor.sh                      — vendor deps for `meson dist`
```

**Build & run locally (Flatpak):**
```bash
# One-time setup (GNOME 50 / freedesktop 25.08)
flatpak install flathub org.gnome.Sdk//50 org.gnome.Platform//50
flatpak install flathub org.freedesktop.Sdk.Extension.rust-stable//25.08

# Build and run
flatpak run org.flatpak.Builder --user --install --force-clean _flatpak \
  build-aux/org.projectbluefin.Finupdate.Devel.json
flatpak run org.projectbluefin.Finupdate.Devel
```

**Data file templates**: Desktop and metainfo files use `.in` suffix and `@APP_ID@` / `@ICON@` placeholders. Meson's `configure_file()` substitutes these and renames the output to include the profile suffix (`.Devel`) so Flatpak export correctly associates files with the app ID.

**Build without Flatpak (direct meson):**
```bash
meson setup _build
meson compile -C _build
./_build/src/finupdate
```

---

## 2. App Entry Point Pattern

```rust
const APP_ID: &str = "org.projectbluefin.YourApp";

fn main() {
    // 1. Structured logging (respects RUST_LOG)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // 2. RelmApp handles adw::init() and GLib main loop
    let app = relm4::RelmApp::new(APP_ID);
    app.run::<AppComponent>(());
}
```

### Rules:
- **APP_ID must match** the `.desktop` filename and metainfo `<id>` exactly
- **Never call `gtk::init()` manually** — relm4 + libadwaita feature handles it
- **Never create widgets before `app.run()`** — Adwaita CSS hasn't loaded yet

---

## 3. Main Window Pattern

```rust
view! {
    adw::ApplicationWindow {
        set_title: Some("Window Title"),
        set_default_size: (600, 500),
        set_width_request: 360,   // HIG minimum
        set_height_request: 360,  // HIG minimum

        adw::ToolbarView {
            add_top_bar = &adw::HeaderBar {},

            #[wrap(Some)]
            set_content = &adw::ToastOverlay {
                // Your content here
            },
        },
    }
}
```

### Key decisions:
- **`AdwToolbarView`** (not raw Box + HeaderBar): handles header integration with scrolling content, unfloating on scroll, etc.
- **`AdwToastOverlay`** wraps content: allows any component to show transient notifications without prop-drilling
- **`AdwHeaderBar`** (not `gtk::HeaderBar`): respects the Adwaita style and integrates with AdwToolbarView
- **When to use `AdwNavigationView`**: multi-page flows (wizards, settings with subpages). Single-view apps use ToolbarView alone.

---

## 4. Async Subprocess Pattern

### Architecture

```
┌──────────────────┐     mpsc::channel      ┌─────────────────┐
│  relm4 Component │ ◄──────────────────────│  Worker (tokio)  │
│  (GTK main loop) │                         │  (std::thread)   │
│                  │  sender.emit(AppMsg)    │                  │
└──────────────────┘                         └─────────────────┘
```

### Why this approach:
1. **Separate thread for tokio**: GTK owns the main thread. Tokio needs its own runtime.
2. **mpsc channel from worker → component**: decouples I/O from UI, makes worker testable in isolation.
3. **`sender.emit()`**: thread-safe; queues messages to the GLib main loop for processing.

### Worker implementation:

```rust
pub struct Worker { /* command, args */ }

pub enum WorkerEvent {
    Output(String),
    Complete,
    Error(String),
}

impl Worker {
    pub async fn run(&mut self) -> mpsc::UnboundedReceiver<WorkerEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            // Spawn process, stream lines, send events via tx
        });
        rx
    }
}
```

### Connecting to the component:

```rust
// In the component's update() method:
let input_sender = sender.input_sender().clone();
std::thread::spawn(move || {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        let mut worker = Worker::new();
        let mut rx = worker.run().await;
        while let Some(event) = rx.recv().await {
            input_sender.emit(/* convert event to AppMsg */);
        }
    });
});
```

### Alternative: relm4 `CommandOutput`
For simpler cases, relm4's `Component::CommandOutput` type + `sender.command()` works well. Use the thread + channel pattern when you need streaming (many events over time) rather than a single result.

### Flatpak Sandbox Awareness

When running inside Flatpak, commands targeting the host must use `flatpak-spawn --host`:

```rust
fn is_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

fn build_command(program: &str, args: &[String]) -> tokio::process::Command {
    if is_flatpak() {
        let mut cmd = Command::new("flatpak-spawn");
        cmd.arg("--host").arg(program).args(args);
        cmd
    } else {
        let mut cmd = Command::new(program);
        cmd.args(args);
        cmd
    }
}
```

**Manifest requirement**: The Flatpak manifest must include `--talk-name=org.freedesktop.Flatpak` in `finish-args` for `flatpak-spawn --host` to work.

### Cancellation Pattern

Use a `tokio::sync::oneshot` channel to signal cancellation to a running worker:

```rust
// In model:
cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,

// When starting:
let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
self.cancel_tx = Some(cancel_tx);

// In the worker thread, use tokio::select!:
tokio::select! {
    event = rx.recv() => { /* handle event */ }
    _ = &mut cancel_rx => { /* cancelled — clean up */ }
}

// To cancel:
if let Some(tx) = self.cancel_tx.take() {
    let _ = tx.send(());
}
```

---

## 5.5. Action Groups & Menus

### relm4 action pattern for hamburger menus:

```rust
use relm4::actions::{RelmAction, RelmActionGroup};

// Define action group and action types (at module level):
relm4::new_action_group!(WindowActionGroup, "win");
relm4::new_stateless_action!(AboutAction, WindowActionGroup, "about");

// In view! macro — reference the action type directly:
menu! {
    main_menu: {
        "_About" => AboutAction,
    }
}

// In init() — create and register the action:
let about_action: RelmAction<AboutAction> = {
    let sender = sender.input_sender().clone();
    RelmAction::new_stateless(move |_| {
        sender.emit(AppMsg::ShowAbout);
    })
};
let mut group = RelmActionGroup::<WindowActionGroup>::new();
group.add_action(about_action);
group.register_for_widget(&root);
```

### AdwAboutDialog:

```rust
let about = adw::AboutDialog::builder()
    .application_name("App Name")
    .application_icon(APP_ID)
    .developer_name("Project Bluefin")
    .version(VERSION)
    .website("https://projectbluefin.io")
    .issue_url("https://github.com/org/repo/issues")
    .license_type(gtk::License::MitX11)
    .developers(vec!["Contributors"])
    .build();
about.present(Some(window));  // takes Option<&impl IsA<gtk::Widget>> in 0.9+
```

**Import**: Requires `use adw::prelude::*` for the `AdwDialogExt::present()` method.

### Window Close Guard:

Prevent the window from closing during destructive operations:

```rust
// In init(), connect to the close-request signal:
root.connect_close_request(move |_| {
    if *state_ref.borrow() == AppState::Updating {
        // Show a toast or warning, then inhibit close
        gtk::glib::Propagation::Stop
    } else {
        gtk::glib::Propagation::Proceed
    }
});
```

### Reboot Confirmation (AdwAlertDialog):

```rust
let dialog = adw::AlertDialog::builder()
    .heading("Restart System?")
    .body("A restart is required to apply the update.")
    .build();
dialog.add_response("cancel", "Cancel");
dialog.add_response("restart", "Restart");
dialog.set_response_appearance("restart", adw::ResponseAppearance::Destructive);
dialog.connect_response(None, move |_, response| {
    if response == "restart" {
        // Execute reboot
    }
});
dialog.present(Some(&window));
```

---

## 5. Status/Feedback Pattern

### State-driven view switching with `gtk::Stack`

```rust
view! {
    gtk::Stack {
        set_transition_type: gtk::StackTransitionType::Crossfade,
        set_transition_duration: 200,

        add_child = &adw::StatusPage { /* idle state */ } -> { set_name: "idle" },
        add_child = &gtk::Box { /* active state */ } -> { set_name: "active" },
        add_child = &adw::StatusPage { /* error state */ } -> { set_name: "error" },
    }
}
```

Then in `update_view()`:
```rust
stack.set_visible_child_name(match &self.state {
    State::Idle => "idle",
    State::Active => "active",
    State::Error(_) => "error",
});
```

### AdwStatusPage — when and how:
- **Empty states**: "No items yet", "Nothing to show"
- **Error states**: with `dialog-error-symbolic` icon and retry button
- **Success states**: with `emblem-ok-symbolic`
- Always include `icon_name`, `title`, and optionally `description`

### AdwToast — transient notifications:
```rust
let toast = adw::Toast::new("Operation complete");
toast.set_timeout(5); // seconds; 0 = persistent until dismissed
self.toast_overlay.add_toast(toast);
```
- Use for: success confirmations, non-critical warnings
- Do NOT use for: errors that need user action (use StatusPage instead)

### Progress indication:
- **Determinate**: `gtk::ProgressBar` with `set_fraction()` when you know percentage
- **Indeterminate**: `gtk::ProgressBar` with `pulse()` on a timer when progress is unknown
- **Spinner**: `adw::Spinner` for small inline loading indicators

### Desktop Notifications (GNotification):
```rust
fn send_notification(id: &str, title: &str, body: &str) {
    let app = relm4::main_application();
    let notification = gtk::gio::Notification::new(title);
    notification.set_body(Some(body));
    notification.set_icon(&gtk::gio::ThemedIcon::new("system-software-update-symbolic"));
    app.send_notification(Some(id), &notification);
}
```
- Use a stable `id` so repeat notifications replace rather than stack
- Works in both Flatpak and native — no extra permissions needed

### Elapsed Timer Pattern:
```rust
// In model:
update_start: Option<std::time::Instant>,
elapsed_label: gtk::Label,

// Start timer when work begins:
self.update_start = Some(Instant::now());

// Periodic tick (every 250ms via glib::timeout_add_local):
if let Some(start) = self.update_start {
    let secs = start.elapsed().as_secs();
    self.elapsed_label.set_label(&format!("{}:{:02}", secs / 60, secs % 60));
}
```
- Use `gtk::glib::timeout_add_local()` — it runs on the main loop so UI updates are safe
- Stop the timer by checking state in the callback, not by removing the source

### Clipboard Pattern:
```rust
if let Some(display) = gtk::gdk::Display::default() {
    let clipboard = display.clipboard();
    clipboard.set_text(&self.log_text);
    // Confirm with toast
    self.toast_overlay.add_toast(adw::Toast::new("Copied to clipboard"));
}
```

---

## 6. HIG Compliance Checklist

Run through this before shipping any Bluefin utility app:

### Icons
- [ ] All icons use `-symbolic` suffix (e.g., `system-software-update-symbolic`)
- [ ] No hardcoded icon paths — only icon names from the system theme
- [ ] Window has an appropriate icon name set

### Spacing & Layout
- [ ] No hardcoded pixel values for margins/padding — use Adwaita CSS classes
- [ ] Text content is wrapped in `AdwClamp` (max ~800px) for readability
- [ ] Buttons use appropriate style classes: `suggested-action`, `destructive-action`, `pill`
- [ ] Content spacing uses multiples of 6px (Adwaita's base unit)

### Accessibility
- [ ] Every button has `set_accessible_label` if icon-only
- [ ] Interactive elements are keyboard-navigable (natural with GTK, but verify)
- [ ] Text views have appropriate `AccessibleRole` (e.g., `Log` for output)
- [ ] Color is never the only way to convey information

### Behavior
- [ ] Single-instance via GApplication (automatic with correct APP_ID)
- [ ] Window remembers size/position (AdwApplicationWindow handles this if you set an ID)
- [ ] Destructive actions have confirmation dialogs
- [ ] Long operations show progress and remain cancelable where possible
- [ ] Errors are shown in-context (StatusPage), not as modal dialogs

### Metadata
- [ ] `.desktop` file uses `@APP_ID@` template for icon and launcher
- [ ] AppStream metainfo passes `appstreamcli validate`
- [ ] App ID follows `org.projectbluefin.<AppName>` convention
- [ ] Content rating (`oars-1.1`) is present even if empty
- [ ] If `DBusActivatable=true`, a matching `.service` file is installed

### Dark Mode
- [ ] App respects system color scheme (automatic with libadwaita)
- [ ] No hardcoded colors anywhere — only Adwaita CSS variables
- [ ] Tested in both light and dark mode

---

## Quick Start: New App

```bash
# 1. Clone the template structure from finupdate
cp -r finupdate/ my-new-app/
cd my-new-app/

# 2. Rename in Cargo.toml, APP_ID, desktop/metainfo files
sed -i 's/Finupdate/MyNewApp/g' **/*.rs data/*
sed -i 's/finupdate/my-new-app/g' Cargo.toml

# 3. Replace the worker with your domain logic

# 4. Build & run
cargo run
```

---

## Appendix: Version Compatibility Matrix

| Crate | Version | Maps to |
|-------|---------|---------|
| gtk4-rs (`gtk`) | 0.11.x | GTK 4.16+ (GNOME 47+) |
| libadwaita-rs (`adw`) | 0.9.x | libadwaita 1.7+ (GNOME 47+) |
| relm4 | 0.11.x | Stable macro syntax |
| tokio | 1.x | Async runtime |

**Flatpak SDK versions**: GNOME Sdk 50 maps to freedesktop-sdk 25.08. The rust-stable extension must match the SDK base version (e.g., `rust-stable//25.08` for GNOME 50).

When GNOME 48+ ships new widgets, bump the `features = ["v1_8"]` in your libadwaita dep.

---

## Registry / Image Version History

### Dakota (and images using sha-only tags)

**Dakota switched from date-stamped tags to pure sha-only tags around 2026-02.**

Before the switch, builds were tagged `latest.20260212`, `latest.20260211`, etc. — parseable as dates directly. After the switch, every build is tagged with a 40-character git commit sha (e.g. `6ca4b317e092ef767de2526e048b7877205efed7`), plus floating `latest` / `testing` tags.

**Consequence for `fetch_versions` / sha-tag probing:**

`RegistryClient::fetch_versions` probes sha-only tags to recover build dates from OCI config-blob `created` timestamps. GHCR returns tags **alphabetically**, so sha hashes starting with `0`–`3` come first in the list. A naive `sha_tags[..N]` slice probes only 25% of the hash alphabet and misses all recent builds whose sha starts with `4`–`f`.

**Fix applied (v3 cache key):** Use stride-sampling (`step_by(stride)`) instead of a head-slice so each probe batch covers the full hash alphabet. The cache-key version is bumped whenever this logic changes so stale caches are invalidated automatically.

**SBOM diff:** Dakota images do not use the same floating tag for booted vs. target — if both resolve to `:latest` the diff is trivially empty. Always compare using the actual OCI digest from `bootc status` (`image@sha256:...`) for booted, and the newest `ImageVersion.full_ref` for target.

### libadwaita 0.9 breaking change:
`AdwDialog::present()` now takes `Option<&impl IsA<Widget>>`:
```rust
// Old (0.6):
about.present(&window);

// New (0.9):
about.present(Some(&window));
```
