/* panel-demo.c
 *
 * Copyright 2026 Project Bluefin Contributors
 * SPDX-License-Identifier: GPL-2.0-or-later
 *
 * Tiny harness that loads the FFI widgets from libfinupdate.so into a
 * plain libadwaita window. Lets us iterate on the changelog / rebase
 * widget code without round-tripping through a patched
 * gnome-control-center build.
 *
 * NOT shipped to users — `just panel-demo` from the repo root builds
 * and runs this against the local cdylib. Production deployment lives
 * in cc-panel/ and the Dakota BuildStream patch.
 *
 * Layout: AdwHeaderBar + AdwViewSwitcher with two pages, each hosting
 * one of the FFI widgets. Mirrors what the real cc panel does (nav-view
 * subpages), just flatter for quicker visual inspection.
 */

#include <adwaita.h>
#include "finupdate.h"

typedef struct
{
  AdwApplicationWindow *window;
  Handle               *backend;
} App;

/* Build the demo window. Creates the backend handle, both widgets, and
 * sticks them in a view stack so the dev can tab between them. */
static void
on_app_activate (AdwApplication *app, gpointer user_data)
{
  App *demo = user_data;
  GtkWidget *changelog;
  GtkWidget *rebase;
  GtkWidget *toolbar_view;
  GtkWidget *header;
  GtkWidget *view_stack;
  GtkWidget *switcher_bar;

  demo->window = ADW_APPLICATION_WINDOW (adw_application_window_new (GTK_APPLICATION (app)));
  gtk_window_set_title (GTK_WINDOW (demo->window), "finupdate panel demo");
  gtk_window_set_default_size (GTK_WINDOW (demo->window), 720, 720);

  demo->backend = finupdate_new ();
  if (demo->backend == NULL)
    {
      g_warning ("finupdate_new returned NULL — check journalctl for backend init errors");
      return;
    }

  toolbar_view = adw_toolbar_view_new ();
  adw_application_window_set_content (demo->window, toolbar_view);

  header = adw_header_bar_new ();
  adw_toolbar_view_add_top_bar (ADW_TOOLBAR_VIEW (toolbar_view), header);

  view_stack = adw_view_stack_new ();
  adw_toolbar_view_set_content (ADW_TOOLBAR_VIEW (toolbar_view), view_stack);

  /* Page 1 — changelog widget */
  changelog = (GtkWidget *) finupdate_changelog_widget_new (demo->backend);
  if (changelog != NULL)
    {
      AdwViewStackPage *page;
      page = adw_view_stack_add_titled_with_icon (ADW_VIEW_STACK (view_stack),
                                                  changelog,
                                                  "whats-new",
                                                  "What's New",
                                                  "view-list-symbolic");
      (void) page;
    }

  /* Page 2 — rebase widget */
  rebase = (GtkWidget *) finupdate_rebase_widget_new (demo->backend);
  if (rebase != NULL)
    {
      AdwViewStackPage *page;
      page = adw_view_stack_add_titled_with_icon (ADW_VIEW_STACK (view_stack),
                                                  rebase,
                                                  "rebase",
                                                  "Change Image",
                                                  "object-flip-vertical-symbolic");
      (void) page;
    }

  switcher_bar = adw_view_switcher_bar_new ();
  adw_view_switcher_bar_set_stack (ADW_VIEW_SWITCHER_BAR (switcher_bar),
                                   ADW_VIEW_STACK (view_stack));
  adw_view_switcher_bar_set_reveal (ADW_VIEW_SWITCHER_BAR (switcher_bar), TRUE);
  adw_toolbar_view_add_bottom_bar (ADW_TOOLBAR_VIEW (toolbar_view), switcher_bar);

  gtk_window_present (GTK_WINDOW (demo->window));
}

static void
on_app_shutdown (GApplication *app, gpointer user_data)
{
  App *demo = user_data;

  if (demo->backend != NULL)
    {
      finupdate_free (demo->backend);
      demo->backend = NULL;
    }
}

int
main (int argc, char *argv[])
{
  App demo = { 0 };
  AdwApplication *app;
  int ret;

  app = adw_application_new ("org.projectbluefin.Finupdate.PanelDemo",
                             G_APPLICATION_NON_UNIQUE);
  g_signal_connect (app, "activate",   G_CALLBACK (on_app_activate),  &demo);
  g_signal_connect (app, "shutdown",   G_CALLBACK (on_app_shutdown),  &demo);

  ret = g_application_run (G_APPLICATION (app), argc, argv);
  g_object_unref (app);
  return ret;
}
