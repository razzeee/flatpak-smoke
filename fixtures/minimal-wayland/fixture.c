#include <gtk/gtk.h>
#include <unistd.h>

static const char *mode;

static gboolean present_window(gpointer user_data) {
    gtk_window_present(GTK_WINDOW(user_data));
    return G_SOURCE_REMOVE;
}

static gboolean crash(gpointer user_data) {
    (void)user_data;
    _exit(42);
}

static void first_frame_painted(GdkFrameClock *clock, gpointer user_data) {
    (void)user_data;
    g_signal_handlers_disconnect_by_func(clock, first_frame_painted, NULL);
    g_print("fixture: first frame painted\n");
    g_timeout_add(600, crash, NULL);
}

static void mapped(GtkWidget *window, gpointer user_data) {
    (void)user_data;
    if (g_strcmp0(mode, "crash-after-frame") == 0) {
        GdkFrameClock *clock = gtk_widget_get_frame_clock(window);
        g_signal_connect(clock, "after-paint", G_CALLBACK(first_frame_painted), NULL);
    }
}

static gboolean animate(gpointer user_data) {
    GtkProgressBar *progress = GTK_PROGRESS_BAR(user_data);
    double fraction = gtk_progress_bar_get_fraction(progress) + 0.1;
    gtk_progress_bar_set_fraction(progress, fraction > 1.0 ? 0.0 : fraction);
    return G_SOURCE_CONTINUE;
}

static void clicked(GtkButton *button, gpointer user_data) {
    if (g_strcmp0(mode, "exit-after-click") == 0) {
        g_print("fixture: exiting after click\n");
        _exit(43);
    }
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

    if (g_strcmp0(mode, "animated") == 0) {
        GtkWidget *progress = gtk_progress_bar_new();
        gtk_widget_set_size_request(progress, 300, -1);
        gtk_box_append(GTK_BOX(box), progress);
        g_timeout_add(80, animate, progress);
    }

    g_signal_connect(button, "clicked", G_CALLBACK(clicked), label);
    g_signal_connect(window, "map", G_CALLBACK(mapped), NULL);
    if (g_strcmp0(mode, "delayed") == 0) {
        g_timeout_add(1500, present_window, window);
    } else if (g_strcmp0(mode, "never-draw") != 0) {
        gtk_window_present(GTK_WINDOW(window));
    }
}

int main(int argc, char **argv) {
    g_setenv("GDK_BACKEND", "wayland", TRUE);
    mode = g_getenv("FLATPAK_SMOKE_FIXTURE_MODE");
    const char *modes[] = {"normal", "delayed", "never-draw", "crash-after-frame", "animated", "exit-after-click"};
    gboolean valid = mode == NULL;
    for (size_t i = 0; i < G_N_ELEMENTS(modes); i++) {
        valid |= g_strcmp0(mode, modes[i]) == 0;
    }
    if (!valid) {
        g_printerr("unknown fixture mode: %s\n", mode);
        return 2;
    }
    g_print("fixture mode: %s\n", mode ? mode : "normal");

    GtkApplication *app = gtk_application_new(
        "org.example.FlatpakSmokeFixture",
        G_APPLICATION_DEFAULT_FLAGS
    );
    g_signal_connect(app, "activate", G_CALLBACK(activate), NULL);

    int status = g_application_run(G_APPLICATION(app), argc, argv);
    g_object_unref(app);
    return status;
}
