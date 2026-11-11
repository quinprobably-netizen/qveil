// The render-API embedding follows mpv-examples/libmpv/qml (public domain).
#include "mpvitem.h"

#include <QGuiApplication>
#include <QOpenGLContext>
#include <QOpenGLFramebufferObject>
#include <QQuickWindow>
#include <QRunnable>
#include <QSGRendererInterface>
#include <QElapsedTimer>
#include <QtGui/qguiapplication_platform.h>

#include <cstdio>
#include <mutex>

#include <vector>

namespace {

void *procAddress(void *, const char *name) {
    QOpenGLContext *context = QOpenGLContext::currentContext();
    return context ? reinterpret_cast<void *>(context->getProcAddress(QByteArray(name))) : nullptr;
}

// Both callbacks arrive on mpv's threads.
void onRenderUpdate(void *link) { static_cast<MpvLink *>(link)->post("update"); }
void onWakeup(void *core) { static_cast<MpvCore *>(core)->post("handleEvents"); }

// The replies to switching the video output off and on for a handover carry
// which handover they were for, and which of the two they were.
quint64 voTag(quint64 handover, bool on) { return handover << 1 | (on ? 1 : 0); }

// For the handover's timing (see MpvCore::handoverLog).
const bool timingWanted = qEnvironmentVariableIsSet("QVEIL_PIP_TIMING");
qint64 now() {
    static QElapsedTimer clock;
    static std::once_flag started;
    std::call_once(started, [] { clock.start(); });
    return clock.elapsed() + 1;
}

// Says when the first frame drawn after a handover is on screen.
class ShownJob : public QRunnable {
public:
    explicit ShownJob(std::shared_ptr<MpvCore> core) : m_core(std::move(core)) {}
    void run() override {
        m_core->handoverLog("on screen");
        m_core->handoverAt = 0;
    }

private:
    std::shared_ptr<MpvCore> m_core;
};

// How the video is decoded and drawn. Left to itself, a render context
// loads every hardware interop mpv has, and some take long to find there is
// nothing for them (CUDA's, on a machine without NVIDIA's card, about a
// second); and with its video output off, "auto-safe" tries every copying
// decoder, some as slow to fail, while the sound waits. So once a video has
// been decoded straight into a render context, that decoder and its interop
// are all that is asked for from then on (see learnHardware): a handover
// then takes a few frames, not seconds. Under `hardware`.
constexpr const char *kHwdec = "auto-safe";
QMutex hardware;
QByteArray directHwdec, learnedInterop;

// The interop that takes frames from the decoder `hwdec` names, for OpenGL.
// Copying decoders and software decoding need none, and say nothing.
QByteArray interopFor(const QByteArray &hwdec) {
    if (hwdec == "vaapi")
        return "vaapi";
    if (hwdec == "nvdec" || hwdec == "cuda")
        return "cuda";
    if (hwdec == "vdpau")
        return "vdpau-gl";
    if (hwdec == "drm" || hwdec == "v4l2m2m")
        return "drmprime";
    return {};
}

// Runs on the GUI thread, so it never asks mpv and waits: what mpv said of
// its decoder, as it said it, is enough.
void learnHardware(MpvCore &core) {
    QByteArray hwdec;
    {
        QMutexLocker lock(&core.mutex);
        hwdec = core.hwdecCurrent;
    }
    const QByteArray found = interopFor(hwdec);
    if (found.isEmpty())
        return;
    {
        QMutexLocker lock(&hardware);
        directHwdec = hwdec;
        learnedInterop = found;
    }
    // Only when it changes, and without waiting: mpv sets its decoder up
    // again for it, which waits for a frame to be drawn, which waits for this
    // thread. (It is read when the next render context is made.)
    {
        QMutexLocker lock(&core.mutex);
        if (core.interop == found)
            return;
        core.interop = found;
    }
    QByteArray value = found;
    char *data = value.data();
    mpv_set_property_async(core.handle, 0, "gpu-hwdec-interop", MPV_FORMAT_STRING, &data);
}

// Switches the video output off or on for a handover; the reply to the
// output's switch carries `tag`. With it off, the video is decoded the
// direct way that can't be had without it (so in software, at once), or in
// software outright; with it on, the direct way is found at once.
void switchVideo(MpvCore &core, bool on, quint64 tag) {
    QByteArray hwdec;
    {
        QMutexLocker lock(&hardware);
        hwdec = !directHwdec.isEmpty() ? directHwdec : QByteArray(on ? kHwdec : "no");
    }
    // (mpv is never called with the mutex held: its callbacks take it.)
    auto setHwdec = [&] {
        {
            QMutexLocker lock(&core.mutex);
            if (core.hwdec == hwdec)
                return;
            core.hwdec = hwdec;
        }
        char *value = hwdec.data();
        mpv_set_property_async(core.handle, 0, "hwdec", MPV_FORMAT_STRING, &value);
    };
    char *vo = const_cast<char *>(on ? "libmpv" : "null");
    if (!on)
        setHwdec();
    mpv_set_property_async(core.handle, tag, "vo", MPV_FORMAT_STRING, &vo);
    if (on)
        setHwdec();
}

// Frees a player's render context on the render thread of the window it
// was made in, once its video output is off (see MpvItem::takeOver). It runs
// as that window synchronizes its next frame. If the window goes before it
// draws one, the job is deleted without running, and the item that has the
// player hears so.
class ReleaseJob : public QRunnable {
public:
    ReleaseJob(std::shared_ptr<MpvCore> core, quint64 handover, const void *owner)
        : m_core(std::move(core)), m_handover(handover), m_owner(owner) {}
    ~ReleaseJob() override {
        if (!m_ran)
            m_core->post("releaseLost", m_handover);
    }
    void run() override {
        // Run as the window synchronizes, on its render thread with its
        // context current, as freeing a render context needs.
        Q_ASSERT(QQuickWindow::graphicsApi() != QSGRendererInterface::OpenGL || QOpenGLContext::currentContext());
        m_ran = true;
        // A later handover asks for its own.
        if (m_core->handover == m_handover)
            m_core->freeRender(m_owner);
    }

private:
    std::shared_ptr<MpvCore> m_core;
    const quint64 m_handover;
    const void *const m_owner;
    bool m_ran = false;
};

class MpvRenderer : public QQuickFramebufferObject::Renderer {
public:
    explicit MpvRenderer(std::shared_ptr<MpvLink> link) : m_link(std::move(link)) {}

    // The scene graph deletes the renderer on the render thread with its GL
    // context current, which is exactly what freeing a render context needs.
    // The item can't do it: it is destroyed on the GUI thread, while this
    // thread may be inside mpv_render_context_render.
    ~MpvRenderer() override {
        if (m_made.empty())
            return;
        if (!QOpenGLContext::currentContext()) {
            // Freeing without the context would crash in the driver, and
            // destroying the player with the context alive is undefined, so
            // keep the players for the rest of the process instead.
            qWarning("libmpv: no current OpenGL context to free the render context in");
            new std::vector<std::shared_ptr<MpvCore>>(m_made);
            return;
        }
        for (const auto &core : m_made)
            core->freeRender(this);
    }

    // Runs on the render thread while the GUI thread waits; the only moment
    // the item can be read safely. The GL context is current.
    void synchronize(QQuickFramebufferObject *item) override {
        m_window = item->window();
        // The scene graph is told not to follow the item's size itself: it
        // would compare the framebuffer with the item's own size and make a
        // new one on every frame while the two differ, which they do whenever
        // the window is drawn larger. It is made again here, when the size it
        // is for changes.
        auto *mpv = static_cast<MpvItem *>(item);
        const double scale = mpv->renderScale();
        const double ratio = m_window ? m_window->effectiveDevicePixelRatio() : 1;
        const QSize size = (item->size() * ratio * scale).toSize();
        if (size != m_size) {
            m_size = size;
            invalidateFramebufferObject();
        }

        m_core = mpv->core();
        // What this made a render context for and nobody plays any more is
        // freed. One that went on to another item is freed once its video
        // output is off (see MpvItem::takeOver), and drawn here until then.
        for (auto it = m_made.begin(); it != m_made.end();) {
            const std::shared_ptr<MpvCore> &core = *it;
            bool mine, wanted;
            {
                QMutexLocker lock(&core->mutex);
                mine = core->owner == this;
                wanted = core->item != nullptr;
            }
            if (mine && !wanted) {
                core->freeRender(this);
                mine = false;
            }
            it = mine ? it + 1 : m_made.erase(it);
        }
        if (m_core)
            make(m_core);
    }

    QOpenGLFramebufferObject *createFramebufferObject(const QSize &size) override {
        return QQuickFramebufferObject::Renderer::createFramebufferObject(m_size.isEmpty() ? size : m_size);
    }

    void render() override {
        mpv_render_context *context = drawn();
        if (!context || !m_window)
            return;
        // The first frame of a handover's (see MpvCore::handoverLog).
        if (timingWanted && m_core && m_core->handoverShowing && (mpv_render_context_update(context) & MPV_RENDER_UPDATE_FRAME)) {
            m_core->handoverShowing = false;
            m_core->handoverLog("first frame drawn");
            m_window->scheduleRenderJob(new ShownJob(m_core), QQuickWindow::AfterSwapStage);
        }
        m_window->beginExternalCommands();
        QOpenGLFramebufferObject *fbo = framebufferObject();
        mpv_opengl_fbo target{static_cast<int>(fbo->handle()), fbo->width(), fbo->height(), 0};
        int flip = 0;
        mpv_render_param params[] = {
            {MPV_RENDER_PARAM_OPENGL_FBO, &target},
            {MPV_RENDER_PARAM_FLIP_Y, &flip},
            {MPV_RENDER_PARAM_INVALID, nullptr},
        };
        mpv_render_context_render(context, params);
        m_window->endExternalCommands();
    }

private:
    // The item's player, or until it is let go of, the one the item handed on.
    mpv_render_context *drawn() {
        for (const auto &core : {m_core, m_made.empty() ? nullptr : m_made.back()}) {
            if (!core)
                continue;
            QMutexLocker lock(&core->mutex);
            if (core->owner == this)
                return core->render;
        }
        return nullptr;
    }

    // Makes the render context for `core`, unless it has one: then it is
    // made once that is freed, which says so (see MpvCore::freeRender).
    void make(const std::shared_ptr<MpvCore> &core) {
        {
            QMutexLocker lock(&core->mutex);
            if (core->render)
                return;
        }
        if (m_failed.lock() == core)
            return;
        mpv_opengl_init_params gl{procAddress, nullptr};
        QList<mpv_render_param> params{
            {MPV_RENDER_PARAM_API_TYPE, const_cast<char *>(MPV_RENDER_API_TYPE_OPENGL)},
            {MPV_RENDER_PARAM_OPENGL_INIT_PARAMS, &gl},
        };
#if QT_CONFIG(wayland)
        // Lets mpv share hardware-decoded frames with the scene on Wayland.
        if (auto *wayland = qGuiApp->nativeInterface<QNativeInterface::QWaylandApplication>())
            if (wl_display *display = wayland->display())
                params.append({MPV_RENDER_PARAM_WL_DISPLAY, display});
#endif
        params.append({MPV_RENDER_PARAM_INVALID, nullptr});
        mpv_render_context *context = nullptr;
        if (mpv_render_context_create(&context, core->handle, params.data()) < 0) {
            qWarning("libmpv: couldn't create the OpenGL render context");
            m_failed = core;
            return;
        }
        {
            QMutexLocker lock(&core->mutex);
            core->render = context;
            core->owner = this;
            core->drawer = m_link;
            core->window = m_window;
            core->ready = true;
        }
        mpv_render_context_set_update_callback(context, onRenderUpdate, m_link.get());
        m_made.push_back(core);
        core->handoverLog("render context made");
        // A handover's video comes back at once, not after a turn of the GUI thread.
        if (core->videoOff.exchange(false))
            switchVideo(*core, true, voTag(core->handover, true));
        // A video, or a handover, may be waiting for this.
        core->post("renderReady");
    }

    std::shared_ptr<MpvLink> m_link;
    // What the item has; and every player this made a render context for
    // that still has it.
    std::shared_ptr<MpvCore> m_core;
    std::vector<std::shared_ptr<MpvCore>> m_made;
    std::weak_ptr<MpvCore> m_failed;
    QQuickWindow *m_window = nullptr;
    QSize m_size;
};

} // namespace

void MpvLink::post(const char *method) {
    QMutexLocker lock(&mutex);
    if (item)
        QMetaObject::invokeMethod(item, method, Qt::QueuedConnection);
}

void MpvCore::handoverLog(const char *stage) {
    const qint64 at = handoverAt;
    if (timingWanted && at > 0)
        std::fprintf(stderr, "qveil: handover +%lld ms: %s\n", static_cast<long long>(now() - at), stage);
}

void MpvCore::post(const char *method) {
    QMutexLocker lock(&mutex);
    if (item)
        QMetaObject::invokeMethod(item, method, Qt::QueuedConnection);
}

void MpvCore::post(const char *method, quint64 argument) {
    QMutexLocker lock(&mutex);
    if (item)
        QMetaObject::invokeMethod(item, method, Qt::QueuedConnection, Q_ARG(qulonglong, argument));
}

bool MpvCore::freeRender(const void *expected) {
    mpv_render_context *context;
    {
        QMutexLocker lock(&mutex);
        if (!render || (expected && owner != expected))
            return false;
        context = render;
    }
    // Every mpv_render_* call for it happens on this thread, so none can
    // overlap these two. After the first, mpv's threads no longer reach the
    // item it drew in. It stays set while it is freed, so that no other
    // renderer makes one meanwhile.
    mpv_render_context_set_update_callback(context, nullptr, nullptr);
    mpv_render_context_free(context);
    std::shared_ptr<MpvLink> was;
    {
        QMutexLocker lock(&mutex);
        render = nullptr;
        owner = nullptr;
        was = std::move(drawer);
        window.clear();
        ready = false;
    }
    handoverLog("render context freed");
    if (was)
        was->post("renderFreed");
    post("renderReleased");
    return true;
}

namespace {
std::atomic<int> livePlayers{0};
}

int MpvCore::players() { return livePlayers; }

MpvCore::MpvCore() : handle(mpv_create()) {
    if (!handle)
        qFatal("libmpv: couldn't create a player");
    ++livePlayers;
    const std::pair<const char *, const char *> options[] = {
        {"vo", "libmpv"},
        // Nothing from the user's mpv setup: no config, no scripts (ytdl_hook
        // among them), no input bindings, no resume files, no history.
        {"config", "no"},
        {"load-scripts", "no"},
        // load-scripts doesn't cover mpv's built-in scripts; turn those off too.
        {"load-stats-overlay", "no"},
        {"load-console", "no"},
        {"load-osd-console", "no"},
        {"load-select", "no"},
        {"load-commands", "no"},
        {"load-context-menu", "no"},
        {"load-positioning", "no"},
        {"load-auto-profiles", "no"},
        {"ytdl", "no"},
        {"osc", "no"},
        {"input-default-bindings", "no"},
        {"input-vo-keyboard", "no"},
        {"save-position-on-quit", "no"},
        {"resume-playback", "no"},
        {"save-watch-history", "no"},
        {"terminal", "no"},
        // At error level mpv still names the URL it fails to open, and every
        // URL it is given here holds the engine's token. `terminal` is off,
        // which silences it too, but a log must not hang on one setting: with
        // the terminal on, the URL goes to stdout and stderr and so to the
        // journal. A failure still reaches the window as an event.
        {"msg-level", "all=no"},
        {"keep-open", "yes"},
        {"cache", "yes"},
        {"demuxer-max-bytes", "256MiB"},
        {"demuxer-readahead-secs", "90"},
        {"network-timeout", "60"},
        {"audio-client-name", "qveil"},
        // mpv's default background is a grey checkerboard, which shows before
        // the first frame and wherever the video doesn't reach. A plain colour
        // instead (set from setBackgroundColor(), black until then), the same
        // one for the letterbox and for the area before there is any picture.
        {"background", "color"},
        {"border-background", "color"},
        {"background-color", "#ff000000"},
    };
    for (const auto &[name, value] : options)
        mpv_set_option_string(handle, name, value);
    // For the tests only: their players play to no sound device ("null").
    // Nothing else sets it, and the app never does.
    if (qEnvironmentVariableIsSet("QVEIL_MPV_AO"))
        mpv_set_option_string(handle, "ao", qgetenv("QVEIL_MPV_AO").constData());
    {
        // As what was learnt of this machine says (see learnHardware).
        QMutexLocker lock(&hardware);
        hwdec = directHwdec.isEmpty() ? QByteArray(kHwdec) : directHwdec;
        mpv_set_option_string(handle, "hwdec", hwdec.constData());
        interop = learnedInterop;
        if (!interop.isEmpty())
            mpv_set_option_string(handle, "gpu-hwdec-interop", interop.constData());
    }
    if (mpv_initialize(handle) < 0)
        qFatal("libmpv: couldn't initialize the player");

    const std::pair<const char *, mpv_format> observed[] = {
        {"time-pos", MPV_FORMAT_DOUBLE},       {"duration", MPV_FORMAT_DOUBLE},
        {"demuxer-cache-time", MPV_FORMAT_DOUBLE}, {"pause", MPV_FORMAT_FLAG},
        {"paused-for-cache", MPV_FORMAT_FLAG}, {"seeking", MPV_FORMAT_FLAG},
        {"eof-reached", MPV_FORMAT_FLAG},      {"volume", MPV_FORMAT_DOUBLE},
        {"mute", MPV_FORMAT_FLAG},             {"speed", MPV_FORMAT_DOUBLE},
        {"hwdec-current", MPV_FORMAT_STRING},
    };
    for (const auto &[name, format] : observed)
        mpv_observe_property(handle, 0, name, format);
    mpv_set_wakeup_callback(handle, onWakeup, this);
}

MpvCore::~MpvCore() {
    mpv_set_wakeup_callback(handle, nullptr, nullptr);
    // The renderer frees the render context before it lets go of this, and a
    // player that was never drawn never had one, so mpv needs no GL context
    // any more. Destroying the core with a live render context is undefined
    // (render.h): should one be left, the player is kept instead.
    if (render) {
        qWarning("libmpv: a player was let go of with its render context");
        return;
    }
    mpv_terminate_destroy(handle);
    --livePlayers;
}

MpvItem::MpvItem(QQuickItem *parent) : QQuickFramebufferObject(parent), m_link(std::make_shared<MpvLink>()) {
    // The renderer sizes the framebuffer (see MpvRenderer::synchronize).
    setTextureFollowsItemSize(false);
    m_link->item = this;
    // A handover whose picture doesn't arrive (see takeOver).
    m_handoverTimeout.setSingleShot(true);
    m_handoverTimeout.setInterval(2500);
    connect(&m_handoverTimeout, &QTimer::timeout, this, [this] {
        if (!m_core)
            return;
        QMutexLocker lock(&m_core->mutex);
        const bool stuck = m_core->render && m_core->drawer != m_link;
        lock.unlock();
        if (stuck)
            abandon();
    });
    // The window this item is in is let go of by then, whatever happened.
    m_handingOverTimeout.setSingleShot(true);
    m_handingOverTimeout.setInterval(3000);
    connect(&m_handingOverTimeout, &QTimer::timeout, this, [this] { setHandingOver(false); });
    ensureCore();
}

MpvItem::~MpvItem() {
    {
        // From here on, mpv's threads can no longer reach this object.
        QMutexLocker lock(&m_link->mutex);
        m_link->item = nullptr;
    }
    if (!m_core)
        return;
    bool mine;
    {
        QMutexLocker lock(&m_core->mutex);
        mine = m_core->item == this;
        if (mine)
            m_core->item = nullptr;
    }
    // The render context belongs to the renderer, which frees it on the render
    // thread, so the player may outlive this item for a frame. It mustn't play
    // on meanwhile; once nothing holds it, it is destroyed (see ~MpvCore).
    if (mine)
        command({"stop"});
}

QQuickFramebufferObject::Renderer *MpvItem::createRenderer() const {
    keepScene(window());
    return new MpvRenderer(m_link);
}

void MpvItem::keepScene(QQuickWindow *window) const {
    if (!window)
        return;
    window->setPersistentGraphics(!m_releaseWhenHidden);
    window->setPersistentSceneGraph(!m_releaseWhenHidden);
}

void MpvItem::setReleaseWhenHidden(bool release) {
    if (m_releaseWhenHidden == release)
        return;
    m_releaseWhenHidden = release;
    keepScene(window());
    emit releaseWhenHiddenChanged();
}

void MpvItem::itemChange(ItemChange change, const ItemChangeData &data) {
    QQuickFramebufferObject::itemChange(change, data);
    if (change != ItemSceneChange)
        return;
    disconnect(m_hidden);
    QQuickWindow *window = data.window;
    if (!window)
        return;
    keepScene(window);
    // Hidden, the window draws nothing, so its renderer would hold on to
    // every render context it made (and their players) until it is shown
    // again: a handover it was in would never get its context freed. Let go
    // of the scene instead (the threaded render loop does so by itself for a
    // scene that isn't persistent; this asks every loop to). The scene graph
    // deletes the renderer on its render thread with the GL context current,
    // and the renderer frees them there (see ~MpvRenderer).
    m_hidden = connect(window, &QWindow::visibleChanged, this, [this, window](bool visible) {
        if (!visible && m_releaseWhenHidden)
            window->releaseResources();
    });
}

void MpvItem::ensureCore() {
    if (m_core)
        return;
    m_core = std::make_shared<MpvCore>();
    {
        QMutexLocker lock(&m_core->mutex);
        m_core->item = this;
    }
    // mpv calls its wakeup once, when events arrive with none waiting, and not
    // again until they have been read. What came in before the item was set
    // (the first values of what is observed) found nobody to post to, so
    // without this the player would be deaf: a pause toggled soon after would
    // never be heard of.
    QMetaObject::invokeMethod(this, "handleEvents", Qt::QueuedConnection);
    // A new player is as this item was told to be.
    double volume = m_volume, speed = m_speed;
    int mute = m_muted;
    mpv_set_property(m_core->handle, "volume", MPV_FORMAT_DOUBLE, &volume);
    mpv_set_property(m_core->handle, "speed", MPV_FORMAT_DOUBLE, &speed);
    mpv_set_property(m_core->handle, "mute", MPV_FORMAT_FLAG, &mute);
    mpv_set_property_string(m_core->handle, "background-color", m_background.name(QColor::HexArgb).toUtf8().constData());
    update();
}

void MpvItem::retire(const std::shared_ptr<MpvCore> &core) {
    std::shared_ptr<MpvLink> drawer;
    {
        QMutexLocker lock(&core->mutex);
        core->item = nullptr;
        drawer = core->drawer;
    }
    const char *stop[] = {"stop", nullptr};
    mpv_command_async(core->handle, 0, stop);
    // Its renderer frees what it drew it with.
    if (drawer)
        drawer->post("update");
}

void MpvItem::command(const QList<QByteArray> &args) {
    if (!m_core)
        return;
    QList<const char *> argv;
    for (const QByteArray &a : args)
        argv.append(a.constData());
    argv.append(nullptr);
    mpv_command_async(m_core->handle, 0, argv.data());
}

void MpvItem::load(const QString &video, const QString &audio, double start) {
    ensureCore();
    m_video = video;
    m_audio = audio;
    m_start = start;
    ++m_asked;

    m_loading = true;
    m_loaded = false;
    m_ended = false;
    setStarted(false);
    m_position = m_duration = m_cached = 0;
    emit bufferingChanged();
    emit loadedChanged();
    emit endedChanged();
    emit positionChanged();
    emit durationChanged();
    emit cachedChanged();

    // mpv sets its video output up when a file is loaded, and gives the
    // picture up for that file if there is nothing to draw it with yet: the
    // sound plays over a black frame. The render context is made by the
    // scene graph the first time the item is drawn, which for a window that
    // is only now being shown (the small window) is after this call. So the
    // file waits for it. Where nothing will ever draw (the software scene
    // graph of the tests), or if it doesn't come, the file is loaded anyway.
    if (m_core->ready || QQuickWindow::graphicsApi() == QSGRendererInterface::Software) {
        startLoad();
        return;
    }
    m_waiting = true;
    update();
    QTimer::singleShot(3000, this, [this, asked = m_asked] {
        if (m_waiting && asked == m_asked)
            startLoad();
    });
}

void MpvItem::renderReady() {
    if (!m_core)
        return;
    {
        QMutexLocker lock(&m_core->mutex);
        if (!m_core->render || m_core->drawer != m_link)
            return;
    }
    if (m_waiting)
        startLoad();
}

void MpvItem::renderFreed() {
    setHandingOver(false);
    update();
}

void MpvItem::startLoad() {
    ensureCore();
    m_waiting = false;
    // Set as a node list: an audio URL contains ':', which the plain string
    // form would split as a path list.
    QByteArray audioUtf8 = m_audio.toUtf8();
    mpv_node item{};
    item.format = MPV_FORMAT_STRING;
    item.u.string = audioUtf8.data();
    mpv_node_list list{m_audio.isEmpty() ? 0 : 1, &item, nullptr};
    mpv_node files{};
    files.format = MPV_FORMAT_NODE_ARRAY;
    files.u.list = &list;
    mpv_set_property(m_core->handle, "audio-files", MPV_FORMAT_NODE, &files);
    mpv_set_property_string(m_core->handle, "start", m_start > 0 ? QByteArray::number(m_start).constData() : "none");
    mpv_set_property_string(m_core->handle, "pause", "no");
    command({"loadfile", m_video.toUtf8(), "replace"});
}

void MpvItem::stop() {
    // What was waiting to be loaded is no longer wanted.
    m_waiting = false;
    m_loading = false;
    setStarted(false);
    emit bufferingChanged();
    command({"stop"});
}

// The handover. One player has one render context, made in one window's GL
// context, and freeing it while the video plays makes mpv drop the video
// track; choosing it again reads the stream anew. Switching the video
// output off first ("vo" to null) and on again once the new window has its
// context lets mpv set its video up again from what it has cached, with no
// request, and the sound plays on throughout. So:
//   1. here, the player is this item's at once (events, position, pause…),
//      and its video output is switched off;
//   2. once it is off, the old window frees the render context on its own
//      render thread (ReleaseJob), drawing the last picture until then;
//   3. this item's renderer makes a new one, and the output is switched on.
// If the old window never frees it (it isn't showing), the player is let go
// of and takeOverFailed() says to open the video again.
bool MpvItem::takeOver(MpvItem *from) {
    if (!from || from == this)
        return false;
    // What it has heard so far is what is handed over.
    from->handleEvents();
    std::shared_ptr<MpvCore> core = std::move(from->m_core);
    if (!core)
        return false;
    if (m_core)
        retire(m_core);
    m_core = core;
    const quint64 handover = ++core->handover;
    if (timingWanted) {
        core->handoverAt = now();
        core->handoverShowing = false;
        core->handoverLog(QQuickWindow::graphicsApi() == QSGRendererInterface::Software ? "asked (nothing draws)" : "asked");
    }
    bool drawn;
    std::shared_ptr<MpvLink> drawer;
    {
        QMutexLocker lock(&core->mutex);
        core->item = this;
        drawn = core->render != nullptr;
        drawer = core->drawer;
    }
    takeState(from);
    from->clearState();
    if (drawn) {
        learnHardware(*core);
        core->videoOff = true;
        switchVideo(*core, false, voTag(handover, false));
        m_handoverTimeout.start();
        if (drawer)
            if (MpvItem *was = drawer->item)
                was->setHandingOver(true);
    }
    if (m_waiting)
        QTimer::singleShot(3000, this, [this, asked = m_asked] {
            if (m_waiting && asked == m_asked)
                startLoad();
        });
    QMetaObject::invokeMethod(this, "handleEvents", Qt::QueuedConnection);
    update();
    from->update();
    return true;
}

void MpvItem::releaseLost(qulonglong handover) {
    if (!m_core || handover != m_core->handover)
        return;
    QMutexLocker lock(&m_core->mutex);
    const bool stuck = m_core->render && m_core->drawer != m_link;
    lock.unlock();
    if (stuck)
        abandon();
}

void MpvItem::abandon() {
    m_handoverTimeout.stop();
    if (m_core)
        retire(m_core);
    m_core.reset();
    clearState();
    ensureCore();
    emit takeOverFailed();
}

void MpvItem::takeState(MpvItem *from) {
    auto take = [this](auto &mine, auto theirs, void (MpvItem::*signal)()) {
        if (mine != theirs) {
            mine = theirs;
            emit(this->*signal)();
        }
    };
    const bool wasBuffering = buffering();
    m_waiting = from->m_waiting;
    m_asked = qMax(m_asked, from->m_asked) + 1;
    m_video = from->m_video;
    m_audio = from->m_audio;
    m_start = from->m_start;
    m_cachePaused = from->m_cachePaused;
    m_seeking = from->m_seeking;
    m_loading = from->m_loading;
    setStarted(from->m_started);
    take(m_position, from->m_position, &MpvItem::positionChanged);
    take(m_duration, from->m_duration, &MpvItem::durationChanged);
    take(m_cached, from->m_cached, &MpvItem::cachedChanged);
    take(m_paused, from->m_paused, &MpvItem::pausedChanged);
    take(m_loaded, from->m_loaded, &MpvItem::loadedChanged);
    take(m_ended, from->m_ended, &MpvItem::endedChanged);
    take(m_volume, from->m_volume, &MpvItem::volumeChanged);
    take(m_muted, from->m_muted, &MpvItem::mutedChanged);
    take(m_speed, from->m_speed, &MpvItem::speedChanged);
    if (buffering() != wasBuffering)
        emit bufferingChanged();
}

// As an item without a player is: nothing loaded. What it was told of the
// sound and the speed stays, for the next player it has.
void MpvItem::clearState() {
    const bool wasBuffering = buffering();
    m_waiting = false;
    m_cachePaused = m_seeking = m_loading = false;
    setStarted(false);
    auto clear = [this](auto &field, auto value, void (MpvItem::*signal)()) {
        if (field != value) {
            field = value;
            emit(this->*signal)();
        }
    };
    clear(m_position, 0.0, &MpvItem::positionChanged);
    clear(m_duration, 0.0, &MpvItem::durationChanged);
    clear(m_cached, 0.0, &MpvItem::cachedChanged);
    clear(m_paused, false, &MpvItem::pausedChanged);
    clear(m_loaded, false, &MpvItem::loadedChanged);
    clear(m_ended, false, &MpvItem::endedChanged);
    if (buffering() != wasBuffering)
        emit bufferingChanged();
}

void MpvItem::setHandingOver(bool handing) {
    if (handing)
        m_handingOverTimeout.start();
    else
        m_handingOverTimeout.stop();
    if (m_handingOver == handing)
        return;
    m_handingOver = handing;
    emit handingOverChanged();
}

void MpvItem::setStarted(bool started) {
    if (m_started == started)
        return;
    m_started = started;
    emit startedChanged();
}

void MpvItem::togglePause() {
    if (m_ended) {
        command({"seek", "0", "absolute"});
        if (m_core)
            mpv_set_property_string(m_core->handle, "pause", "no");
        return;
    }
    command({"cycle", "pause"});
}

void MpvItem::seek(double seconds, bool relative) {
    command({"seek", QByteArray::number(seconds), relative ? "relative" : "absolute"});
}

void MpvItem::setVolume(double volume) {
    ensureCore();
    volume = qBound(0.0, volume, 100.0);
    mpv_set_property(m_core->handle, "volume", MPV_FORMAT_DOUBLE, &volume);
}

void MpvItem::setRenderScale(double scale) {
    if (!(scale > 0) || qFuzzyCompare(scale, m_renderScale))
        return;
    m_renderScale = scale;
    emit renderScaleChanged();
    update();
}

void MpvItem::setSpeed(double speed) {
    ensureCore();
    speed = qBound(0.25, speed, 3.0);
    mpv_set_property(m_core->handle, "speed", MPV_FORMAT_DOUBLE, &speed);
}

void MpvItem::setMuted(bool muted) {
    ensureCore();
    int flag = muted;
    mpv_set_property(m_core->handle, "mute", MPV_FORMAT_FLAG, &flag);
}

void MpvItem::setBackgroundColor(const QColor &color) {
    if (!color.isValid() || color == m_background)
        return;
    m_background = color;
    // mpv reads #AARRGGBB, which is what Qt calls HexArgb.
    if (m_core)
        mpv_set_property_string(m_core->handle, "background-color", color.name(QColor::HexArgb).toUtf8().constData());
    emit backgroundColorChanged();
}

void MpvItem::onProperty(const mpv_event_property *p) {
    const QByteArray name(p->name);
    const bool has = p->data != nullptr;
    const double number = has && p->format == MPV_FORMAT_DOUBLE ? *static_cast<double *>(p->data) : 0;
    const bool flag = has && p->format == MPV_FORMAT_FLAG && *static_cast<int *>(p->data);

    auto update = [](auto &field, auto value, auto signal, MpvItem *self) {
        if (field != value) {
            field = value;
            emit(self->*signal)();
        }
    };
    if (name == "time-pos")
        update(m_position, number, &MpvItem::positionChanged, this);
    else if (name == "duration")
        update(m_duration, number, &MpvItem::durationChanged, this);
    else if (name == "demuxer-cache-time")
        update(m_cached, number, &MpvItem::cachedChanged, this);
    else if (name == "pause")
        update(m_paused, flag, &MpvItem::pausedChanged, this);
    else if (name == "paused-for-cache")
        update(m_cachePaused, flag, &MpvItem::bufferingChanged, this);
    else if (name == "seeking")
        update(m_seeking, flag, &MpvItem::bufferingChanged, this);
    else if (name == "eof-reached")
        update(m_ended, flag, &MpvItem::endedChanged, this);
    else if (name == "volume" && has)
        update(m_volume, number, &MpvItem::volumeChanged, this);
    else if (name == "mute")
        update(m_muted, flag, &MpvItem::mutedChanged, this);
    else if (name == "speed" && has)
        update(m_speed, number, &MpvItem::speedChanged, this);
    else if (name == "hwdec-current" && m_core) {
        const QByteArray current = has && p->format == MPV_FORMAT_STRING ? QByteArray(*static_cast<char **>(p->data)) : QByteArray();
        QMutexLocker lock(&m_core->mutex);
        m_core->hwdecCurrent = current;
    }
}

void MpvItem::handleEvents() {
    while (m_core) {
        mpv_event *event = mpv_wait_event(m_core->handle, 0);
        if (event->event_id == MPV_EVENT_NONE)
            break;
        switch (event->event_id) {
        case MPV_EVENT_PROPERTY_CHANGE:
            onProperty(static_cast<mpv_event_property *>(event->data));
            break;
        case MPV_EVENT_PLAYBACK_RESTART:
            // The first one after loading means the first frame is up; the
            // ones after seeks change nothing.
            setStarted(true);
            break;
        case MPV_EVENT_FILE_LOADED:
            m_loading = false;
            m_loaded = true;
            emit bufferingChanged();
            emit loadedChanged();
            break;
        case MPV_EVENT_END_FILE: {
            const auto *end = static_cast<mpv_event_end_file *>(event->data);
            if (end->reason == MPV_END_FILE_REASON_ERROR) {
                m_loading = false;
                emit bufferingChanged();
                emit failed(tr("Playback failed: %1").arg(QString::fromUtf8(mpv_error_string(end->error))));
            }
            break;
        }
        case MPV_EVENT_SET_PROPERTY_REPLY: {
            // The video output, switched for a handover (see takeOver).
            const quint64 tag = event->reply_userdata;
            if (!tag || tag >> 1 != m_core->handover)
                break;
            if (tag & 1) {
                m_core->handoverLog("video output on");
                m_core->handoverShowing = true;
                update();
                if (event->error < 0)
                    qWarning("libmpv: couldn't switch the video back on: %s", mpv_error_string(event->error));
                m_handoverTimeout.stop();
                break;
            }
            if (event->error < 0) {
                abandon();
                return;
            }
            m_core->handoverLog("video output off");
            QQuickWindow *window;
            const void *owner;
            bool drawn;
            {
                QMutexLocker lock(&m_core->mutex);
                window = m_core->window;
                owner = m_core->owner;
                drawn = m_core->render != nullptr;
            }
            if (!drawn) {
                update();
            } else if (owner && window && window->isExposed()) {
                // As the window synchronizes, when its render thread is
                // sure to have the GL context current; the update makes it.
                window->scheduleRenderJob(new ReleaseJob(m_core, m_core->handover, owner),
                                          QQuickWindow::BeforeSynchronizingStage);
                window->update();
            } else {
                // A window that isn't showing draws nothing, so it would
                // never get to it.
                abandon();
                return;
            }
            break;
        }
        default:
            break;
        }
    }
}
