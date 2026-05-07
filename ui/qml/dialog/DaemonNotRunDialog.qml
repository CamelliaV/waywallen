pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import QtQuick.Templates as T

import Qcm.Material as MD
import waywallen.ui as W

MD.Popup {
    id: root

    // Dialog is the single anchor for "daemon is not usable yet". It
    // wins on either of two orthogonal conditions:
    //   - DBus says the daemon process is missing / version-mismatched
    //   - DBus is connected but the daemon's `phase` is still Starting
    //     (WS not bound yet, or core services still booting)
    readonly property bool dbusConnected: W.DaemonDBusClient.status === W.DaemonDBusClient.Connected
    readonly property bool daemonStarting: dbusConnected && W.Notify.daemonPhase !== W.Notify.DaemonPhase.Ready

    visible: !dbusConnected || daemonStarting
    closePolicy: T.Popup.NoAutoClose
    dim: true
    modal: true
    parent: T.Overlay.overlay
    x: Math.round((parent.width - width) / 2)
    y: Math.round((parent.height - height) / 2)
    bottomPadding: 24

    function refreshProcs() {
        m_proc_model.clear();
        const list = W.DaemonDBusClient.listWaywallenProcesses();
        for (let i = 0; i < list.length; ++i) {
            m_proc_model.append(list[i]);
        }
    }

    onVisibleChanged: if (visible && !daemonStarting)
        refreshProcs()

    Connections {
        target: W.DaemonDBusClient
        function onStatusChanged() {
            if (root.visible && !root.daemonStarting)
                root.refreshProcs();
        }
    }

    contentItem: ColumnLayout {
        spacing: 16

        MD.DialogHeader {
            Layout.fillWidth: true
            title: {
                if (root.daemonStarting)
                    return "Starting…";
                switch (W.DaemonDBusClient.status) {
                case W.DaemonDBusClient.Disconnected:
                    return "Daemon not running";
                case W.DaemonDBusClient.VersionMissing:
                    return "Daemon too old";
                case W.DaemonDBusClient.VersionMismatch:
                    return "Daemon version mismatch";
                }
                return "";
            }
        }

        MD.Label {
            Layout.fillWidth: true
            Layout.leftMargin: 24
            Layout.rightMargin: 24
            wrapMode: Text.WordWrap
            text: {
                if (root.daemonStarting)
                    return "waywallen is initializing core services. This usually takes a few seconds.";
                switch (W.DaemonDBusClient.status) {
                case W.DaemonDBusClient.Disconnected:
                    return "The waywallen daemon is not on the session bus.";
                case W.DaemonDBusClient.VersionMissing:
                    return `Daemon is online but does not advertise a version.`;
                case W.DaemonDBusClient.VersionMismatch:
                    return `Daemon version ${W.DaemonDBusClient.daemonVersion} + is incompatible.`;
                }
                return "";
            }
        }

        MD.LinearIndicator {
            Layout.fillWidth: true
            Layout.leftMargin: 24
            Layout.rightMargin: 24
            visible: root.daemonStarting
        }

        MD.VerticalListView {
            id: m_proc_list
            Layout.fillWidth: true
            Layout.leftMargin: 16
            Layout.rightMargin: 16
            Layout.preferredWidth: 300
            implicitHeight: Math.min(contentHeight, 200)
            visible: !root.daemonStarting && m_proc_model.count > 0
            clip: true
            spacing: 4
            model: ListModel {
                id: m_proc_model
            }

            delegate: MD.ListItem {
                id: m_item
                width: ListView.view ? ListView.view.contentWidth : 0
                spacing: 8
                required property int pid
                required property string cmdline

                text: cmdline
                elide: Text.ElideLeft
                background: MD.Rectangle {
                    color: root.MD.MProp.color.surface
                    corners: MD.Util.listCorners(index, count, 16)
                }

                leader: MD.Text {
                    text: m_item.pid
                }

                trailing: MD.BusyButton {
                    text: "Kill"
                    mdState.type: MD.Enum.BtText
                    busy: m_t.running
                    onClicked: {
                        W.DaemonDBusClient.killProcess(parent.pid);
                        m_t.start();
                    }
                }

                Timer {
                    id: m_t
                    interval: 2000
                    onTriggered: root.refreshProcs()
                }
            }
        }

        MD.DialogButtonBox {
            Layout.fillWidth: true

            MD.Button {
                text: "Exit"
                mdState.type: MD.Enum.BtText
                T.DialogButtonBox.buttonRole: T.DialogButtonBox.RejectRole
                onClicked: Qt.quit()
            }
            MD.Button {
                text: "Restart"
                mdState.type: MD.Enum.BtText
                T.DialogButtonBox.buttonRole: T.DialogButtonBox.AcceptRole
                visible: !root.daemonStarting
                onClicked: W.DaemonDBusClient.launchDaemon()
            }
        }
    }
}
