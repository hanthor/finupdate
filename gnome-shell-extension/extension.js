/**
 * Finupdate Progress — GNOME Shell Extension
 *
 * Shows a circular progress indicator in the panel system status area
 * (same location as Caffeine) while system updates are running.
 * Uses the same QuickSettings.SystemIndicator + addExternalIndicator
 * pattern that Caffeine uses.
 *
 * Communicates with finupdate over D-Bus:
 *   Bus name: org.tunaos.finupdate
 *   Path:     /org/tunaos/finupdate
 *   Interface: org.tunaos.finupdate.Progress
 *   Properties:
 *     State    (s) — "idle" | "checking" | "updating" | "complete" | "error"
 *     Progress (d) — 0.0 to 1.0
 *     Message  (s) — human-readable status text
 */

'use strict';

import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Gio from 'gi://Gio';
import St from 'gi://St';

import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as QuickSettings from 'resource:///org/gnome/shell/ui/quickSettings.js';

const QuickSettingsMenu = Main.panel.statusArea.quickSettings;

const DBUS_NAME = 'org.tunaos.finupdate';
const DBUS_PATH = '/org/tunaos/finupdate';
const DBUS_IFACE = 'org.tunaos.finupdate.Progress';

// ─── Progress Circle Widget ──────────────────────────────────────────────────
// Custom St.DrawingArea that renders a circular progress indicator via Cairo.
// Supports: idle (hidden), checking (spinning), updating (filling arc),
// complete (green check), error (red X).

const ProgressCircle = GObject.registerClass(
class ProgressCircle extends St.DrawingArea {
    _init() {
        super._init({
            width: 16,
            height: 16,
            y_align: Clutter.ActorAlign.CENTER,
            style_class: 'finupdate-circle',
        });
        this._progress = 0;
        this._state = 'idle';
        this._pulseOffset = 0;
        this._pulseTimer = null;
    }

    set progress(value) {
        this._progress = Math.max(0, Math.min(1, value));
        this.queue_repaint();
    }

    get progress() {
        return this._progress;
    }

    set state(value) {
        this._state = value;
        if (value === 'checking' || (value === 'updating' && this._progress === 0)) {
            this._startPulse();
        } else {
            this._stopPulse();
        }
        this.queue_repaint();
    }

    get state() {
        return this._state;
    }

    _startPulse() {
        if (this._pulseTimer) return;
        this._pulseTimer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 50, () => {
            this._pulseOffset = (this._pulseOffset + 0.04) % 1.0;
            this.queue_repaint();
            return GLib.SOURCE_CONTINUE;
        });
    }

    _stopPulse() {
        if (this._pulseTimer) {
            GLib.source_remove(this._pulseTimer);
            this._pulseTimer = null;
        }
    }

    vfunc_repaint() {
        const cr = this.get_context();
        const [width, height] = this.get_surface_size();
        const cx = width / 2;
        const cy = height / 2;
        const radius = Math.min(cx, cy) - 1.5;
        const lineWidth = 2.0;

        // Clear canvas
        cr.setOperator(0); // CLEAR
        cr.paint();
        cr.setOperator(2); // OVER

        if (this._state === 'idle') {
            cr.$dispose();
            return;
        }

        // Background ring (subtle)
        cr.setLineWidth(lineWidth);
        cr.setSourceRGBA(1, 1, 1, 0.15);
        cr.arc(cx, cy, radius, 0, 2 * Math.PI);
        cr.stroke();

        const startAngle = -Math.PI / 2;

        if (this._state === 'checking' || (this._state === 'updating' && this._progress === 0)) {
            // Indeterminate spinner: rotating partial arc
            const pulseStart = startAngle + (this._pulseOffset * 2 * Math.PI);
            const pulseEnd = pulseStart + Math.PI * 0.75;
            cr.setSourceRGBA(0.21, 0.52, 0.89, 1.0); // #3584e4 accent blue
            cr.arc(cx, cy, radius, pulseStart, pulseEnd);
            cr.stroke();
        } else if (this._state === 'updating') {
            // Determinate arc filling clockwise from top
            const endAngle = startAngle + (this._progress * 2 * Math.PI);
            cr.setSourceRGBA(0.21, 0.52, 0.89, 1.0);
            cr.arc(cx, cy, radius, startAngle, endAngle);
            cr.stroke();
        } else if (this._state === 'complete') {
            // Full ring, green
            cr.setSourceRGBA(0.34, 0.89, 0.54, 1.0); // #57e389
            cr.arc(cx, cy, radius, 0, 2 * Math.PI);
            cr.stroke();
            // Checkmark inside
            cr.setLineWidth(1.8);
            cr.setLineCap(1); // ROUND
            cr.moveTo(cx - 3, cy + 0.5);
            cr.lineTo(cx - 0.5, cy + 3);
            cr.lineTo(cx + 3.5, cy - 2);
            cr.stroke();
        } else if (this._state === 'error') {
            // Full ring, red
            cr.setSourceRGBA(0.93, 0.2, 0.23, 1.0); // #ed333b
            cr.arc(cx, cy, radius, 0, 2 * Math.PI);
            cr.stroke();
            // X inside
            cr.setLineWidth(1.8);
            cr.setLineCap(1); // ROUND
            cr.moveTo(cx - 2.5, cy - 2.5);
            cr.lineTo(cx + 2.5, cy + 2.5);
            cr.moveTo(cx + 2.5, cy - 2.5);
            cr.lineTo(cx - 2.5, cy + 2.5);
            cr.stroke();
        }

        cr.$dispose();
    }

    destroy() {
        this._stopPulse();
        super.destroy();
    }
});

// ─── System Indicator (Caffeine pattern) ─────────────────────────────────────
// Extends QuickSettings.SystemIndicator so it lives in the same panel area
// as Caffeine, Night Light, etc. Uses addExternalIndicator to register.

const FinupdateIndicator = GObject.registerClass(
class FinupdateIndicator extends QuickSettings.SystemIndicator {
    _init() {
        super._init();

        // Create the progress circle and add it as a child
        // (like Caffeine adds its icon + timer label)
        this._circle = new ProgressCircle();
        this._circle.visible = false;
        this.add_child(this._circle);

        this._state = 'idle';
        this._hideTimeout = null;
        this._proxy = null;
        this._propsChangedId = null;

        this._setupDBusWatch();
    }

    _setupDBusWatch() {
        this._watchId = Gio.bus_watch_name(
            Gio.BusType.SESSION,
            DBUS_NAME,
            Gio.BusNameWatcherFlags.NONE,
            this._onNameAppeared.bind(this),
            this._onNameVanished.bind(this),
        );
    }

    _onNameAppeared(_connection, _name, _owner) {
        this._proxy = new Gio.DBusProxy({
            g_connection: Gio.DBus.session,
            g_name: DBUS_NAME,
            g_object_path: DBUS_PATH,
            g_interface_name: DBUS_IFACE,
            g_flags: Gio.DBusProxyFlags.NONE,
        });

        this._proxy.init_async(GLib.PRIORITY_DEFAULT, null, (proxy, result) => {
            try {
                proxy.init_finish(result);
                this._onPropertiesChanged();
                this._propsChangedId = this._proxy.connect(
                    'g-properties-changed',
                    this._onPropertiesChanged.bind(this),
                );
            } catch (e) {
                logError(e, 'finupdate-progress: proxy init failed');
            }
        });
    }

    _onNameVanished(_connection, _name) {
        if (this._proxy && this._propsChangedId) {
            this._proxy.disconnect(this._propsChangedId);
            this._propsChangedId = null;
        }
        this._proxy = null;
        this._updateState('idle', 0);
    }

    _onPropertiesChanged() {
        if (!this._proxy) return;

        const stateV = this._proxy.get_cached_property('State');
        const progressV = this._proxy.get_cached_property('Progress');

        const state = stateV ? stateV.unpack() : 'idle';
        const progress = progressV ? progressV.unpack() : 0.0;

        this._updateState(state, progress);
    }

    _updateState(state, progress) {
        this._state = state;
        this._circle.state = state;
        this._circle.progress = progress;

        // Show/hide the circle based on state
        const shouldShow = state !== 'idle';
        this._circle.visible = shouldShow;

        // Auto-hide after 8s when complete/error
        if (this._hideTimeout) {
            GLib.source_remove(this._hideTimeout);
            this._hideTimeout = null;
        }
        if (state === 'complete' || state === 'error') {
            this._hideTimeout = GLib.timeout_add_seconds(
                GLib.PRIORITY_DEFAULT, 8, () => {
                    this._circle.visible = false;
                    this._hideTimeout = null;
                    return GLib.SOURCE_REMOVE;
                });
        }
    }

    destroy() {
        if (this._watchId) {
            Gio.bus_unwatch_name(this._watchId);
            this._watchId = null;
        }
        if (this._proxy && this._propsChangedId) {
            this._proxy.disconnect(this._propsChangedId);
        }
        if (this._hideTimeout) {
            GLib.source_remove(this._hideTimeout);
        }
        this._circle?.destroy();
        super.destroy();
    }
});

// ─── Extension entry point ───────────────────────────────────────────────────

export default class FinupdateProgressExtension extends Extension {
    enable() {
        this._indicator = new FinupdateIndicator();
        // Register with Quick Settings (same as Caffeine)
        QuickSettingsMenu.addExternalIndicator(this._indicator);
    }

    disable() {
        this._indicator?.destroy();
        this._indicator = null;
    }
}
