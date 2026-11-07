// Theme watching follows Omacut (MIT, David Heinemeier Hansson).
#include "theme.h"

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

Theme::Theme(const QString &directory, QObject *parent, const QString &hyprctl, const QString &hyprEvents)
    : QObject(parent),
      m_directory(directory.isEmpty() ? QDir::homePath() + "/.local/state/omarchy/current" : directory) {
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

QString Theme::readAccent(const QString &path) {
    QFile file(path);
    if (file.open(QIODevice::ReadOnly)) {
        const QRegularExpression expression(R"re(^\s*accent\s*=\s*["'](#[0-9a-fA-F]{6})["'])re",
                                            QRegularExpression::MultilineOption);
        const auto match = expression.match(QString::fromUtf8(file.readAll()));
        if (match.hasMatch())
            return match.captured(1);
    }
    return "#FFD60A";
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
    candidates.removeDuplicates();
    for (const auto &path : candidates)
        if (QFileInfo::exists(path))
            m_watcher.addPath(path);

    const auto accent = readAccent(m_directory + "/theme/colors.toml");
    if (accent != m_accent) {
        m_accent = accent;
        emit changed();
    }
}
