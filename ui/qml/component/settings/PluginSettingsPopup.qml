pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import QtQuick.Templates as T
import Qcm.Material as MD
import waywallen.ui as W

MD.Popup {
    id: root
    parent: T.Overlay.overlay

    required property string pluginName
    required property var schemaList
    property var currentValues: ({})
    // SettingsSet is full-replace, so we forward the rest of the plugin
    // map and the global block verbatim — otherwise editing one plugin
    // would wipe everyone else.
    property var allCurrentPlugins: ({})
    property var currentGlobal: ({})
    property var pendingValues: ({})

    signal applied

    modal: true
    dim: true
    closePolicy: T.Popup.CloseOnEscape | T.Popup.CloseOnPressOutside
    padding: 0
    bottomPadding: 8

    width: Math.min(parent ? parent.width - 64 : 390, 480)
    height: Math.min(parent ? parent.height - 96 : 720, 720)
    x: parent ? Math.round((parent.width - width) / 2) : 0
    y: parent ? Math.round((parent.height - height) / 2) : 0

    background: Rectangle {
        color: MD.Token.color.surface
        radius: MD.Token.shape.corner.large
    }

    W.SettingsSetQuery {
        id: setQuery
        // 2 = QAsyncResult::Status::Finished.
        onStatusChanged: {
            if (status === 2) {
                root.applied();
                root.close();
            }
        }
    }

    function valueFor(key) {
        const pv = root.pendingValues;
        const cv = root.currentValues;
        if (key in pv)
            return pv[key];
        if (key in cv)
            return cv[key];
        for (let i = 0; i < root.schemaList.length; ++i) {
            const s = root.schemaList[i];
            if (s.key === key)
                return s.default_value;
        }
        return "";
    }

    function syncCurrent(values) {
        root.currentValues = values || ({});
    }

    function reset() {
        root.pendingValues = ({});
    }

    function _serialize(map) {
        const keys = Object.keys(map).sort();
        const out = {};
        for (let i = 0; i < keys.length; ++i)
            out[keys[i]] = map[keys[i]];
        return JSON.stringify(out);
    }

    function _baseline() {
        const m = ({});
        for (let i = 0; i < schemaList.length; ++i) {
            const s = schemaList[i];
            m[s.key] = s.default_value;
        }
        for (const k in currentValues)
            m[k] = currentValues[k];
        return m;
    }

    function _merged() {
        const m = _baseline();
        for (const k in pendingValues)
            m[k] = pendingValues[k];
        return m;
    }

    // Compare serialized baseline to merged-with-pending; only enable
    // Apply/Reset when the user has produced a real delta (a no-op edit
    // — type the current value, then back out — leaves us clean).
    readonly property bool isDirty: _serialize(_baseline()) !== _serialize(_merged())

    function apply() {
        const plugins = Object.assign({}, root.allCurrentPlugins);
        plugins[root.pluginName] = _merged();
        setQuery.global = root.currentGlobal;
        setQuery.plugins = plugins;
        setQuery.reload();
    }

    onClosed: {
        pendingValues = ({});
        setQuery.setError("");
    }

    readonly property var flatSchemas: {
        const buckets = {};
        for (let i = 0; i < schemaList.length; ++i) {
            const s = schemaList[i];
            const g = (s.group && s.group.length > 0) ? s.group : "General";
            if (!buckets[g])
                buckets[g] = [];
            buckets[g].push(s);
        }
        const keys = Object.keys(buckets).sort();
        const out = [];
        for (let i = 0; i < keys.length; ++i) {
            const k = keys[i];
            const items = buckets[k];
            items.sort(function (a, b) {
                return (a.order || 0) - (b.order || 0);
            });
            for (let j = 0; j < items.length; ++j) {
                let pos;
                if (items.length === 1)
                    pos = "single";
                else if (j === 0)
                    pos = "first";
                else if (j === items.length - 1)
                    pos = "last";
                else
                    pos = "middle";
                out.push({
                    "group": k,
                    "schema": items[j],
                    "position": pos
                });
            }
        }
        return out;
    }

    contentItem: ColumnLayout {
        spacing: 0

        MD.DialogHeader {
            Layout.fillWidth: true
            title: "Configure " + root.pluginName
        }

        MD.Text {
            // 3 = QAsyncResult::Status::Error.
            visible: setQuery.status === 3
            Layout.fillWidth: true
            Layout.leftMargin: 24
            Layout.rightMargin: 24
            text: setQuery.error
            color: MD.Token.color.error
            typescale: MD.Token.typescale.body_small
            wrapMode: Text.WordWrap
        }

        MD.VerticalListView {
            id: settingsList
            Layout.fillWidth: true
            Layout.fillHeight: true
            Layout.leftMargin: 24
            Layout.rightMargin: 24
            Layout.topMargin: 8
            clip: true
            model: root.flatSchemas
            spacing: 2

            section.property: "group"
            section.delegate: MD.Text {
                required property string section
                width: ListView.view ? ListView.view.width : 0
                text: section
                typescale: MD.Token.typescale.title_small
                color: MD.Token.color.on_surface_variant
                topPadding: 16
                bottomPadding: 6
                leftPadding: 4
            }

            delegate: Rectangle {
                id: itemRect
                required property var modelData
                width: ListView.view ? ListView.view.width : 0
                implicitHeight: fieldCol.implicitHeight + 16
                color: MD.Token.color.surface_container

                readonly property real radiusBig: 16
                readonly property bool roundTop: modelData.position === "single" || modelData.position === "first"
                readonly property bool roundBottom: modelData.position === "single" || modelData.position === "last"

                topLeftRadius: roundTop ? radiusBig : 0
                topRightRadius: roundTop ? radiusBig : 0
                bottomLeftRadius: roundBottom ? radiusBig : 0
                bottomRightRadius: roundBottom ? radiusBig : 0

                ColumnLayout {
                    id: fieldCol
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    anchors.leftMargin: 16
                    anchors.rightMargin: 16

                    SettingField {
                        Layout.fillWidth: true
                        schema: itemRect.modelData.schema
                        value: root.valueFor(itemRect.modelData.schema.key)
                        onCommitted: function (key, newValue) {
                            const next = Object.assign({}, root.pendingValues);
                            next[key] = newValue;
                            root.pendingValues = next;
                        }
                    }
                }
            }
        }

        MD.DialogButtonBox {
            Layout.fillWidth: true

            MD.Button {
                text: "Reset"
                mdState.type: MD.Enum.BtText
                enabled: root.isDirty
                T.DialogButtonBox.buttonRole: T.DialogButtonBox.ResetRole
                onClicked: root.reset()
            }
            MD.Button {
                text: "Apply"
                mdState.type: MD.Enum.BtText
                enabled: root.isDirty
                T.DialogButtonBox.buttonRole: T.DialogButtonBox.ApplyRole
                onClicked: root.apply()
            }
        }
    }
}
