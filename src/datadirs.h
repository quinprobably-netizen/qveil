#pragma once

#include <QString>

// qveil's three per-user directories (config, data, cache), which exist before
// Qt does anything: Qt writes its own files into the config and cache ones,
// and a directory it creates first would strand the old one.
namespace DataDirs {

// Where the XDG base directory named by `variable` points, or `fallback`
// under the home directory when it is unset or not absolute (as the XDG spec
// and the engine both treat it).
QString base(const char *variable, const QString &fallback);

// Moves `<base>/veil` (the app's old name) to `<base>/qveil` when the new one
// doesn't exist yet. A cache holds nothing worth keeping, and what was watched
// besides, so with `cache` an old directory next to a new one is deleted.
void migrate(const QString &base, bool cache);

// Migrates all three, then makes sure each exists and is private (0700). The
// settings file the window's Qt Settings use is created private as well, so it
// is never readable by others between Qt writing it and a later chmod.
void prepare();

// Qt rewrites the settings file as it likes while the app runs, so this makes
// sure it ended up private. Called once the event loop is over.
void tightenSettings();

}
