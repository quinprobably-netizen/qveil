QT += core gui quick quickcontrols2 network testlib opengl svg
CONFIG += c++17 testcase link_pkgconfig
PKGCONFIG += mpv
TARGET = qveil_tests
TEMPLATE = app

INCLUDEPATH += ../src
# Where the tests that load a QML file of the app find it.
DEFINES += SRCDIR=\\\"$$PWD/../src\\\"

HEADERS += suite.h ../src/backend.h ../src/datadirs.h ../src/launch.h ../src/mpvitem.h ../src/theme.h ../src/translator.h ../src/videomodel.h
SOURCES += main.cpp qveil_tests.cpp videomodel_tests.cpp engine_tests.cpp \
    ../src/backend.cpp ../src/datadirs.cpp ../src/mpvitem.cpp ../src/theme.cpp ../src/translator.cpp ../src/videomodel.cpp

# The app's resources (icons, the veil's shader), so its QML loads as it does there.
include(../src/resources.pri)
