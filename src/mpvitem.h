#pragma once

#include <QColor>
#include <QMutex>
#include <QQuickFramebufferObject>

#include <memory>

#include <mpv/client.h>
#include <mpv/render_gl.h>

class MpvItem;

// What an MpvItem and its renderer share. The item lives on the GUI thread
// and the renderer on the render thread, and either can be destroyed first,
// so neither owns the player alone: it is destroyed when the last of them lets
// go, and the renderer always frees the render context before that.
struct MpvShared {
    explicit MpvShared(mpv_handle *player) : handle(player) {}
    ~MpvShared();

    mpv_handle *const handle;
    // Created, used and freed by the renderer alone, on the render thread with
    // its GL context current: render.h allows one mpv_render_* call at a time.
    mpv_render_context *render = nullptr;
    // mpv's threads post to the item through this; it is cleared (under the
    // mutex) before the item goes away.
    QMutex mutex;
    MpvItem *item = nullptr;
};

// libmpv rendered into the Qt Quick scene. mpv runs without any user config
// or scripts, keeps no resume or history state, and is only ever handed
// loopback URLs from qveil-core.
class MpvItem : public QQuickFramebufferObject {
    Q_OBJECT
    Q_PROPERTY(double position READ position NOTIFY positionChanged)
    Q_PROPERTY(double duration READ duration NOTIFY durationChanged)
    Q_PROPERTY(double cached READ cached NOTIFY cachedChanged)
    Q_PROPERTY(bool paused READ paused NOTIFY pausedChanged)
    Q_PROPERTY(bool buffering READ buffering NOTIFY bufferingChanged)
    Q_PROPERTY(bool loaded READ loaded NOTIFY loadedChanged)
    Q_PROPERTY(bool ended READ ended NOTIFY endedChanged)
    // Whether the current file has started playing: its first frame is up.
    Q_PROPERTY(bool started READ started NOTIFY startedChanged)
    Q_PROPERTY(double volume READ volume WRITE setVolume NOTIFY volumeChanged)
    Q_PROPERTY(bool muted READ muted WRITE setMuted NOTIFY mutedChanged)
    // How fast it plays: 1 is as recorded.
    Q_PROPERTY(double speed READ speed WRITE setSpeed NOTIFY speedChanged)
    // What mpv paints where there is no video: before the first frame and in
    // the letterbox. It has to be the scene's own colour behind the item.
    Q_PROPERTY(QColor backgroundColor READ backgroundColor WRITE setBackgroundColor NOTIFY backgroundColorChanged)

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
    bool started() const { return m_started; }
    double volume() const { return m_volume; }
    bool muted() const { return m_muted; }
    void setVolume(double volume);
    void setMuted(bool muted);
    double speed() const { return m_speed; }
    void setSpeed(double speed);
    QColor backgroundColor() const { return m_background; }
    void setBackgroundColor(const QColor &color);

signals:
    void positionChanged();
    void durationChanged();
    void cachedChanged();
    void pausedChanged();
    void bufferingChanged();
    void loadedChanged();
    void endedChanged();
    void startedChanged();
    void volumeChanged();
    void mutedChanged();
    void speedChanged();
    void backgroundColorChanged();
    void failed(const QString &message);

private slots:
    void handleEvents();

private:
    void command(const QList<QByteArray> &args);
    void onProperty(const mpv_event_property *p);
    void setStarted(bool started);

    mpv_handle *m_mpv = nullptr;
    std::shared_ptr<MpvShared> m_shared;
    double m_position = 0, m_duration = 0, m_cached = 0, m_volume = 100, m_speed = 1;
    bool m_paused = false, m_cachePaused = false, m_seeking = false, m_loading = false;
    bool m_loaded = false, m_ended = false, m_muted = false, m_started = false;
    QColor m_background = Qt::black;
};
