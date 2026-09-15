import sys
from PyQt6.QtWidgets import QApplication, QWidget, QVBoxLayout, QLabel, QLineEdit, QPushButton, QDialog

app = QApplication(sys.argv)
window = QWidget()
window.setWindowTitle("Qt Screenshot Fixture")
window.resize(600, 400)
layout = QVBoxLayout(window)
label = QLabel("Ready to capture")
entry = QLineEdit()
entry.setPlaceholderText("Search")
entry.returnPressed.connect(lambda: label.setText("Results for " + entry.text()))
button = QPushButton("Preferences")
dialog = QDialog(window)
dialog.setWindowTitle("Preferences")
dialog.resize(400, 300)
QVBoxLayout(dialog).addWidget(QLabel("Choose how the app behaves"))
button.clicked.connect(dialog.show)
for widget in [label, entry, button]:
    layout.addWidget(widget)
window.show()
sys.exit(app.exec())
