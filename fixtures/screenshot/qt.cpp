#include <QApplication>
#include <QDialog>
#include <QDir>
#include <QFile>
#include <QLabel>
#include <QLineEdit>
#include <QProgressBar>
#include <QPushButton>
#include <QShortcut>
#include <QStandardPaths>
#include <QTextStream>
#include <QTimer>
#include <QVBoxLayout>

int main(int argc, char **argv) {
    QApplication app(argc, argv);
    app.setApplicationName("ScreenshotQt");
    QWidget window;
    window.setWindowTitle("Qt Screenshot Fixture");
    window.resize(600, 400);
    QVBoxLayout layout(&window);
    const auto config = QStandardPaths::writableLocation(QStandardPaths::AppConfigLocation);
    QDir().mkpath(config);
    QFile marker(config + "/screenshot-fixture-marker");
    QLabel label(marker.exists() ? "Reused profile" : "Ready to capture");
    marker.open(QIODevice::WriteOnly);
    marker.write("created by fixture");
    marker.close();
    QTextStream(stdout) << "fixture config: " << config << Qt::endl;
    QLineEdit entry;
    entry.setPlaceholderText("Search");
    QObject::connect(&entry, &QLineEdit::returnPressed, [&] {
        QTextStream(stdout) << "submitted text: " << entry.text() << Qt::endl;
        if (entry.text() == "delay") {
            label.setText("Loading");
            QTimer::singleShot(700, [&] { label.setText("Results for delay"); });
        } else {
            label.setText("Results for " + entry.text());
        }
    });
    QPushButton button("Preferences");
    const auto preferences = [&] {
        auto *dialog = new QDialog(&window);
        dialog->setAttribute(Qt::WA_DeleteOnClose);
        dialog->setWindowTitle("Preferences");
        dialog->resize(400, 300);
        auto *box = new QVBoxLayout(dialog);
        box->addWidget(new QLabel("Choose how the app behaves"));
        dialog->show();
    };
    QObject::connect(&button, &QPushButton::clicked, preferences);
    QShortcut quit(QKeySequence("Ctrl+Q"), &window);
    QObject::connect(&quit, &QShortcut::activated, &app, &QApplication::quit);
    QShortcut settings(QKeySequence("Ctrl+,"), &window);
    QObject::connect(&settings, &QShortcut::activated, preferences);
    QProgressBar progress;
    progress.setRange(0, 100);
    QTimer timer;
    QObject::connect(&timer, &QTimer::timeout, [&] { progress.setValue((progress.value() + 10) % 100); });
    timer.start(80);
    layout.addWidget(&label);
    layout.addWidget(&entry);
    layout.addWidget(&button);
    layout.addWidget(&progress);
    window.show();
    return app.exec();
}
