#include <X11/Xlib.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fprintf(stderr, "failed to open X display\n");
        return 2;
    }

    int screen = DefaultScreen(display);
    Window window = XCreateSimpleWindow(
        display,
        RootWindow(display, screen),
        80,
        80,
        480,
        260,
        1,
        BlackPixel(display, screen),
        WhitePixel(display, screen)
    );

    XStoreName(display, window, "flatpak-smoke fixture");
    XSelectInput(display, window, ExposureMask | StructureNotifyMask);
    XMapWindow(display, window);
    XFlush(display);

    for (;;) {
        while (XPending(display) > 0) {
            XEvent event;
            XNextEvent(display, &event);
        }
        usleep(100000);
    }
}
