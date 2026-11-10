QT += core gui qml quick quickcontrols2 network opengl svg
CONFIG += c++17 release link_pkgconfig
PKGCONFIG += mpv
TARGET = qveil_shot
TEMPLATE = app

INCLUDEPATH += ../src
# Where the stand-in engine is.
DEFINES += SHOTS_DIR=\\\"$$PWD/shots\\\"

HEADERS += ../src/backend.h ../src/datadirs.h ../src/mpvitem.h ../src/theme.h ../src/translator.h ../src/videomodel.h
SOURCES += shot.cpp ../src/backend.cpp ../src/datadirs.cpp ../src/mpvitem.cpp ../src/theme.cpp ../src/translator.cpp ../src/videomodel.cpp

include(../src/resources.pri)
