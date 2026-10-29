#include "web.h"
#include "bridge.h"

#if defined(Q_OS_WASM) || defined(__EMSCRIPTEN__)

#include <QJsonArray>
#include <QJsonDocument>
#include <QNetworkAccessManager>
#include <QNetworkReply>
#include <QNetworkRequest>
#include <QTimer>
#include <QUrl>
#include <cstdlib>
#include <emscripten.h>

namespace {

std::function<void()> g_ready;
std::function<void(const QString &, const QByteArray &)> g_picked;
std::function<void(const QString &)> g_auth;

QNetworkAccessManager *nam()
{
    static QNetworkAccessManager m;
    return &m;
}

EM_JS(char *, bl_js_origin, (), {
    return stringToNewUTF8(location.origin);
});

} // namespace

extern "C" {

EMSCRIPTEN_KEEPALIVE void bl_web_storage_ready()
{
    if (g_ready) {
        auto f = std::move(g_ready);
        g_ready = nullptr;
        QTimer::singleShot(0, f);
    }
}

EMSCRIPTEN_KEEPALIVE void bl_web_auth_result(const char *query)
{
    if (g_auth)
        g_auth(QString::fromUtf8(query));
}

EMSCRIPTEN_KEEPALIVE void bl_web_file_picked(const char *name, const uint8_t *data, int len)
{
    if (g_picked)
        g_picked(QString::fromUtf8(name), QByteArray(reinterpret_cast<const char *>(data), len));
}

} // extern "C"

namespace web {

QString dataDir()
{
    return QStringLiteral("/beatlumen");
}

void initStorage(std::function<void()> ready)
{
    g_ready = std::move(ready);
    EM_ASM({
        try { FS.mkdir('/beatlumen'); } catch (e) {}
        FS.mount(IDBFS, {}, '/beatlumen');
        FS.syncfs(true, function (err) {
            if (err) console.warn('Beatlumen: storage', err);
            Module._bl_web_storage_ready();
        });
    });
}

void persistStorage()
{
    EM_ASM({
        if (Module.blSyncing) { Module.blSyncAgain = true; return; }
        Module.blSyncing = true;
        const run = function () {
            FS.syncfs(false, function (err) {
                if (err) console.warn('Beatlumen: storage', err);
                if (Module.blSyncAgain) { Module.blSyncAgain = false; run(); } else { Module.blSyncing = false; }
            });
        };
        run();
    });
}

void httpRequest(void *user, uint64_t id, const char *method, const char *url, const char *headers, const uint8_t *body, size_t len)
{
    Bridge *bridge = static_cast<Bridge *>(user);
    QString u = QString::fromUtf8(url);
    if (u.startsWith(QLatin1Char('/'))) {
        // Same origin as the page (the server that delivered the app).
        char *origin = bl_js_origin();
        u = QString::fromUtf8(origin) + u;
        free(origin);
    }
    QNetworkRequest req{QUrl(u)};
    const QJsonArray hs = QJsonDocument::fromJson(QByteArray(headers)).array();
    for (const QJsonValue &h : hs) {
        const QJsonArray pair = h.toArray();
        req.setRawHeader(pair.at(0).toString().toUtf8(), pair.at(1).toString().toUtf8());
    }
    const QByteArray data(reinterpret_cast<const char *>(body), qsizetype(len));
    QNetworkReply *reply = nam()->sendCustomRequest(req, QByteArray(method), data);
    QObject::connect(reply, &QNetworkReply::finished, bridge, [bridge, reply, id]() {
        reply->deleteLater();
        if (!bridge->core())
            return;
        const int status = reply->attribute(QNetworkRequest::HttpStatusCodeAttribute).toInt();
        const QByteArray content = reply->readAll();
        QJsonArray hdrs;
        for (const auto &p : reply->rawHeaderPairs())
            hdrs.append(QJsonArray{QString::fromUtf8(p.first), QString::fromUtf8(p.second)});
        const QByteArray hjson = QJsonDocument(hdrs).toJson(QJsonDocument::Compact);
        const QByteArray err = reply->errorString().toUtf8();
        bl_core_http_complete(bridge->core(), id, status, hjson.constData(), reinterpret_cast<const uint8_t *>(content.constData()), size_t(content.size()), err.constData());
    });
}

void pickFiles(const QString &accept, bool multiple, std::function<void(const QString &, const QByteArray &)> done)
{
    g_picked = std::move(done);
    const QByteArray a = accept.toUtf8();
    EM_ASM({
        const input = document.createElement('input');
        input.type = 'file';
        input.accept = UTF8ToString($0);
        input.multiple = !!$1;
        input.onchange = async function () {
            for (const f of input.files) {
                const buf = new Uint8Array(await f.arrayBuffer());
                const name = stringToNewUTF8(f.name);
                const ptr = _malloc(buf.length || 1);
                HEAPU8.set(buf, ptr);
                Module._bl_web_file_picked(name, ptr, buf.length);
                _free(ptr);
                _free(name);
            }
        };
        input.click();
    }, a.constData(), multiple ? 1 : 0);
}

void openUrl(const QString &url)
{
    const QByteArray u = url.toUtf8();
    EM_ASM({ window.open(UTF8ToString($0), '_blank'); }, u.constData());
}

void listenAuth(std::function<void(const QString &)> done)
{
    g_auth = std::move(done);
    // The sign-in page (another tab of this server) sends its sealed result
    // over a same-origin channel.
    EM_ASM({
        const ch = new BroadcastChannel('beatlumen-auth');
        ch.onmessage = function (e) {
            const d = e.data || {};
            if (typeof d.state !== 'string' || typeof d.result !== 'string')
                return;
            const q = stringToNewUTF8('?state=' + encodeURIComponent(d.state) + '&result=' + encodeURIComponent(d.result));
            Module._bl_web_auth_result(q);
            _free(q);
        };
    });
}

} // namespace web

#else

namespace web {
QString dataDir() { return {}; }
void initStorage(std::function<void()> ready) { ready(); }
void persistStorage() {}
void httpRequest(void *, uint64_t, const char *, const char *, const char *, const uint8_t *, size_t) {}
void pickFiles(const QString &, bool, std::function<void(const QString &, const QByteArray &)>) {}
void openUrl(const QString &) {}
void listenAuth(std::function<void(const QString &)>) {}
} // namespace web

#endif
