pragma ValueTypeBehavior: Assertable
import QtQuick
import QtQuick.Layouts
import QtQuick.Templates as T
import Qcm.Material as MD
import waywallen.ui as W
import "../component/settings"

MD.Page {
    id: root
    padding: 0
    showHeader: true
    showBackground: false
    title: 'Status'

    component SectionTitle: MD.Text {
        typescale: MD.Token.typescale.title_medium
        color: MD.Token.color.on_surface
    }

    component SectionHint: MD.Text {
        Layout.fillWidth: true
        typescale: MD.Token.typescale.body_medium
        color: MD.Token.color.on_surface_variant
        wrapMode: Text.WordWrap
    }

    component SectionPane: MD.Pane {
        Layout.fillWidth: true
        radius: 16
        padding: 16
        backgroundColor: MD.MProp.color.surface
    }

    W.HealthQuery {
        id: healthQuery
    }

    W.RendererListQuery {
        id: rendererQuery
    }

    W.RendererPluginListQuery {
        id: pluginQuery
    }

    W.SettingsGetQuery {
        id: settingsQuery
    }

    // Queries fan out only after the daemon is Ready (avoid hitting
    // a half-booted daemon at UI startup). `daemonReady` is edge-
    // triggered, so pages constructed AFTER ready also need the level
    // check in `Component.onCompleted`.
    Connections {
        target: W.Notify
        function onDaemonReady() {
            root.reloadAll();
        }
        function onSettingsChanged() {
            settingsQuery.reload();
        }
    }

    Component.onCompleted: {
        if (W.Notify.daemonPhase === W.Notify.DaemonPhase.Ready)
            reloadAll();
    }

    Connections {
        target: settingsQuery
        function onPluginsChanged() {
            if (pluginSettingsPopup.opened) {
                const p = settingsQuery.plugins[pluginSettingsPopup.pluginName];
                pluginSettingsPopup.syncCurrent(p);
            }
        }
    }

    PluginSettingsPopup {
        id: pluginSettingsPopup
        pluginName: ""
        schemaList: []
    }

    W.SourceListQuery {
        id: sourceQuery
    }

    function reloadAll() {
        healthQuery.reload();
        rendererQuery.reload();
        pluginQuery.reload();
        sourceQuery.reload();
        settingsQuery.reload();
    }

    function rendererLabel(d) {
        const name = (d && d.name && d.name.length) ? d.name : "renderer";
        const pid = (d && d.pid) ? d.pid : 0;
        return name + "-" + pid;
    }

    W.RendererKillQuery {
        id: killQuery
        onStatusChanged: {
            if (status === 3) {
                rendererQuery.reload();
                healthQuery.reload();
            }
        }
    }

    MD.Dialog {
        id: killDialog
        property string rendererId: ""
        property string label: ""
        title: "Kill renderer?"
        parent: T.Overlay.overlay
        standardButtons: T.Dialog.Cancel | T.Dialog.Ok

        contentItem: MD.Text {
            text: "Stop the renderer process\n\"" + killDialog.label + "\"?\nUnsaved frame state may be lost."
            typescale: MD.Token.typescale.body_medium
            color: MD.Token.color.on_surface_variant
            wrapMode: Text.WordWrap
        }

        onAccepted: {
            killQuery.rendererId = killDialog.rendererId;
            killQuery.reload();
        }
    }

    contentItem: MD.VerticalFlickable {
        id: m_flick
        topMargin: 12
        leftMargin: 12
        rightMargin: 12
        bottomMargin: 12

        ColumnLayout {
            width: m_flick.contentWidth
            spacing: 12

            // --- Daemon ---
            SectionPane {
                contentItem: ColumnLayout {
                    spacing: 8

                    SectionTitle {
                        text: "Daemon"
                    }

                    RowLayout {
                        spacing: 8
                        MD.Text {
                            text: "Service:"
                            typescale: MD.Token.typescale.label_medium
                            color: MD.Token.color.on_surface_variant
                        }
                        MD.Text {
                            text: healthQuery.service || "—"
                            typescale: MD.Token.typescale.body_medium
                            color: MD.Token.color.on_surface
                        }
                    }

                    RowLayout {
                        spacing: 8
                        MD.Text {
                            text: "State:"
                            typescale: MD.Token.typescale.label_medium
                            color: MD.Token.color.on_surface_variant
                        }

                        Rectangle {
                            Layout.preferredWidth: 8
                            Layout.preferredHeight: 8
                            radius: 4
                            color: healthQuery.state === "healthy" ? MD.Token.color.primary : MD.Token.color.error
                        }

                        MD.Text {
                            text: healthQuery.state || "unknown"
                            typescale: MD.Token.typescale.body_medium
                            color: MD.Token.color.on_surface
                        }
                    }
                }
            }

            // --- Active Renderers ---
            SectionPane {
                contentItem: ColumnLayout {
                    spacing: 8

                    SectionTitle {
                        text: "Active Renderers"
                    }

                    SectionHint {
                        readonly property var liveRenderers: W.App.rendererManager.renderers
                        visible: !liveRenderers || liveRenderers.length === 0
                        text: "No active renderers"
                    }

                    ListView {
                        Layout.fillWidth: true
                        Layout.preferredHeight: contentHeight
                        implicitHeight: contentHeight
                        interactive: false
                        spacing: 4

                        // Live, push-updated. Backend events (RendererSnapshot /
                        // RendererChanged / RendererRemoved) flow through
                        // RendererManager so a child process exiting drops out of
                        // this list without needing a manual refresh.
                        model: W.App.rendererManager.renderers

                        delegate: MD.ListItem {
                            required property var modelData

                            width: ListView.view.width
                            radius: 12
                            text: root.rendererLabel(modelData)
                            font.family: "monospace"
                            supportText: (modelData.status || "") + " · " + (modelData.fps || 0) + " fps"
                                + (modelData.textureWidth ? " · " + modelData.textureWidth + "×" + modelData.textureHeight : "")
                            leader: MD.Icon {
                                name: modelData.status === "paused" ? MD.Token.icon.pause : MD.Token.icon.play_arrow
                                size: 24
                                color: modelData.status === "paused" ? MD.Token.color.on_surface_variant : MD.Token.color.primary
                            }
                            trailing: RowLayout {
                                spacing: 6
                                W.GpuTag {
                                    Layout.alignment: Qt.AlignVCenter
                                    drmRenderMajor: modelData.drmRenderMajor || 0
                                    drmRenderMinor: modelData.drmRenderMinor || 0
                                }
                                MD.IconButton {
                                    icon.name: MD.Token.icon.close
                                    onClicked: {
                                        killDialog.rendererId = modelData.id;
                                        killDialog.label = root.rendererLabel(modelData);
                                        killDialog.open();
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // --- Renderer Plugins ---
            SectionPane {
                contentItem: ColumnLayout {
                    spacing: 8

                    SectionTitle {
                        text: "Renderer Plugins"
                    }

                    SectionHint {
                        typescale: MD.Token.typescale.label_medium
                        visible: pluginQuery.supportedTypes && pluginQuery.supportedTypes.length > 0
                        text: "Supported types: " + (pluginQuery.supportedTypes ? pluginQuery.supportedTypes.join(", ") : "")
                    }

                    SectionHint {
                        visible: !pluginQuery.renderers || pluginQuery.renderers.length === 0
                        text: "No renderer plugins"
                    }

                    ListView {
                        Layout.fillWidth: true
                        Layout.preferredHeight: contentHeight
                        implicitHeight: contentHeight
                        interactive: false
                        spacing: 4

                        model: pluginQuery.renderers

                        delegate: MD.ListItem {
                            id: pluginItem
                            required property var modelData

                            readonly property bool hasSettings: (modelData.settings && modelData.settings.length > 0) === true

                            width: ListView.view.width
                            radius: 12
                            text: modelData.name || ""
                            supportText: (modelData.types ? modelData.types.join(", ") : "")
                            leader: MD.Icon {
                                name: MD.Token.icon.extension
                                size: 24
                                color: MD.Token.color.on_surface_variant
                            }
                            trailing: RowLayout {
                                spacing: 4
                                MD.Text {
                                    text: (pluginItem.modelData.version || "v0.0.0")
                                    typescale: MD.Token.typescale.label_small
                                    color: MD.Token.color.on_surface_variant
                                }
                                MD.IconButton {
                                    visible: pluginItem.hasSettings
                                    icon.name: MD.Token.icon.settings
                                    onClicked: {
                                        pluginSettingsPopup.pluginName = pluginItem.modelData.name;
                                        pluginSettingsPopup.schemaList = pluginItem.modelData.settings || [];
                                        pluginSettingsPopup.allCurrentPlugins = settingsQuery.plugins || ({});
                                        pluginSettingsPopup.currentGlobal = settingsQuery.global || ({});
                                        const p = settingsQuery.plugins ? settingsQuery.plugins[pluginItem.modelData.name] : undefined;
                                        pluginSettingsPopup.currentValues = p || ({});
                                        pluginSettingsPopup.pendingValues = ({});
                                        pluginSettingsPopup.open();
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // --- Source Plugins ---
            SectionPane {
                contentItem: ColumnLayout {
                    spacing: 8

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 8
                        SectionTitle {
                            text: "Source Plugins"
                            Layout.fillWidth: true
                        }
                        MD.IconButton {
                            icon.name: MD.Token.icon.hard_drive
                            onClicked: MD.Util.showPopup('waywallen.ui/PagePopup', {
                                source: 'waywallen.ui/SourceManagePage'
                            }, root)
                        }
                    }

                    SectionHint {
                        visible: !sourceQuery.sources || sourceQuery.sources.length === 0
                        text: "No source plugins loaded"
                    }

                    ListView {
                        Layout.fillWidth: true
                        Layout.preferredHeight: contentHeight
                        implicitHeight: contentHeight
                        interactive: false
                        spacing: 4

                        model: sourceQuery.sources

                        delegate: MD.ListItem {
                            required property var modelData

                            width: ListView.view.width
                            radius: 12
                            text: modelData.name || ""
                            supportText: "Types: " + (modelData.types ? modelData.types.join(", ") : "—")
                            leader: MD.Icon {
                                name: MD.Token.icon.source
                                size: 24
                                color: MD.Token.color.on_surface_variant
                            }
                            trailing: MD.Text {
                                text: "v" + (modelData.version || "?")
                                typescale: MD.Token.typescale.label_small
                                color: MD.Token.color.on_surface_variant
                            }
                        }
                    }
                }
            }
        }
    }
}
