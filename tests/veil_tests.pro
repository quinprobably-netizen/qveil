QT += core gui quick quickcontrols2 network testlib
CONFIG += c++17 testcase
TARGET = veil_tests
TEMPLATE = app

INCLUDEPATH += ../src

HEADERS += ../src/backend.h ../src/theme.h ../src/translator.h
SOURCES += veil_tests.cpp ../src/backend.cpp ../src/theme.cpp ../src/translator.cpp
