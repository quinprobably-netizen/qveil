#pragma once

#include <QList>
#include <QObject>

// The test classes of this binary. Each file registers its class with
// QVEIL_SUITE, and main.cpp runs them one after the other, so tests can live
// beside the part of the window they are about.
class Suite {
public:
    static QList<QObject *> &all() {
        static QList<QObject *> tests;
        return tests;
    }
    static bool add(QObject *test) {
        all().append(test);
        return true;
    }
};

#define QVEIL_SUITE(Class) \
    static Class suite##Class; \
    static const bool registered##Class = Suite::add(&suite##Class);
