// Browser (WebAssembly) services: HTTP through Qt Network (fetch),
// persistent storage (IndexedDB) and the browser's file picker.
#pragma once

#include <QByteArray>
#include <QString>
#include <functional>

#include "core.h"

namespace web {

// Directory backed by IndexedDB where the core keeps its data.
QString dataDir();
// Mounts and loads the persistent storage, then calls `ready`.
void initStorage(std::function<void()> ready);
// Writes pending changes back to IndexedDB.
void persistStorage();
// BlCallbacks::http_request implementation.
void httpRequest(void *user, uint64_t id, const char *method, const char *url, const char *headers, const uint8_t *body, size_t len);
// Opens the browser's file picker; `done` is called once per chosen file.
void pickFiles(const QString &accept, bool multiple, std::function<void(const QString &, const QByteArray &)> done);

} // namespace web
