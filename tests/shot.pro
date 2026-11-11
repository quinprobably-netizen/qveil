QT += core gui qml quick quickcontrols2 network opengl svg
CONFIG += c++17 release link_pkgconfig
PKGCONFIG += mpv
TARGET = qveil_shot
TEMPLATE = app

# Where the stand-in engine is.
DEFINES += SHOTS_DIR=\\\"$$PWD/shots\\\"

SOURCES += shot.cpp

include(../src/sources.pri)
include(../src/resources.pri)
