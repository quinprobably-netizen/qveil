#pragma once

#include <QQuickFramebufferObject>

#include <mpv/client.h>
#include <mpv/render_gl.h>

// libmpv rendered into the Qt Quick scene. mpv runs without any user config
// or scripts, keeps no resume or history state, and is only ever handed
// loopback URLs from veil-core.
class MpvItem : public QQuickFramebufferObject {
    Q_OBJECT
    Q_PROPERTY(double position READ position NOTIFY positionChanged)
    Q_PROPERTY(double duration READ duration NOTIFY durationChanged)
    Q_PROPERTY(double cached READ cached NOTIFY cachedChanged)
    Q_PROPERTY(bool paused READ paused NOTIFY pausedChanged)
    Q_PROPERTY(bool buffering READ buffering NOTIFY bufferingChanged)
    Q_PROPERTY(bool loaded READ loaded NOTIFY loadedChanged)
    Q_PROPERTY(bool ended READ ended NOTIFY endedChanged)
    Q_PROPERTY(double volume READ volume WRITE setVolume NOTIFY volumeChanged)
    Q_PROPERTY(bool muted READ muted WRITE setMuted NOTIFY mutedChanged)

public:
    explicit MpvItem(QQuickItem *parent = nullptr);
    ~MpvItem() override;

    Renderer *createRenderer() const override;

    Q_INVOKABLE void load(const QString &video, const QString &audio, double start);
    Q_INVOKABLE void stop();
    Q_INVOKABLE void togglePause();
    Q_INVOKABLE void seek(double seconds, bool relative);

    double position() const { return m_position; }
    double duration() const { return m_duration; }
    double cached() const { return m_cached; }
    bool paused() const { return m_paused; }
    bool buffering() const { return m_loading || m_cachePaused || m_seeking; }
    bool loaded() const { return m_loaded; }
    bool ended() const { return m_ended; }
    double volume() const { return m_volume; }
    bool muted() const { return m_muted; }
    void setVolume(double volume);
    void setMuted(bool muted);

    mpv_handle *handle() const { return m_mpv; }
    mpv_render_context *renderContext = nullptr;

signals:
    void positionChanged();
    void durationChanged();
    void cachedChanged();
    void pausedChanged();
    void bufferingChanged();
    void loadedChanged();
    void endedChanged();
    void volumeChanged();
    void mutedChanged();
    void failed(const QString &message);

private slots:
    void handleEvents();

private:
    void command(const QList<QByteArray> &args);
    void onProperty(const mpv_event_property *p);

    mpv_handle *m_mpv = nullptr;
    double m_position = 0, m_duration = 0, m_cached = 0, m_volume = 100;
    bool m_paused = false, m_cachePaused = false, m_seeking = false, m_loading = false;
    bool m_loaded = false, m_ended = false, m_muted = false;
};
