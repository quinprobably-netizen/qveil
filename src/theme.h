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
    // The colour of a protected route (Tor, proxy): a muted green that is the
    // same on every theme, so "protected" never reads as the decorative accent.
    Q_PROPERTY(QString protectedColor READ protectedColor CONSTANT)

public:
    // `hyprctl` and `hyprEvents` (Hyprland's event socket) are only given by
    // tests; otherwise they are found from the environment.
    explicit Theme(const QString &currentDirectory = {}, QObject *parent = nullptr, const QString &hyprctl = {},
                   const QString &hyprEvents = {});
    ~Theme() override;

    QString accent() const { return m_accent; }
    QString accentForeground() const;
    int radius() const { return m_radius; }
    QString protectedColor() const { return "#5fae83"; }

    static QString readAccent(const QString &path);

signals:
    void changed();

private:
    void reload();
    void refreshRounding();
    void readEvents();

    QString m_directory;
    QString m_accent = "#FFD60A";
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
