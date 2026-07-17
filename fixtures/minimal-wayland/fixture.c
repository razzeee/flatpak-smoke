#include <gtk/gtk.h>

static void activate(GtkApplication *app, gpointer user_data) {
    (void)user_data;

    GtkWidget *window = gtk_application_window_new(app);
    gtk_window_set_title(GTK_WINDOW(window), "flatpak-smoke fixture");
    gtk_window_set_default_size(GTK_WINDOW(window), 480, 260);

    GtkWidget *label = gtk_label_new("flatpak-smoke fixture");
    gtk_window_set_child(GTK_WINDOW(window), label);
    gtk_window_present(GTK_WINDOW(window));
}

int main(int argc, char **argv) {
    g_setenv("GDK_BACKEND", "wayland", TRUE);

    GtkApplication *app = gtk_application_new(
        "org.example.FlatpakSmokeFixture",
        G_APPLICATION_DEFAULT_FLAGS
    );
    g_signal_connect(app, "activate", G_CALLBACK(activate), NULL);

    int status = g_application_run(G_APPLICATION(app), argc, argv);
    g_object_unref(app);
    return status;
}
