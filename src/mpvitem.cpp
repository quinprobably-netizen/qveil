// The render-API embedding follows mpv-examples/libmpv/qml (public domain).
#include "mpvitem.h"

#include <QGuiApplication>
#include <QOpenGLContext>
#include <QOpenGLFramebufferObject>
#include <QQuickWindow>
#include <QSGRendererInterface>
#include <QTimer>
#include <QtGui/qguiapplication_platform.h>

namespace {

void *procAddress(void *, const char *name) {
    QOpenGLContext *context = QOpenGLContext::currentContext();
    return context ? reinterpret_cast<void *>(context->getProcAddress(QByteArray(name))) : nullptr;
}

// Both callbacks arrive on mpv's threads, and the item can be destroyed at any
// moment, so it is only touched under the lock its destructor takes to clear it.
void post(void *shared, const char *method) {
    auto *s = static_cast<MpvShared *>(shared);
    QMutexLocker lock(&s->mutex);
    if (s->item)
        QMetaObject::invokeMethod(s->item, method, Qt::QueuedConnection);
}

void onRenderUpdate(void *shared) { post(shared, "update"); }
void onWakeup(void *shared) { post(shared, "handleEvents"); }

class MpvRenderer : public QQuickFramebufferObject::Renderer {
public:
    explicit MpvRenderer(std::shared_ptr<MpvShared> shared) : m_shared(std::move(shared)) {}

    // The scene graph deletes the renderer on the render thread with its GL
    // context current, which is exactly what freeing the render context needs.
    // The item can't do it: it is destroyed on the GUI thread, while this
    // thread may be inside mpv_render_context_render.
    ~MpvRenderer() override {
        mpv_render_context *context = m_shared->render;
        if (!context)
            return;
        if (!QOpenGLContext::currentContext()) {
            // Freeing without the context would crash in the driver, and
            // destroying the player with the context alive is undefined, so
            // keep the player for the rest of the process instead.
            qWarning("libmpv: no current OpenGL context to free the render context in");
            new std::shared_ptr<MpvShared>(m_shared);
            return;
        }
        m_shared->render = nullptr;
        m_shared->ready = false;
        // Every mpv_render_* call happens on this thread, so none can overlap
        // these two. After the first, mpv's threads no longer reach the item.
        mpv_render_context_set_update_callback(context, nullptr, nullptr);
        mpv_render_context_free(context);
    }

    // Runs on the render thread while the GUI thread waits; the only moment
    // the item can be read safely.
    void synchronize(QQuickFramebufferObject *item) override {
        m_window = item->window();
        // The scene graph is told not to follow the item's size itself: it
        // would compare the framebuffer with the item's own size and make a
        // new one on every frame while the two differ, which they do whenever
        // the window is drawn larger. It is made again here, when the size it
        // is for changes.
        const double scale = static_cast<MpvItem *>(item)->renderScale();
        const double ratio = m_window ? m_window->effectiveDevicePixelRatio() : 1;
        const QSize size = (item->size() * ratio * scale).toSize();
        if (size != m_size) {
            m_size = size;
            invalidateFramebufferObject();
        }
    }

    QOpenGLFramebufferObject *createFramebufferObject(const QSize &size) override {
        if (!m_shared->render) {
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
            if (mpv_render_context_create(&context, m_shared->handle, params.data()) < 0) {
                qWarning("libmpv: couldn't create the OpenGL render context");
            } else {
                m_shared->render = context;
                m_shared->ready = true;
                mpv_render_context_set_update_callback(context, onRenderUpdate, m_shared.get());
                // A video may be waiting for this (see MpvItem::load).
                post(m_shared.get(), "renderReady");
            }
        }
        return QQuickFramebufferObject::Renderer::createFramebufferObject(m_size.isEmpty() ? size : m_size);
    }

    void render() override {
        mpv_render_context *context = m_shared->render;
        if (!context || !m_window)
            return;
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
    std::shared_ptr<MpvShared> m_shared;
    QQuickWindow *m_window = nullptr;
    QSize m_size;
};

} // namespace

MpvShared::~MpvShared() {
    // The renderer frees the render context before it lets go of this, and an
    // item that was never drawn never made one, so mpv needs no GL context
    // any more. Destroying the core with a live render context is undefined
    // (render.h), and so is doing it before that.
    mpv_terminate_destroy(handle);
}

MpvItem::MpvItem(QQuickItem *parent) : QQuickFramebufferObject(parent), m_mpv(mpv_create()) {
    // The renderer sizes the framebuffer (see MpvRenderer::synchronize).
    setTextureFollowsItemSize(false);
    if (!m_mpv)
        qFatal("libmpv: couldn't create a player");
    m_shared = std::make_shared<MpvShared>(m_mpv);
    m_shared->item = this;

    const std::pair<const char *, const char *> options[] = {
        {"vo", "libmpv"},
        {"hwdec", "auto-safe"},
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
        mpv_set_option_string(m_mpv, name, value);
    if (mpv_initialize(m_mpv) < 0)
        qFatal("libmpv: couldn't initialize the player");

    const std::pair<const char *, mpv_format> observed[] = {
        {"time-pos", MPV_FORMAT_DOUBLE},       {"duration", MPV_FORMAT_DOUBLE},
        {"demuxer-cache-time", MPV_FORMAT_DOUBLE}, {"pause", MPV_FORMAT_FLAG},
        {"paused-for-cache", MPV_FORMAT_FLAG}, {"seeking", MPV_FORMAT_FLAG},
        {"eof-reached", MPV_FORMAT_FLAG},      {"volume", MPV_FORMAT_DOUBLE},
        {"mute", MPV_FORMAT_FLAG},             {"speed", MPV_FORMAT_DOUBLE},
    };
    for (const auto &[name, format] : observed)
        mpv_observe_property(m_mpv, 0, name, format);
    mpv_set_wakeup_callback(m_mpv, onWakeup, m_shared.get());
}

MpvItem::~MpvItem() {
    {
        // From here on, mpv's threads can no longer reach this object.
        QMutexLocker lock(&m_shared->mutex);
        m_shared->item = nullptr;
    }
    mpv_set_wakeup_callback(m_mpv, nullptr, nullptr);
    // The render context belongs to the renderer, which frees it on the render
    // thread, so the player may outlive this item for a frame. It mustn't play
    // on meanwhile; once the renderer and this item are both gone, it is
    // destroyed (see ~MpvShared).
    command({"stop"});
}

QQuickFramebufferObject::Renderer *MpvItem::createRenderer() const {
    window()->setPersistentGraphics(true);
    window()->setPersistentSceneGraph(true);
    return new MpvRenderer(m_shared);
}

void MpvItem::command(const QList<QByteArray> &args) {
    QList<const char *> argv;
    for (const QByteArray &a : args)
        argv.append(a.constData());
    argv.append(nullptr);
    mpv_command_async(m_mpv, 0, argv.data());
}

void MpvItem::load(const QString &video, const QString &audio, double start) {
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
    if (m_shared->ready || QQuickWindow::graphicsApi() == QSGRendererInterface::Software) {
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
    if (m_waiting)
        startLoad();
}

void MpvItem::startLoad() {
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
    mpv_set_property(m_mpv, "audio-files", MPV_FORMAT_NODE, &files);
    mpv_set_property_string(m_mpv, "start", m_start > 0 ? QByteArray::number(m_start).constData() : "none");
    mpv_set_property_string(m_mpv, "pause", "no");
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

void MpvItem::setStarted(bool started) {
    if (m_started == started)
        return;
    m_started = started;
    emit startedChanged();
}

void MpvItem::togglePause() {
    if (m_ended) {
        command({"seek", "0", "absolute"});
        mpv_set_property_string(m_mpv, "pause", "no");
        return;
    }
    command({"cycle", "pause"});
}

void MpvItem::seek(double seconds, bool relative) {
    command({"seek", QByteArray::number(seconds), relative ? "relative" : "absolute"});
}

void MpvItem::setVolume(double volume) {
    volume = qBound(0.0, volume, 100.0);
    mpv_set_property(m_mpv, "volume", MPV_FORMAT_DOUBLE, &volume);
}

void MpvItem::setRenderScale(double scale) {
    if (!(scale > 0) || qFuzzyCompare(scale, m_renderScale))
        return;
    m_renderScale = scale;
    emit renderScaleChanged();
    update();
}

void MpvItem::setSpeed(double speed) {
    speed = qBound(0.25, speed, 3.0);
    mpv_set_property(m_mpv, "speed", MPV_FORMAT_DOUBLE, &speed);
}

void MpvItem::setMuted(bool muted) {
    int flag = muted;
    mpv_set_property(m_mpv, "mute", MPV_FORMAT_FLAG, &flag);
}

void MpvItem::setBackgroundColor(const QColor &color) {
    if (!color.isValid() || color == m_background)
        return;
    m_background = color;
    // mpv reads #AARRGGBB, which is what Qt calls HexArgb.
    mpv_set_property_string(m_mpv, "background-color", color.name(QColor::HexArgb).toUtf8().constData());
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
}

void MpvItem::handleEvents() {
    while (m_mpv) {
        mpv_event *event = mpv_wait_event(m_mpv, 0);
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
        default:
            break;
        }
    }
}
