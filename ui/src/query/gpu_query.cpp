module;
#include "waywallen/query/gpu_query.moc.h"
#undef assert
#include <rstd/macro.hpp>

module waywallen;
import :query.gpu;
import :app;
import :gpu;

using namespace Qt::Literals::StringLiterals;
using namespace qextra::prelude;

namespace proto = waywallen::control::v1;

namespace waywallen
{

GpuListQuery::GpuListQuery(QObject* parent): Query(parent) {}

auto GpuListQuery::gpus() const -> const QVariantList& { return m_gpus; }

void GpuListQuery::reload() {
    setStatus(Status::Querying);
    auto backend = App::instance()->backend();

    auto req = proto::Request {};
    req.setGpuList(proto::GpuListRequest {});

    auto self = QWatcher { this };
    spawn([self, backend, req = std::move(req)]() mutable -> task<void> {
        auto result = co_await backend->send(std::move(req));
        co_await asio::post(asio::bind_executor(self->get_executor(), use_task));
        if (! self) co_return;

        self->inspect_set(result, [self](const proto::Response& rsp) {
            auto& list_rsp = rsp.gpuList();

            if (auto* gm = GpuManager::instance()) {
                gm->replaceAll(list_rsp.gpus());
            }

            QVariantList items;
            for (const auto& g : list_rsp.gpus()) {
                QVariantMap m;
                m[u"renderNode"_s]   = g.renderNode();
                m[u"primaryNode"_s]  = g.primaryNode();
                m[u"renderMajor"_s]  = g.renderMajor();
                m[u"renderMinor"_s]  = g.renderMinor();
                m[u"primaryMajor"_s] = g.primaryMajor();
                m[u"primaryMinor"_s] = g.primaryMinor();
                m[u"pciBdf"_s]       = g.pciBdf();
                m[u"vendorId"_s]     = g.vendorId();
                m[u"deviceId"_s]     = g.deviceId();
                m[u"driver"_s]       = g.driver();
                m[u"description"_s]  = g.description();
                items.append(m);
            }
            self->m_gpus = std::move(items);
            Q_EMIT self->gpusChanged();
        });
        co_return;
    });
}

} // namespace waywallen

#include "waywallen/query/gpu_query.moc.cpp"
