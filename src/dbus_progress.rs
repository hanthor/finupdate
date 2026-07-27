//! D-Bus progress interface for the GNOME Shell panel extension.
//!
//! Exports `org.tunaos.finupdate.Progress` on the session bus
//! with three properties:
//!   - State (s): "idle" | "checking" | "updating" | "complete" | "error"
//!   - Progress (d): 0.0 to 1.0
//!   - Message (s): human-readable status text
//!
//! The GNOME Shell extension watches these properties and renders a
//! circular progress indicator in the panel (same area as Caffeine).

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

const BUS_NAME: &str = "org.tunaos.finupdate";
const OBJECT_PATH: &str = "/org/tunaos/finupdate";
const INTERFACE_NAME: &str = "org.tunaos.finupdate.Progress";

const INTERFACE_XML: &str = r#"
<node>
  <interface name="org.tunaos.finupdate.Progress">
    <property name="State" type="s" access="read"/>
    <property name="Progress" type="d" access="read"/>
    <property name="Message" type="s" access="read"/>
  </interface>
</node>
"#;

/// Internal mutable state for the D-Bus object.
#[derive(Debug, Clone)]
struct ProgressState {
    state: String,
    progress: f64,
    message: String,
}

impl Default for ProgressState {
    fn default() -> Self {
        Self {
            state: "idle".to_string(),
            progress: 0.0,
            message: String::new(),
        }
    }
}

/// Handle to the D-Bus progress publisher.
///
/// Clone-safe — all clones share the same underlying state and connection.
#[derive(Clone)]
pub struct ProgressDBus {
    state: Rc<RefCell<ProgressState>>,
    connection: Rc<RefCell<Option<gio::DBusConnection>>>,
}

impl ProgressDBus {
    /// Create a new progress publisher and register on the session bus.
    pub fn new() -> Self {
        let instance = Self {
            state: Rc::new(RefCell::new(ProgressState::default())),
            connection: Rc::new(RefCell::new(None)),
        };

        instance.register();
        instance
    }

    /// Set the current state and emit PropertiesChanged.
    #[allow(dead_code)]
    pub fn set_state(&self, state: &str) {
        self.state.borrow_mut().state = state.to_string();
        self.emit_properties_changed();
    }

    /// Set the progress fraction (0.0 to 1.0) and emit PropertiesChanged.
    pub fn set_progress(&self, progress: f64) {
        self.state.borrow_mut().progress = progress.clamp(0.0, 1.0);
        self.emit_properties_changed();
    }

    /// Set the human-readable message and emit PropertiesChanged.
    #[allow(dead_code)]
    pub fn set_message(&self, message: &str) {
        self.state.borrow_mut().message = message.to_string();
        self.emit_properties_changed();
    }

    /// Convenience: set state + progress + message in one call.
    pub fn update(&self, state: &str, progress: f64, message: &str) {
        {
            let mut s = self.state.borrow_mut();
            s.state = state.to_string();
            s.progress = progress.clamp(0.0, 1.0);
            s.message = message.to_string();
        }
        self.emit_properties_changed();
    }

    /// Reset to idle state.
    pub fn reset(&self) {
        self.update("idle", 0.0, "");
    }

    fn register(&self) {
        let state = self.state.clone();
        let connection_ref = self.connection.clone();

        gio::bus_own_name(
            gio::BusType::Session,
            BUS_NAME,
            gio::BusNameOwnerFlags::NONE,
            // on_bus_acquired
            {
                let state = state.clone();
                let connection_ref = connection_ref.clone();
                move |connection, _name| {
                    *connection_ref.borrow_mut() = Some(connection.clone());

                    let node_info = gio::DBusNodeInfo::for_xml(INTERFACE_XML)
                        .expect("Failed to parse D-Bus interface XML");
                    let interface_info = node_info
                        .lookup_interface(INTERFACE_NAME)
                        .expect("Interface not found in XML");

                    let state_for_get = state.clone();
                    let result = connection
                        .register_object(OBJECT_PATH, &interface_info)
                        .property(move |_conn, _sender, _path, _iface, prop_name| {
                            let s = state_for_get.borrow();
                            match prop_name {
                                "State" => s.state.to_variant(),
                                "Progress" => s.progress.to_variant(),
                                "Message" => s.message.to_variant(),
                                _ => "".to_variant(),
                            }
                        })
                        .build();

                    match result {
                        Ok(_id) => {
                            tracing::debug!("D-Bus progress interface registered at {OBJECT_PATH}");
                        }
                        Err(e) => {
                            tracing::warn!("Failed to register D-Bus object: {e}");
                        }
                    }
                }
            },
            // on_name_acquired
            |_connection, _name| {
                tracing::debug!("Acquired D-Bus name: {BUS_NAME}");
            },
            // on_name_lost
            |_connection, _name| {
                tracing::warn!("Lost D-Bus name: {BUS_NAME} — another instance running?");
            },
        );
    }

    /// Emit org.freedesktop.DBus.Properties.PropertiesChanged signal.
    fn emit_properties_changed(&self) {
        let conn = self.connection.borrow();
        let Some(ref connection) = *conn else { return };

        let s = self.state.borrow();

        // Build a{sv} for changed_properties
        let builder = glib::VariantDict::new(None);
        builder.insert("State", &s.state.as_str());
        builder.insert("Progress", &s.progress);
        builder.insert("Message", &s.message.as_str());
        let changed_properties = builder.end();

        // Empty string array for invalidated_properties
        let invalidated: &[&str] = &[];

        let _ = connection.emit_signal(
            None::<&str>,
            OBJECT_PATH,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            Some(&glib::Variant::tuple_from_iter([
                INTERFACE_NAME.to_variant(),
                changed_properties,
                invalidated.to_variant(),
            ])),
        );
    }
}
