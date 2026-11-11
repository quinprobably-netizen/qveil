QT += core gui qml quick quickcontrols2 network testlib opengl svg
CONFIG += c++17 testcase link_pkgconfig
PKGCONFIG += mpv
TARGET = qveil_tests
TEMPLATE = app

# Where the tests that load a QML file of the app find it.
DEFINES += SRCDIR=\\\"$$PWD/../src\\\"

HEADERS += suite.h
SOURCES += main.cpp qveil_tests.cpp videomodel_tests.cpp engine_tests.cpp parts_tests.cpp window_tests.cpp linkify_tests.cpp player_tests.cpp

# The window's code, as the app has it.
include(../src/sources.pri)
# The app's resources (icons, the veil's shader), so its QML loads as it does there.
include(../src/resources.pri)
