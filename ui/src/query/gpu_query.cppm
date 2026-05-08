module;
#include "QExtra/macro_qt.hpp"

#ifdef Q_MOC_RUN
#    include "waywallen/query/gpu_query.moc"
#endif

export module waywallen:query.gpu;
export import :query.query;

namespace waywallen
{

/// One-shot fetch of the host's GPU set. Daemon never re-emits, so this
/// is fired once on backend connect (see `App::init`) to populate the
/// global `GpuManager`.
export class GpuListQuery : public Query, public QueryExtra<control::v1::Response, GpuListQuery> {
    Q_OBJECT
    QML_ELEMENT

    Q_PROPERTY(QVariantList gpus READ gpus NOTIFY gpusChanged FINAL)

public:
    GpuListQuery(QObject* parent = nullptr);

    auto gpus() const -> const QVariantList&;

    void reload() override;

    Q_SIGNAL void gpusChanged();

private:
    QVariantList m_gpus;
};

} // namespace waywallen
