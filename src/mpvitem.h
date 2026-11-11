#pragma once

#include <QColor>
#include <QMutex>
#include <QPointer>
#include <QQuickFramebufferObject>
#include <QTimer>

#include <atomic>
#include <memory>

#include <mpv/client.h>
#include <mpv/render_gl.h>

class MpvItem;
class QQuickWindow;

// An item, as mpv's threads and the renderer reach it: it is cleared (under
// the mutex) before the item goes away.
struct MpvLink {
    QMutex mutex;
    MpvItem *item = nullptr;
    void post(const char *method);
};

// One mpv player: what plays, its streams, what is cached of them, where it
// has got to. It belongs to no window. The item that has it shows it and
// gets its events, and can hand it to another item, in another window, which
// goes on with it where it was (see MpvItem::takeOver). It is destroyed when
// the last of the items and renderers that hold it lets go, and a renderer
// always frees the render context before that.
struct MpvCore {
    MpvCore();
    ~MpvCore();

    mpv_handle *handle = nullptr;
    QMutex mutex;
    // The item that has it: mpv's threads post its events there.
    MpvItem *item = nullptr;
    // The render context. render.h allows one per player and one mpv_render_*
    // call at a time, so it is made, used and freed on the render thread of
    // one window, with that window's GL context current; and another is only
    // made once it is gone. `owner` is the renderer that made it, `drawer` the
    // item that renderer draws, `window` that item's window. Under the mutex.
    mpv_render_context *render = nullptr;
    const void *owner = nullptr;
    std::shared_ptr<MpvLink> drawer;
    QPointer<QQuickWindow> window;
    // Whether there is a render context, for the GUI thread to ask.
    std::atomic<bool> ready{false};
    // Counts handovers, so that what was asked for an earlier one is let be.
    std::atomic<quint64> handover{0};
    // Its video output is off, for a handover: the renderer that makes the
    // next render context switches it on.
    std::atomic<bool> videoOff{false};
    // What its "hwdec" and "gpu-hwdec-interop" are set to (see
    // switchVideo, learnHardware). Under the mutex.
    QByteArray hwdec, interop;
    // When the handover under way began (ms, see handoverLog), and whether
    // its video output is back on, so the first frame after it is told of.
    std::atomic<qint64> handoverAt{0};
    std::atomic<bool> handoverShowing{false};
    // With QVEIL_PIP_TIMING set, says on stderr how long into the handover
    // under way `stage` was reached. Nothing else is said.
    void handoverLog(const char *stage);

    // Frees the render context on the thread that made it; `owner` null for
    // whichever renderer made it. Says whether there was one to free.
    bool freeRender(const void *owner);
    // Posts to the item that has it.
    void post(const char *method);
    void post(const char *method, quint64 argument);
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
    // How much larger than its own size the item is drawn on screen (the
    // window draws itself larger when it is wide). The video is rendered at
    // that size, so that it isn't rendered small and stretched.
    Q_PROPERTY(double renderScale READ renderScale WRITE setRenderScale NOTIFY renderScaleChanged)
    // What mpv paints where there is no video: before the first frame and in
    // the letterbox. It has to be the scene's own colour behind the item.
    Q_PROPERTY(QColor backgroundColor READ backgroundColor WRITE setBackgroundColor NOTIFY backgroundColorChanged)
    // True while a player this item handed on is still drawn here: its
    // window has to stay up until then (see takeOver).
    Q_PROPERTY(bool handingOver READ handingOver NOTIFY handingOverChanged)

public:
    explicit MpvItem(QQuickItem *parent = nullptr);
    ~MpvItem() override;

    Renderer *createRenderer() const override;

    Q_INVOKABLE void load(const QString &video, const QString &audio, double start);
    Q_INVOKABLE void stop();
    Q_INVOKABLE void togglePause();
    Q_INVOKABLE void seek(double seconds, bool relative);
    // Takes the player `from` has, as it is: what it plays goes on here from
    // where it is, and nothing is opened again. `from` is left with none.
    Q_INVOKABLE bool takeOver(MpvItem *from);

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
    double renderScale() const { return m_renderScale; }
    void setRenderScale(double scale);
    void setSpeed(double speed);
    QColor backgroundColor() const { return m_background; }
    void setBackgroundColor(const QColor &color);
    bool handingOver() const { return m_handingOver; }

    // The player this item shows (the renderer reads it while the GUI thread waits).
    std::shared_ptr<MpvCore> core() const { return m_core; }

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
    void renderScaleChanged();
    void backgroundColorChanged();
    void handingOverChanged();
    void failed(const QString &message);
    // A handover (takeOver) couldn't move the picture here, and the player
    // was let go of: what it played has to be opened again.
    void takeOverFailed();

private slots:
    void handleEvents();
    // This item's renderer has made the player's render context.
    void renderReady();
    // The render context of the player this item has was freed elsewhere:
    // one can be made here.
    void renderReleased() { update(); }
    // A render context this item's renderer made was freed.
    void renderFreed();
    // The window that had to free the render context couldn't.
    void releaseLost(qulonglong handover);

private:
    void ensureCore();
    // Lets go of `core`: it stops, and its render context is freed where it was made.
    static void retire(const std::shared_ptr<MpvCore> &core);
    // Gives up on a handover that doesn't finish (see takeOver).
    void abandon();
    void command(const QList<QByteArray> &args);
    void onProperty(const mpv_event_property *p);
    void setStarted(bool started);
    void setHandingOver(bool handing);
    void takeState(MpvItem *from);
    void clearState();

    std::shared_ptr<MpvCore> m_core;
    std::shared_ptr<MpvLink> m_link;
    QTimer m_handoverTimeout, m_handingOverTimeout;
    // A video that was asked for before there was anything to draw it with
    // (see load()).
    void startLoad();
    bool m_waiting = false;
    quint64 m_asked = 0;
    QString m_video, m_audio;
    double m_start = 0;
    double m_position = 0, m_duration = 0, m_cached = 0, m_volume = 100, m_speed = 1, m_renderScale = 1;
    bool m_paused = false, m_cachePaused = false, m_seeking = false, m_loading = false;
    bool m_loaded = false, m_ended = false, m_muted = false, m_started = false, m_handingOver = false;
    QColor m_background = Qt::black;
};
