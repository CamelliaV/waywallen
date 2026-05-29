module;
#include "waywallen/query/settings_query.moc.h"
#undef assert
#include <rstd/macro.hpp>

module waywallen;
import :query.settings;
import :app;

using namespace Qt::Literals::StringLiterals;
using namespace qextra::prelude;

namespace proto = waywallen::control::v1;

namespace waywallen
{

namespace
{

auto layout_to_map(const proto::LayoutPrefs& l) -> QVariantMap {
    QVariantMap m;
    m[u"fillmode"_s] = static_cast<int>(l.fillmode());
    m[u"align"_s]    = static_cast<int>(l.align());
    return m;
}

auto map_to_layout(const QVariantMap& m) -> proto::LayoutPrefs {
    proto::LayoutPrefs l;
    l.setFillmode(static_cast<proto::FillMode>(m.value(u"fillmode"_s).toInt()));
    l.setAlign(static_cast<proto::Align>(m.value(u"align"_s).toInt()));
    return l;
}

auto autopause_to_map(const proto::AutopauseSettings& a) -> QVariantMap {
    QVariantMap m;
    m[u"mode"_s]              = static_cast<int>(a.mode());
    m[u"resumeMs"_s]          = a.resumeMs();
    m[u"pauseOnLock"_s]       = a.pauseOnLock();
    m[u"pauseOnUserSwitch"_s] = a.pauseOnUserSwitch();
    if (a.hasPauseOnMediaPlaying()) {
        m[u"pauseOnMediaPlaying"_s] = a.pauseOnMediaPlaying();
    }
    return m;
}

auto map_to_autopause(const QVariantMap& m) -> proto::AutopauseSettings {
    proto::AutopauseSettings a;
    a.setMode(static_cast<proto::AutopauseMode>(m.value(u"mode"_s).toInt()));
    a.setResumeMs(m.value(u"resumeMs"_s).toUInt());
    a.setPauseOnLock(m.value(u"pauseOnLock"_s, true).toBool());
    a.setPauseOnUserSwitch(m.value(u"pauseOnUserSwitch"_s, true).toBool());
    if (m.contains(u"pauseOnMediaPlaying"_s)) {
        a.setPauseOnMediaPlaying(m.value(u"pauseOnMediaPlaying"_s, true).toBool());
    }
    return a;
}

auto global_to_map(const proto::GlobalSettings& g) -> QVariantMap {
    QVariantMap m;
    QVariantList wallpaper_filters;
    for (const auto& filter : g.wallpaperFilters()) {
        wallpaper_filters.append(QVariant::fromValue(filter));
    }
    m[u"wallpaperFilters"_s] = wallpaper_filters;
    QVariantList wallpaper_filter_logics;
    for (const auto& logic : g.wallpaperFilterLogics()) {
        wallpaper_filter_logics.append(QVariant::fromValue(logic));
    }
    m[u"wallpaperFilterLogics"_s] = wallpaper_filter_logics;
    QVariantList wallpaper_sorts;
    for (const auto& sort : g.wallpaperSorts()) {
        wallpaper_sorts.append(QVariant::fromValue(sort));
    }
    m[u"wallpaperSorts"_s] = wallpaper_sorts;
    if (g.hasLayoutDefaults()) {
        m[u"layoutDefaults"_s] = layout_to_map(g.layoutDefaults());
    }
    if (g.hasAutopause()) {
        m[u"autopause"_s] = autopause_to_map(g.autopause());
    }
    m[u"queueMode"_s]    = g.queueMode();
    m[u"rotationSecs"_s] = g.rotationSecs();
    if (g.hasSilentStartup()) {
        m[u"silentStartup"_s] = g.silentStartup();
    }
    return m;
}

auto plugins_to_map(const proto::SettingsGetResponse::PluginsEntry& src) -> QVariantMap {
    QVariantMap out;
    for (auto it = src.constBegin(); it != src.constEnd(); ++it) {
        QVariantMap inner;
        const auto& values = it.value().values();
        for (auto vit = values.constBegin(); vit != values.constEnd(); ++vit) {
            inner[vit.key()] = vit.value();
        }
        out[it.key()] = inner;
    }
    return out;
}

auto map_to_global(const QVariantMap& m) -> proto::GlobalSettings {
    proto::GlobalSettings g;
    QList<proto::WallpaperFilterRule> wallpaper_filters;
    for (const auto& value : m.value(u"wallpaperFilters"_s).toList()) {
        wallpaper_filters.append(value.value<proto::WallpaperFilterRule>());
    }
    g.setWallpaperFilters(wallpaper_filters);
    QList<proto::FilterLogic> wallpaper_filter_logics;
    for (const auto& value : m.value(u"wallpaperFilterLogics"_s).toList()) {
        wallpaper_filter_logics.append(value.value<proto::FilterLogic>());
    }
    g.setWallpaperFilterLogics(wallpaper_filter_logics);
    QList<proto::WallpaperSortRule> wallpaper_sorts;
    for (const auto& value : m.value(u"wallpaperSorts"_s).toList()) {
        wallpaper_sorts.append(value.value<proto::WallpaperSortRule>());
    }
    g.setWallpaperSorts(wallpaper_sorts);
    // Round-trip layout_defaults so a single-plugin SettingsSet doesn't
    // wipe the daemon's current LayoutPrefs (fillmode / align /
    // clear_rgba). UI never edits these — it just forwards them.
    if (m.contains(u"layoutDefaults"_s)) {
        g.setLayoutDefaults(map_to_layout(m.value(u"layoutDefaults"_s).toMap()));
    }
    if (m.contains(u"autopause"_s)) {
        g.setAutopause(map_to_autopause(m.value(u"autopause"_s).toMap()));
    }
    if (m.contains(u"queueMode"_s)) {
        g.setQueueMode(m.value(u"queueMode"_s).toString());
    }
    if (m.contains(u"rotationSecs"_s)) {
        g.setRotationSecs(m.value(u"rotationSecs"_s).toUInt());
    }
    if (m.contains(u"silentStartup"_s)) {
        g.setSilentStartup(m.value(u"silentStartup"_s, true).toBool());
    }
    return g;
}

auto map_to_plugins(const QVariantMap& m) -> QHash<QString, proto::PluginSettings> {
    QHash<QString, proto::PluginSettings> out;
    for (auto it = m.constBegin(); it != m.constEnd(); ++it) {
        proto::PluginSettings ps;
        proto::PluginSettings::ValuesEntry values;
        const auto inner = it.value().toMap();
        for (auto vit = inner.constBegin(); vit != inner.constEnd(); ++vit) {
            values.insert(vit.key(), vit.value().toString());
        }
        ps.setValues(values);
        out.insert(it.key(), ps);
    }
    return out;
}

} // namespace

// ---------------------------------------------------------------------------
// SettingsGetQuery
// ---------------------------------------------------------------------------

SettingsGetQuery::SettingsGetQuery(QObject* parent): Query(parent) {}

auto SettingsGetQuery::global() const -> const QVariantMap& { return m_global; }
auto SettingsGetQuery::plugins() const -> const QVariantMap& { return m_plugins; }

void SettingsGetQuery::reload() {
    setStatus(Status::Querying);
    auto backend = App::instance()->backend();

    auto req = proto::Request {};
    req.setSettingsGet(proto::SettingsGetRequest {});

    auto self = QWatcher { this };
    spawn([self, backend, req = std::move(req)]() mutable -> task<void> {
        auto result = co_await backend->send(std::move(req));
        co_await asio::post(asio::bind_executor(self->get_executor(), use_task));
        if (! self) co_return;

        self->inspect_set(result, [self](const proto::Response& rsp) {
            const auto& get_rsp = rsp.settingsGet();
            self->m_global  = global_to_map(get_rsp.global());
            self->m_plugins = plugins_to_map(get_rsp.plugins());
            Q_EMIT self->globalChanged();
            Q_EMIT self->pluginsChanged();
        });
        co_return;
    });
}

// ---------------------------------------------------------------------------
// SettingsSetQuery
// ---------------------------------------------------------------------------

SettingsSetQuery::SettingsSetQuery(QObject* parent): Query(parent) {}

auto SettingsSetQuery::global() const -> const QVariantMap& { return m_global; }
void SettingsSetQuery::setGlobal(const QVariantMap& v) {
    if (m_global != v) {
        m_global = v;
        Q_EMIT globalChanged();
    }
}

auto SettingsSetQuery::plugins() const -> const QVariantMap& { return m_plugins; }
void SettingsSetQuery::setPlugins(const QVariantMap& v) {
    if (m_plugins != v) {
        m_plugins = v;
        Q_EMIT pluginsChanged();
    }
}

void SettingsSetQuery::reload() {
    setStatus(Status::Querying);
    auto backend = App::instance()->backend();

    auto req   = proto::Request {};
    auto inner = proto::SettingsSetRequest {};
    inner.setGlobal(map_to_global(m_global));
    inner.setPlugins(map_to_plugins(m_plugins));
    req.setSettingsSet(std::move(inner));

    auto self = QWatcher { this };
    spawn([self, backend, req = std::move(req)]() mutable -> task<void> {
        auto result = co_await backend->send(std::move(req));
        co_await asio::post(asio::bind_executor(self->get_executor(), use_task));
        if (! self) co_return;

        self->inspect_set(result, [](const proto::Response&) {});
        co_return;
    });
}

} // namespace waywallen

#include "waywallen/query/settings_query.moc.cpp"
