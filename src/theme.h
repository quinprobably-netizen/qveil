#pragma once

#include <QFileSystemWatcher>
#include <QLocalSocket>
#include <QObject>
#include <QProcess>
#include <QTimer>

class Theme : public QObject {
    Q_OBJECT
    Q_PROPERTY(QString accent READ accent NOTIFY changed)
    Q_PROPERTY(QString accentForeground READ accentForeground NOTIFY changed)
    Q_PROPERTY(int radius READ radius NOTIFY changed)
    // The room: the theme's own background and text colours, so the window
    // sits in the desktop the way the bar and its panels do.
    Q_PROPERTY(QString background READ background NOTIFY changed)
    Q_PROPERTY(QString foreground READ foreground NOTIFY changed)
    // The system's typeface: what fontconfig's `monospace` alias resolves to,
    // which is what `omarchy font set` changes and the bar draws with.
    Q_PROPERTY(QString fontFamily READ fontFamily NOTIFY changed)
    // The colour of a protected route (Tor, proxy): a muted green that is the
    // same on every theme, so "protected" never reads as the decorative accent.
    Q_PROPERTY(QString protectedColor READ protectedColor CONSTANT)

public:
    // `hyprctl`, `hyprEvents` (Hyprland's event socket) and `fcMatch` are only
    // given by tests; otherwise they are found from the environment.
    explicit Theme(const QString &currentDirectory = {}, QObject *parent = nullptr, const QString &hyprctl = {},
                   const QString &hyprEvents = {}, const QString &fcMatch = {});
    ~Theme() override;

    QString accent() const { return m_accent; }
    QString accentForeground() const;
    int radius() const { return m_radius; }
    QString background() const { return m_background; }
    QString foreground() const { return m_foreground; }
    QString fontFamily() const { return m_fontFamily; }
    QString protectedColor() const { return "#5fae83"; }

    static QString readAccent(const QString &path);
    // The colour `key` of a colors.toml, or `fallback`.
    static QString readColour(const QString &path, const QString &key, const QString &fallback);

signals:
    void changed();

private:
    void reload();
    void refreshRounding();
    void refreshFont();
    void readEvents();

    QString m_directory;
    QString m_accent = "#FFD60A";
    QString m_background = "#111115";
    QString m_foreground = "#e9e4f0";
    QString m_fontFamily = "monospace";
    QString m_fcMatch;
    QProcess m_fontProcess;
    QString m_fontconfig;
    QFileSystemWatcher m_watcher;
    QTimer m_debounce;
    QString m_hyprctl;
    int m_radius = 0;
    QProcess m_roundingProcess;
    QTimer m_roundingTimeout;
    // Set when rounding is asked for while a query is still running, whose
    // answer may predate the change that asked.
    bool m_roundingAgain = false;
    QLocalSocket m_events;
    QByteArray m_eventBuffer;
};
