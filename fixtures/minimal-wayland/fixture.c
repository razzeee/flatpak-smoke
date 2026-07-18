#include <gtk/gtk.h>

static void clicked(GtkButton *button, gpointer user_data) {
    GtkLabel *label = GTK_LABEL(user_data);

    gtk_label_set_text(label, "flatpak-smoke fixture clicked");
    gtk_button_set_label(button, "Clicked");
}

static void activate(GtkApplication *app, gpointer user_data) {
    (void)user_data;

    GtkWidget *window = gtk_application_window_new(app);
    gtk_window_set_title(GTK_WINDOW(window), "flatpak-smoke fixture");
    gtk_window_set_default_size(GTK_WINDOW(window), 480, 260);

    GtkWidget *label = gtk_label_new("flatpak-smoke fixture");
    GtkWidget *button = gtk_button_new_with_label("Click Me");
    GtkWidget *box = gtk_box_new(GTK_ORIENTATION_VERTICAL, 24);

    gtk_widget_add_css_class(button, "suggested-action");
    gtk_widget_set_halign(box, GTK_ALIGN_CENTER);
    gtk_widget_set_valign(box, GTK_ALIGN_CENTER);
    gtk_widget_set_size_request(button, 160, 44);
    gtk_box_append(GTK_BOX(box), label);
    gtk_box_append(GTK_BOX(box), button);
    gtk_window_set_child(GTK_WINDOW(window), box);

    g_signal_connect(button, "clicked", G_CALLBACK(clicked), label);
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
