#include "datadirs.h"

#include <QDir>
#include <QFile>
#include <QFileInfo>

#include <cstdio>

namespace DataDirs {

namespace {

const QFile::Permissions Private = QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner;

struct Kind {
    const char *variable;
    const char *fallback;
    bool cache;
};

constexpr Kind kinds[] = {
    {"XDG_CONFIG_HOME", ".config", false},
    {"XDG_DATA_HOME", ".local/share", false},
    {"XDG_CACHE_HOME", ".cache", true},
};

QString configDirectory() { return base("XDG_CONFIG_HOME", ".config") + "/qveil"; }

void makePrivate(const QString &directory) {
    QDir().mkpath(directory);
    QFile::setPermissions(directory, Private);
}

} // namespace

QString base(const char *variable, const QString &fallback) {
    const QString value = qEnvironmentVariable(variable);
    return QDir::isAbsolutePath(value) ? QDir::cleanPath(value) : QDir::homePath() + "/" + fallback;
}

void migrate(const QString &base, bool cache) {
    const QFileInfo old(base + "/veil");
    if (!old.isDir())
        return;
    if (!QFileInfo::exists(base + "/qveil")) {
        if (!QDir(base).rename("veil", "qveil"))
            std::fprintf(stderr, "qveil: couldn't move %s to %s/qveil\n", qPrintable(old.filePath()), qPrintable(base));
    } else if (cache && !old.isSymLink()) {
        QDir(old.filePath()).removeRecursively();
    }
}

void prepare() {
    for (const Kind &kind : kinds) {
        const QString root = base(kind.variable, kind.fallback);
        migrate(root, kind.cache);
        makePrivate(root + "/qveil");
    }

    // The window's settings file was named after the app too.
    const QString directory = configDirectory();
    if (QFileInfo(directory + "/veil.conf").isFile() && !QFileInfo::exists(directory + "/qveil.conf"))
        QFile::rename(directory + "/veil.conf", directory + "/qveil.conf");

    // Qt rewrites the file in place and keeps the mode it finds, so it only
    // has to start out private.
    QFile settings(directory + "/qveil.conf");
    if (settings.open(QIODevice::WriteOnly | QIODevice::Append))
        settings.close();
    settings.setPermissions(QFile::ReadOwner | QFile::WriteOwner);
}

void tightenSettings() {
    QFile::setPermissions(configDirectory() + "/qveil.conf", QFile::ReadOwner | QFile::WriteOwner);
}

} // namespace DataDirs
