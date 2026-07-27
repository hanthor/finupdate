/* Exercise finupdate_panel_widget_new() — the entry point the shipped
 * gnome-control-center panel (cc-updates-panel.c) actually calls, and the one
 * examples/panel-demo/ never touched because it composes the individual
 * changelog/rebase widgets instead. */
#include <adwaita.h>

extern void *finupdate_new(void);
extern void  finupdate_free(void *handle);
extern void *finupdate_panel_widget_new(void *handle);

static void on_activate(GtkApplication *app, gpointer user_data) {
    void *h = finupdate_new();
    g_assert(h != NULL);
    GtkWidget *panel = (GtkWidget *) finupdate_panel_widget_new(h);
    g_assert(panel != NULL);

    GtkWidget *win = adw_application_window_new(app);
    gtk_window_set_title(GTK_WINDOW(win), "cc panel entry point");
    gtk_window_set_default_size(GTK_WINDOW(win), 900, 700);
    adw_application_window_set_content(ADW_APPLICATION_WINDOW(win), panel);
    gtk_window_present(GTK_WINDOW(win));
}

int main(int argc, char **argv) {
    AdwApplication *app = adw_application_new("org.projectbluefin.PanelEntryDemo",
                                              G_APPLICATION_DEFAULT_FLAGS);
    g_signal_connect(app, "activate", G_CALLBACK(on_activate), NULL);
    return g_application_run(G_APPLICATION(app), argc, argv);
}
