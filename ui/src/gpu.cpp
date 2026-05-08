module;
#include "waywallen/gpu.moc.h"

#undef assert
#include <rstd/macro.hpp>

module waywallen;
import :gpu;

using namespace Qt::Literals::StringLiterals;
using namespace qextra::prelude;
using namespace rstd::prelude;

namespace proto = waywallen::control::v1;

namespace waywallen
{

Gpu::Gpu(const proto::GpuInfo& info, QObject* parent)
    : QObject(parent),
      m_render_node(info.renderNode()),
      m_primary_node(info.primaryNode()),
      m_render_major(info.renderMajor()),
      m_render_minor(info.renderMinor()),
      m_primary_major(info.primaryMajor()),
      m_primary_minor(info.primaryMinor()),
      m_pci_bdf(info.pciBdf()),
      m_vendor_id(info.vendorId()),
      m_device_id(info.deviceId()),
      m_driver(info.driver()),
      m_description(info.description()) {}

static auto gm_instance(GpuManager* in = nullptr) -> GpuManager* {
    static GpuManager* instance { in };
    if (in && instance != in) instance = in;
    return instance;
}

GpuManager::GpuManager(QObject* parent): QObject(parent) { gm_instance(this); }

GpuManager::~GpuManager() {}

auto GpuManager::instance() -> GpuManager* { return gm_instance(); }

auto GpuManager::gpus() const -> QVariantList {
    QVariantList out;
    out.reserve(m_ordered.size());
    for (auto* g : m_ordered) out.append(QVariant::fromValue(g));
    return out;
}

auto GpuManager::find(quint32 major, quint32 minor) const -> Gpu* {
    if (major == 0) return nullptr;
    for (auto* g : m_ordered) {
        if (g->renderMajor() == major && g->renderMinor() == minor) return g;
        if (g->primaryMajor() == major && g->primaryMinor() == minor) return g;
    }
    return nullptr;
}

void GpuManager::replaceAll(const QList<proto::GpuInfo>& list) {
    for (auto* g : m_ordered) g->deleteLater();
    m_ordered.clear();
    m_ordered.reserve(list.size());
    for (const auto& info : list) {
        m_ordered.append(new Gpu(info, this));
    }
    Q_EMIT gpusChanged();
}

} // namespace waywallen

#include "waywallen/gpu.moc.cpp"
