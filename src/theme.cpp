// Theme watching follows Omacut (MIT, David Heinemeier Hansson).
#include "theme.h"

#include <QFontDatabase>
#include <QColor>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonDocument>
#include <QJsonObject>
#include <QRegularExpression>
#include <QStandardPaths>
#include <cmath>
#include <utility>

Theme::Theme(const QString &directory, QObject *parent, const QString &hyprctl, const QString &hyprEvents,
             const QString &fcMatch, const QString &gsettings)
    : QObject(parent),
      m_directory(directory.isEmpty() ? QDir::homePath() + "/.local/state/omarchy/current" : directory) {
    // The typeface is whatever `monospace` means on this system right now.
    // Qt resolves the alias once and never again, so the name is asked for
    // (as the Omarchy shell does) and asked again when fontconfig's settings
    // change, which is what `omarchy font set` does.
    m_fcMatch = !fcMatch.isEmpty() ? fcMatch : directory.isEmpty() ? QStandardPaths::findExecutable("fc-match") : QString();
    m_fontconfig = directory.isEmpty() ? QStandardPaths::writableLocation(QStandardPaths::GenericConfigLocation) + "/fontconfig"
                                       : QString();
    connect(&m_fontProcess, &QProcess::finished, this, [this](int code, QProcess::ExitStatus status) {
        const QString family = QString::fromUtf8(m_fontProcess.readAllStandardOutput()).trimmed();
        if (code == 0 && status == QProcess::NormalExit && !family.isEmpty() && family != m_fontFamily) {
            m_fontFamily = family;
            emit changed();
        }
    });

    // The interface typeface is the desktop's: the one its settings name,
    // which is what its other apps are set in. The toolkit's own general
    // font stands in until that is known, and when it isn't.
    m_uiDefault = QFontDatabase::systemFont(QFontDatabase::GeneralFont).family();
    m_uiFamily = m_uiDefault;
    m_gsettings = !gsettings.isEmpty() ? gsettings : directory.isEmpty() ? QStandardPaths::findExecutable("gsettings") : QString();
    connect(&m_uiProcess, &QProcess::finished, this, [this](int code, QProcess::ExitStatus status) {
        QString family;
        if (code == 0 && status == QProcess::NormalExit)
            family = familyOf(QString::fromUtf8(m_uiProcess.readAllStandardOutput()));
        if (family.isEmpty())
            family = m_uiDefault;
        if (family != m_uiFamily) {
            m_uiFamily = family;
            emit changed();
        }
    });

    m_debounce.setSingleShot(true);
    m_debounce.setInterval(80);
    connect(&m_watcher, &QFileSystemWatcher::directoryChanged, this, [this] { m_debounce.start(); });
    connect(&m_watcher, &QFileSystemWatcher::fileChanged, this, [this] { m_debounce.start(); });
    connect(&m_debounce, &QTimer::timeout, this, &Theme::reload);

    // Corners follow Hyprland's effective rounding, which themes and personal
    // overrides can both set, so ask hyprctl rather than reading its config.
    // Without Hyprland the corners stay sharp, like Omarchy's default.
    const bool inHyprland = directory.isEmpty() && !qEnvironmentVariableIsEmpty("HYPRLAND_INSTANCE_SIGNATURE");
    m_hyprctl = !hyprctl.isEmpty() ? hyprctl : inHyprland ? QStandardPaths::findExecutable("hyprctl") : QString();
    m_roundingTimeout.setSingleShot(true);
    m_roundingTimeout.setInterval(500);
    connect(&m_roundingTimeout, &QTimer::timeout, &m_roundingProcess, &QProcess::kill);
    connect(&m_roundingProcess, &QProcess::errorOccurred, this, [this](QProcess::ProcessError error) {
        m_roundingTimeout.stop();
        // A process that never started has no end to rerun from.
        if (error == QProcess::FailedToStart)
            m_roundingAgain = false;
    });
    connect(&m_roundingProcess, &QProcess::finished, this, [this](int code, QProcess::ExitStatus status) {
        m_roundingTimeout.stop();
        const auto value = QJsonDocument::fromJson(m_roundingProcess.readAllStandardOutput()).object().value("int");
        if (code == 0 && status == QProcess::NormalExit && value.isDouble() && value.toInt(-1) >= 0
            && value.toInt() != m_radius) {
            m_radius = value.toInt();
            emit changed();
        }
        if (std::exchange(m_roundingAgain, false))
            refreshRounding();
    });

    // Hyprland says when its config is reloaded, which is the only time the
    // rounding can change, so there is nothing to poll: one query now, and
    // another per reload.
    const QString runtime = qEnvironmentVariable("XDG_RUNTIME_DIR");
    const QString events = !hyprEvents.isEmpty() ? hyprEvents
                         : inHyprland && !runtime.isEmpty()
                             ? runtime + "/hypr/" + qEnvironmentVariable("HYPRLAND_INSTANCE_SIGNATURE") + "/.socket2.sock"
                             : QString();
    if (!m_hyprctl.isEmpty() && !events.isEmpty()) {
        connect(&m_events, &QLocalSocket::readyRead, this, &Theme::readEvents);
        m_events.connectToServer(events);
    }
    reload();
}

Theme::~Theme() {
    if (m_roundingProcess.state() != QProcess::NotRunning) {
        m_roundingProcess.kill();
        m_roundingProcess.waitForFinished(1000);
    }
    if (m_fontProcess.state() != QProcess::NotRunning) {
        m_fontProcess.kill();
        m_fontProcess.waitForFinished(1000);
    }
    if (m_uiProcess.state() != QProcess::NotRunning) {
        m_uiProcess.kill();
        m_uiProcess.waitForFinished(1000);
    }
}

void Theme::refreshRounding() {
    if (m_hyprctl.isEmpty())
        return;
    if (m_roundingProcess.state() != QProcess::NotRunning) {
        m_roundingAgain = true;
        return;
    }
    m_roundingProcess.start(m_hyprctl, {"-j", "getoption", "decoration:rounding"});
    m_roundingTimeout.start();
}

// Hyprland's events are lines of `name>>data`.
void Theme::readEvents() {
    m_eventBuffer += m_events.readAll();
    int newline;
    while ((newline = m_eventBuffer.indexOf('\n')) >= 0) {
        if (m_eventBuffer.left(newline).startsWith("configreloaded>>"))
            refreshRounding();
        m_eventBuffer.remove(0, newline + 1);
    }
    // No event is anywhere near this long, so what is left isn't one.
    if (m_eventBuffer.size() > 65536)
        m_eventBuffer.clear();
}

QString Theme::readAccent(const QString &path) { return readColour(path, "accent", "#FFD60A"); }

QString Theme::readColour(const QString &path, const QString &key, const QString &fallback) {
    QFile file(path);
    if (file.open(QIODevice::ReadOnly)) {
        const QRegularExpression expression("^\\s*" + QRegularExpression::escape(key) + R"re(\s*=\s*["'](#[0-9a-fA-F]{6})["'])re",
                                            QRegularExpression::MultilineOption);
        const auto match = expression.match(QString::fromUtf8(file.readAll()));
        if (match.hasMatch())
            return match.captured(1);
    }
    return fallback;
}

void Theme::refreshFont() {
    if (!m_gsettings.isEmpty() && m_uiProcess.state() == QProcess::NotRunning)
        m_uiProcess.start(m_gsettings, {"get", "org.gnome.desktop.interface", "font-name"});
    if (m_fcMatch.isEmpty() || m_fontProcess.state() != QProcess::NotRunning)
        return;
    m_fontProcess.start(m_fcMatch, {"-f", "%{family[0]}", "monospace"});
}

QString Theme::familyOf(const QString &description) {
    // 'Adwaita Sans 11': quoted, and ending in a size, sometimes after a
    // style. Words are dropped from the end until what is left is a family
    // that is installed.
    QString name = description.trimmed();
    if (name.size() >= 2 && (name.startsWith('\'') || name.startsWith('"')) && name.endsWith(name.at(0)))
        name = name.mid(1, name.size() - 2).trimmed();
    QStringList words = name.split(' ', Qt::SkipEmptyParts);
    while (!words.isEmpty()) {
        const QString family = words.join(' ');
        if (QFontDatabase::hasFamily(family))
            return family;
        words.removeLast();
    }
    return {};
}

// Black or white, whichever reads on the accent (WCAG relative luminance).
QString Theme::accentForeground() const {
    const QColor c(m_accent);
    auto linear = [](double v) { return v <= .04045 ? v / 12.92 : std::pow((v + .055) / 1.055, 2.4); };
    const double luminance = .2126 * linear(c.redF()) + .7152 * linear(c.greenF()) + .0722 * linear(c.blueF());
    return luminance > .179 ? "black" : "white";
}

void Theme::reload() {
    refreshRounding();
    const auto paths = m_watcher.files() + m_watcher.directories();
    if (!paths.isEmpty())
        m_watcher.removePaths(paths);

    // A theme switch replaces symlinks and files, so watch the parents too and
    // re-arm after every change. Watching the nearest existing ancestor means
    // installing Omarchy later works without restarting the app.
    QString ancestor = m_directory;
    while (!QFileInfo::exists(ancestor) && ancestor != "/")
        ancestor = QFileInfo(ancestor).absolutePath();
    QStringList candidates{ancestor, QFileInfo(ancestor).absolutePath(), m_directory,
                           m_directory + "/theme", m_directory + "/theme/colors.toml"};
    // Where `omarchy font set` writes which font `monospace` is.
    if (!m_fontconfig.isEmpty())
        candidates << m_fontconfig << m_fontconfig + "/fonts.conf";
    candidates.removeDuplicates();
    for (const auto &path : candidates)
        if (QFileInfo::exists(path))
            m_watcher.addPath(path);

    const QString colours = m_directory + "/theme/colors.toml";
    const auto accent = readAccent(colours);
    const auto background = readColour(colours, "background", "#111115");
    const auto foreground = readColour(colours, "foreground", "#e9e4f0");
    if (accent != m_accent || background != m_background || foreground != m_foreground) {
        m_accent = accent;
        m_background = background;
        m_foreground = foreground;
        emit changed();
    }
    refreshFont();
}
