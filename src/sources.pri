# The window's code, shared by the app, its tests and the stills tool. Each
# of them adds its own main().
INCLUDEPATH += $$PWD

HEADERS += \
    $$PWD/backend.h \
    $$PWD/datadirs.h \
    $$PWD/launch.h \
    $$PWD/mpvitem.h \
    $$PWD/theme.h \
    $$PWD/translator.h \
    $$PWD/videomodel.h

SOURCES += \
    $$PWD/backend.cpp \
    $$PWD/datadirs.cpp \
    $$PWD/mpvitem.cpp \
    $$PWD/theme.cpp \
    $$PWD/translator.cpp \
    $$PWD/videomodel.cpp
