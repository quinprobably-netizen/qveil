QT += core gui quick quickcontrols2 network testlib opengl
CONFIG += c++17 testcase link_pkgconfig
PKGCONFIG += mpv
TARGET = qveil_tests
TEMPLATE = app

INCLUDEPATH += ../src

HEADERS += ../src/backend.h ../src/mpvitem.h ../src/theme.h ../src/translator.h
SOURCES += qveil_tests.cpp ../src/backend.cpp ../src/mpvitem.cpp ../src/theme.cpp ../src/translator.cpp
