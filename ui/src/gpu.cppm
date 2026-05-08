module;
#include "QExtra/macro_qt.hpp"

#ifdef Q_MOC_RUN
#    include "waywallen/gpu.moc"
#endif

export module waywallen:gpu;
export import :proto;
import rstd;
import rstd.cppstd;
import qextra;

using rstd::boxed::Box;

namespace proto = waywallen::control::v1;

export namespace waywallen
{

/// One GPU, mirroring `proto::GpuInfo`. Daemon enumerates DRM devices once
/// at startup and never re-emits, so all properties are CONSTANT.
class Gpu : public QObject {
    Q_OBJECT
    QML_ELEMENT
    QML_UNCREATABLE("Gpu instances are owned by GpuManager")

    Q_PROPERTY(QString renderNode READ renderNode CONSTANT FINAL)
    Q_PROPERTY(QString primaryNode READ primaryNode CONSTANT FINAL)
    Q_PROPERTY(quint32 renderMajor READ renderMajor CONSTANT FINAL)
    Q_PROPERTY(quint32 renderMinor READ renderMinor CONSTANT FINAL)
    Q_PROPERTY(quint32 primaryMajor READ primaryMajor CONSTANT FINAL)
    Q_PROPERTY(quint32 primaryMinor READ primaryMinor CONSTANT FINAL)
    Q_PROPERTY(QString pciBdf READ pciBdf CONSTANT FINAL)
    Q_PROPERTY(quint32 vendorId READ vendorId CONSTANT FINAL)
    Q_PROPERTY(quint32 deviceId READ deviceId CONSTANT FINAL)
    Q_PROPERTY(QString driver READ driver CONSTANT FINAL)
    Q_PROPERTY(QString description READ description CONSTANT FINAL)

public:
    explicit Gpu(const proto::GpuInfo& info, QObject* parent = nullptr);

    auto renderNode() const -> const QString& { return m_render_node; }
    auto primaryNode() const -> const QString& { return m_primary_node; }
    auto renderMajor() const -> quint32 { return m_render_major; }
    auto renderMinor() const -> quint32 { return m_render_minor; }
    auto primaryMajor() const -> quint32 { return m_primary_major; }
    auto primaryMinor() const -> quint32 { return m_primary_minor; }
    auto pciBdf() const -> const QString& { return m_pci_bdf; }
    auto vendorId() const -> quint32 { return m_vendor_id; }
    auto deviceId() const -> quint32 { return m_device_id; }
    auto driver() const -> const QString& { return m_driver; }
    auto description() const -> const QString& { return m_description; }

private:
    QString m_render_node;
    QString m_primary_node;
    quint32 m_render_major;
    quint32 m_render_minor;
    quint32 m_primary_major;
    quint32 m_primary_minor;
    QString m_pci_bdf;
    quint32 m_vendor_id;
    quint32 m_device_id;
    QString m_driver;
    QString m_description;
};

/// Singleton model holding the host's GPU set. Populated once at backend
/// connect via `GpuListQuery::reload` (no push events — daemon enumerates
/// at startup and the set is static for the daemon's lifetime).
///
/// `find(major, minor)` resolves a DRM node id (either the render or primary
/// node) to the owning Gpu*. Renderers carry a render-node id, displays
/// carry the consumer-side render-node id; both look up via the same call.
class GpuManager : public QObject {
    Q_OBJECT
    QML_ELEMENT

    Q_PROPERTY(QVariantList gpus READ gpus NOTIFY gpusChanged FINAL)
    Q_PROPERTY(int count READ count NOTIFY gpusChanged FINAL)

public:
    GpuManager(QObject* parent = nullptr);
    ~GpuManager() override;

    static auto instance() -> GpuManager*;

    auto gpus() const -> QVariantList;
    auto count() const -> int { return (int)m_ordered.size(); }

    /// Resolve a DRM node id (either render or primary) to its Gpu.
    /// Returns nullptr when major == 0 (unknown) or no GPU matches.
    Q_INVOKABLE waywallen::Gpu* find(quint32 major, quint32 minor) const;

    /// Full replace from a fresh GpuListResponse. Emits `gpusChanged` once.
    void replaceAll(const QList<proto::GpuInfo>& list);

    Q_SIGNAL void gpusChanged();

private:
    QList<Gpu*> m_ordered;
};

} // namespace waywallen
