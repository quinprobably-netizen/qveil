QT += core gui qml quick quickcontrols2 network opengl svg

CONFIG += c++17 release link_pkgconfig
PKGCONFIG += mpv
TARGET = qveil
TEMPLATE = app

HEADERS += \
    src/backend.h \
    src/mpvitem.h \
    src/theme.h \
    src/translator.h

SOURCES += \
    src/main.cpp \
    src/backend.cpp \
    src/mpvitem.cpp \
    src/theme.cpp \
    src/translator.cpp

RESOURCES += src/resources.qrc
