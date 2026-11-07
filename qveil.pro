QT += core gui qml quick quickcontrols2 network opengl svg

CONFIG += c++17 release link_pkgconfig
PKGCONFIG += mpv
TARGET = qveil
TEMPLATE = app

# The engine's version is the app's, so it is read from one place: the first
# `version = "x.y.z"` line of core/Cargo.toml is the package's own.
CARGO_TOML = $$cat($$PWD/core/Cargo.toml, lines)
CARGO_VERSION_LINES = $$find(CARGO_TOML, ^version)
CARGO_VERSION_LINE = $$first(CARGO_VERSION_LINES)
QVEIL_VERSION = $$section(CARGO_VERSION_LINE, \", 1, 1)
isEmpty(QVEIL_VERSION): error("Couldn't read the version from core/Cargo.toml")
DEFINES += QVEIL_VERSION=\\\"$$QVEIL_VERSION\\\"

HEADERS += \
    src/backend.h \
    src/datadirs.h \
    src/mpvitem.h \
    src/theme.h \
    src/translator.h \
    src/videomodel.h

SOURCES += \
    src/main.cpp \
    src/backend.cpp \
    src/datadirs.cpp \
    src/mpvitem.cpp \
    src/theme.cpp \
    src/translator.cpp \
    src/videomodel.cpp

include(src/resources.pri)
