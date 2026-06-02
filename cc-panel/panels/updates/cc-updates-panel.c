/* cc-updates-panel.c
 *
 * Copyright 2026 Project Bluefin Contributors
 * SPDX-License-Identifier: GPL-2.0-or-later
 *
 * gnome-control-center "Software Updates" panel — embeds the finupdate
 * backend (libfinupdate.so) for bootc image management. See
 * `docs/control-center-integration.md` in the finupdate repo for the
 * overall integration design. This file is intended to live at
 * `panels/updates/cc-updates-panel.c` inside a vendored gnome-control-center
 * source tree.
 *
 * Layout: a single AdwPreferencesPage with three groups:
 *   1. Hero row     — current image identity (title + ref + digest + date)
 *   2. Update state — "Up to date" / "Update available" + Check button
 *   3. Advanced     — placeholder for the Image History / Rebase subpages
 *
 * Threading: every libfinupdate call returns via a callback on a worker
 * thread. We marshal the result back to the main loop with g_idle_add_full
 * before touching widgets, so the panel never blocks the cc UI thread on
 * registry I/O.
 */

#include <adwaita.h>
#include <glib/gi18n.h>

#include "cc-updates-panel.h"
#include "shell/cc-panel.h"

#include "finupdate.h"

struct _CcUpdatesPanel
{
  CcPanel               parent_instance;

  /* Widgets owned by the .ui template */
  AdwNavigationView    *nav_view;
  AdwActionRow         *hero_row;
  AdwActionRow         *update_state_row;
  AdwActionRow         *whats_new_row;
  AdwActionRow         *change_image_row;
  AdwPreferencesGroup  *progress_group;
  AdwActionRow         *system_row;
  AdwActionRow         *flatpak_row;
  AdwActionRow         *brew_row;
  AdwActionRow         *distrobox_row;
  GtkButton            *check_button;
  GtkButton            *install_button;

  /* Rust backend handle. Created in init, freed in dispose. */
  Handle               *backend;
};

G_DEFINE_FINAL_TYPE (CcUpdatesPanel, cc_updates_panel, CC_TYPE_PANEL)

/* ───── helpers ───── */

/* Read the current image identity from the backend and write it into the
 * hero row. Synchronous (block_on inside libfinupdate) — fast enough for
 * initial paint because the service caches the bootc-status / os-release
 * lookup. */
static void
refresh_hero_row (CcUpdatesPanel *self)
{
  char *title;
  char *image_ref;

  if (self->backend == NULL)
    return;

  title = finupdate_current_image_title (self->backend);
  image_ref = finupdate_current_image_ref (self->backend);

  adw_action_row_set_title (self->hero_row, title ? title : "System Image");
  adw_action_row_set_subtitle (self->hero_row, image_ref ? image_ref : "");

  finupdate_string_free (title);
  finupdate_string_free (image_ref);
}

/* ───── update-check flow ───── */

/* Carries the result back from the worker thread to the main loop via
 * g_idle_add_full. The Rust callback can't touch GTK widgets directly. */
typedef struct
{
  CcUpdatesPanel *panel;
  int             result;
} CheckResult;

static gboolean
apply_check_result (gpointer data)
{
  CheckResult *r = data;

  if (G_IS_OBJECT (r->panel))
    {
      const char *label;
      gboolean show_install = FALSE;
      switch (r->result)
        {
        case 1:  label = _("Update available");        show_install = TRUE; break;
        case 0:  label = _("Up to date");              break;
        default: label = _("Couldn't reach registry"); break;
        }
      adw_action_row_set_title (r->panel->update_state_row, label);
      gtk_widget_set_visible (GTK_WIDGET (r->panel->install_button), show_install);
      gtk_widget_set_sensitive (GTK_WIDGET (r->panel->check_button), TRUE);
    }

  g_object_unref (r->panel);
  g_free (r);
  return G_SOURCE_REMOVE;
}

/* Called from a tokio worker thread when the registry probe completes.
 * MUST NOT touch widgets — instead it queues apply_check_result on the
 * main loop. */
static void
on_check_finished (int update_available, void *user_data)
{
  CcUpdatesPanel *panel = user_data;
  CheckResult *r = g_new0 (CheckResult, 1);

  r->panel = panel; /* main-loop idle takes ownership of the ref */
  r->result = update_available;

  g_idle_add_full (G_PRIORITY_DEFAULT, apply_check_result, r, NULL);
}

static void
on_check_button_clicked (GtkButton *button, gpointer user_data)
{
  CcUpdatesPanel *self = CC_UPDATES_PANEL (user_data);

  if (self->backend == NULL)
    return;

  adw_action_row_set_title (self->update_state_row, _("Checking…"));
  gtk_widget_set_sensitive (GTK_WIDGET (button), FALSE);

  /* Take a ref so the panel can't be disposed while the worker thread
   * is mid-call. apply_check_result drops the ref. */
  g_object_ref (self);
  finupdate_check_for_updates (self->backend, on_check_finished, self);
}

/* ───── "What's new" subpage ───── */

/* Activated when the user clicks the "What's new in this update" row.
 * Builds a fresh changelog widget from libfinupdate, wraps it in an
 * AdwNavigationPage, and pushes it onto the nav stack. Rebuilding on
 * every push (rather than caching) keeps the data fresh and avoids
 * having to subscribe to registry-change notifications across the FFI. */
static void
on_whats_new_activated (AdwActionRow *row, gpointer user_data)
{
  CcUpdatesPanel *self = CC_UPDATES_PANEL (user_data);
  GtkWidget *content;
  AdwNavigationPage *page;

  if (self->backend == NULL)
    return;

  /* The cdylib returns the widget as a void* — cast back to GtkWidget.
   * Ownership transferred: the floating ref is sunk when we add the
   * widget to the AdwNavigationPage below. */
  content = (GtkWidget *) finupdate_changelog_widget_new (self->backend);
  if (content == NULL)
    return;

  page = ADW_NAVIGATION_PAGE (
      g_object_new (ADW_TYPE_NAVIGATION_PAGE,
                    "title", _("What's New"),
                    "tag",   "whats-new",
                    "child", content,
                    NULL));
  adw_navigation_view_push (self->nav_view, page);
}

/* ───── Apply-update flow ───── */

/* Update events are emitted from a tokio worker thread. We can't touch
 * widgets from there — every event gets copied into a heap struct, sent
 * to the main loop via g_idle_add_full, and dispatched to the right row
 * inside ApplyResult. */
typedef struct
{
  CcUpdatesPanel        *panel;
  FinupdateEventKind    kind;
  FinupdateModule       module;
  FinupdateModuleStatus status;
  int                   exit_code;
  char                 *message; /* g_strdup'd in the worker; freed in apply */
} ApplyEvent;

static AdwActionRow *
row_for_module (CcUpdatesPanel *self, FinupdateModule module)
{
  switch (module)
    {
    case FinupdateModule_System:    return self->system_row;
    case FinupdateModule_Flatpak:   return self->flatpak_row;
    case FinupdateModule_Brew:      return self->brew_row;
    case FinupdateModule_Distrobox: return self->distrobox_row;
    default:                        return NULL;
    }
}

static const char *
status_string (FinupdateModuleStatus status)
{
  switch (status)
    {
    case FinupdateModuleStatus_Success:  return _("Done");
    case FinupdateModuleStatus_UpToDate: return _("Up to date");
    case FinupdateModuleStatus_Skipped:  return _("Skipped");
    case FinupdateModuleStatus_Failed:   return _("Failed");
    default:                             return NULL;
    }
}

static gboolean
apply_update_event (gpointer data)
{
  ApplyEvent *evt = data;
  CcUpdatesPanel *self = evt->panel;
  AdwActionRow *row;

  if (!G_IS_OBJECT (self))
    goto out;

  switch (evt->kind)
    {
    case FinupdateEventKind_ModuleStarted:
      row = row_for_module (self, evt->module);
      if (row)
        adw_action_row_set_subtitle (row, _("Running…"));
      break;

    case FinupdateEventKind_ModuleFinished:
      row = row_for_module (self, evt->module);
      if (row)
        {
          const char *label = status_string (evt->status);
          if (label)
            adw_action_row_set_subtitle (row, label);
        }
      break;

    case FinupdateEventKind_Output:
      /* Could land in a log expander; for now the per-row subtitle
       * updates already give the user enough signal. */
      break;

    case FinupdateEventKind_Complete:
      adw_action_row_set_title (self->update_state_row,
                                _("Update installed — restart to apply"));
      gtk_widget_set_visible (GTK_WIDGET (self->install_button), FALSE);
      gtk_widget_set_visible (GTK_WIDGET (self->check_button), TRUE);
      gtk_widget_set_sensitive (GTK_WIDGET (self->check_button), TRUE);
      break;

    case FinupdateEventKind_UpToDate:
      adw_action_row_set_title (self->update_state_row, _("Already up to date"));
      gtk_widget_set_visible (GTK_WIDGET (self->install_button), FALSE);
      gtk_widget_set_visible (GTK_WIDGET (self->check_button), TRUE);
      gtk_widget_set_sensitive (GTK_WIDGET (self->check_button), TRUE);
      break;

    case FinupdateEventKind_Error:
      adw_action_row_set_title (self->update_state_row,
                                evt->message ? evt->message : _("Update failed"));
      gtk_widget_set_visible (GTK_WIDGET (self->install_button), FALSE);
      gtk_widget_set_visible (GTK_WIDGET (self->check_button), TRUE);
      gtk_widget_set_sensitive (GTK_WIDGET (self->check_button), TRUE);
      break;
    }

out:
  g_object_unref (evt->panel);
  g_free (evt->message);
  g_free (evt);
  return G_SOURCE_REMOVE;
}

/* Worker-thread callback from libfinupdate. Must not touch widgets —
 * copies the event into an ApplyEvent and dispatches via g_idle_add.
 *
 * Refs the panel on every event; apply_update_event drops one ref per
 * fire, so refs balance regardless of how many events come in. Pending
 * events after a panel dispose stay valid because the ref count keeps
 * the GObject alive until the last event drains. */
static void
on_apply_event (FinupdateEventKind    kind,
                FinupdateModule       module,
                FinupdateModuleStatus status,
                int                   exit_code,
                const char           *message,
                void                 *user_data)
{
  CcUpdatesPanel *panel = user_data;
  ApplyEvent *evt = g_new0 (ApplyEvent, 1);

  evt->panel    = g_object_ref (panel);
  evt->kind     = kind;
  evt->module   = module;
  evt->status   = status;
  evt->exit_code = exit_code;
  /* Copy the message before returning — libfinupdate frees the
   * underlying CString as soon as this callback returns. */
  evt->message  = message ? g_strdup (message) : NULL;

  g_idle_add_full (G_PRIORITY_DEFAULT, apply_update_event, evt, NULL);
}

static void
reset_progress_rows (CcUpdatesPanel *self)
{
  const char *waiting = _("Waiting…");
  adw_action_row_set_subtitle (self->system_row,    waiting);
  adw_action_row_set_subtitle (self->flatpak_row,   waiting);
  adw_action_row_set_subtitle (self->brew_row,      waiting);
  adw_action_row_set_subtitle (self->distrobox_row, waiting);
}

static void
on_install_button_clicked (GtkButton *button, gpointer user_data)
{
  CcUpdatesPanel *self = CC_UPDATES_PANEL (user_data);

  if (self->backend == NULL)
    return;

  gtk_widget_set_sensitive (GTK_WIDGET (button), FALSE);
  gtk_widget_set_visible (GTK_WIDGET (self->progress_group), TRUE);
  adw_action_row_set_title (self->update_state_row, _("Installing…"));
  reset_progress_rows (self);

  /* Take a ref so a flurry of events during a long install can't race a
   * panel disposal. Every event call drops one ref via apply_update_event,
   * so we ref once per event we expect — for the multi-event stream we
   * just ref enough for the terminal Complete/UpToDate/Error tick to be
   * safe. Simplest: ref on each callback inside on_apply_event. */
  finupdate_apply_update_start (self->backend, on_apply_event, self);
}

/* ───── "Change image" subpage ───── */

static void
on_change_image_activated (AdwActionRow *row, gpointer user_data)
{
  CcUpdatesPanel *self = CC_UPDATES_PANEL (user_data);
  GtkWidget *content;
  AdwNavigationPage *page;

  if (self->backend == NULL)
    return;

  content = (GtkWidget *) finupdate_rebase_widget_new (self->backend);
  if (content == NULL)
    return;

  page = ADW_NAVIGATION_PAGE (
      g_object_new (ADW_TYPE_NAVIGATION_PAGE,
                    "title", _("Change Image"),
                    "tag",   "change-image",
                    "child", content,
                    NULL));
  adw_navigation_view_push (self->nav_view, page);
}

/* ───── GObject lifecycle ───── */

static void
cc_updates_panel_dispose (GObject *object)
{
  CcUpdatesPanel *self = CC_UPDATES_PANEL (object);

  /* Free the Rust handle. In-flight callbacks hold their own panel
   * reference (g_object_ref above) so they won't crash even if
   * libfinupdate's check returns after dispose — they'll just see an
   * already-disposed panel and skip the widget update. */
  if (self->backend != NULL)
    {
      finupdate_free (self->backend);
      self->backend = NULL;
    }

  G_OBJECT_CLASS (cc_updates_panel_parent_class)->dispose (object);
}

static void
cc_updates_panel_class_init (CcUpdatesPanelClass *klass)
{
  GObjectClass *object_class = G_OBJECT_CLASS (klass);
  GtkWidgetClass *widget_class = GTK_WIDGET_CLASS (klass);

  object_class->dispose = cc_updates_panel_dispose;

  gtk_widget_class_set_template_from_resource (
      widget_class,
      "/org/gnome/control-center/updates/cc-updates-panel.ui");

  gtk_widget_class_bind_template_child (widget_class, CcUpdatesPanel, nav_view);
  gtk_widget_class_bind_template_child (widget_class, CcUpdatesPanel, hero_row);
  gtk_widget_class_bind_template_child (widget_class, CcUpdatesPanel, update_state_row);
  gtk_widget_class_bind_template_child (widget_class, CcUpdatesPanel, whats_new_row);
  gtk_widget_class_bind_template_child (widget_class, CcUpdatesPanel, change_image_row);
  gtk_widget_class_bind_template_child (widget_class, CcUpdatesPanel, progress_group);
  gtk_widget_class_bind_template_child (widget_class, CcUpdatesPanel, system_row);
  gtk_widget_class_bind_template_child (widget_class, CcUpdatesPanel, flatpak_row);
  gtk_widget_class_bind_template_child (widget_class, CcUpdatesPanel, brew_row);
  gtk_widget_class_bind_template_child (widget_class, CcUpdatesPanel, distrobox_row);
  gtk_widget_class_bind_template_child (widget_class, CcUpdatesPanel, check_button);
  gtk_widget_class_bind_template_child (widget_class, CcUpdatesPanel, install_button);

  gtk_widget_class_bind_template_callback (widget_class, on_check_button_clicked);
  gtk_widget_class_bind_template_callback (widget_class, on_install_button_clicked);
  gtk_widget_class_bind_template_callback (widget_class, on_whats_new_activated);
  gtk_widget_class_bind_template_callback (widget_class, on_change_image_activated);
}

static void
cc_updates_panel_init (CcUpdatesPanel *self)
{
  gtk_widget_init_template (GTK_WIDGET (self));

  self->backend = finupdate_new ();
  if (self->backend == NULL)
    {
      /* Backend init failure is rare (tokio runtime creation) — leave
       * the hero row at its template default and disable the Check
       * button so the user knows something's off without us putting a
       * scary error in their settings panel. */
      gtk_widget_set_sensitive (GTK_WIDGET (self->check_button), FALSE);
      adw_action_row_set_title (self->update_state_row,
                                _("Backend unavailable"));
      return;
    }

  refresh_hero_row (self);
}
